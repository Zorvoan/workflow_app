//! Tool-using generation loop. The model edits files inside the project folder
//! and may run allowlisted commands; the engine (not the model) decides whether
//! the build succeeded, by re-running the commands itself afterwards.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use super::plan::commands_schema;
use super::sandbox::{self, CommandSpec, Policy};
use super::{Level, Reporter, Stage, Workspace};
use crate::ai::{AiProvider, Block, ChatRequest, Message, Role, StopReason, ToolDef};
use crate::error::{AppError, AppResult};
use crate::util::{resolve_within, truncate_middle};

const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Default, Clone)]
pub struct AgentOutcome {
    pub summary: String,
    pub build_commands: Option<Vec<CommandSpec>>,
    pub test_commands: Option<Vec<CommandSpec>>,
    pub run_instructions: String,
    pub files_written: usize,
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

pub fn tools() -> Vec<ToolDef> {
    let path = json!({"type": "string", "description": "Path relative to the project root. Prefix with materials/ to read assignment materials (read-only)."});
    vec![
        ToolDef {
            name: "list_files".into(),
            description: "List files under a directory of the project (or materials/). Use \"\" for the project root.".into(),
            input_schema: obj(json!({"path": path}), &["path"]),
        },
        ToolDef {
            name: "read_file".into(),
            description: "Read a UTF-8 text file from the project or from materials/.".into(),
            input_schema: obj(json!({"path": path}), &["path"]),
        },
        ToolDef {
            name: "write_file".into(),
            description: "Create or overwrite a file in the project with the complete content. Parent folders are created automatically. Cannot write to materials/.".into(),
            input_schema: obj(json!({"path": {"type": "string"}, "content": {"type": "string"}}), &["path", "content"]),
        },
        ToolDef {
            name: "delete_file".into(),
            description: "Delete a file from the project.".into(),
            input_schema: obj(json!({"path": {"type": "string"}}), &["path"]),
        },
        ToolDef {
            name: "run_command".into(),
            description: "Run one build/test command in the project root, without a shell (no pipes, redirection or &&). Only compilers, interpreters and package managers are allowed. Returns exit code and output.".into(),
            input_schema: obj(json!({"program": {"type": "string"}, "args": {"type": "array", "items": {"type": "string"}}}), &["program", "args"]),
        },
        ToolDef {
            name: "finish".into(),
            description: "Call once when the project is complete. Give the final build and test commands; EduTask will run them itself to verify the result.".into(),
            input_schema: obj(
                json!({
                    "summary": {"type": "string", "description": "What was built, for the student"},
                    "build_commands": commands_schema(),
                    "test_commands": commands_schema(),
                    "run_instructions": {"type": "string", "description": "How to run/open the result"}
                }),
                &["summary", "build_commands", "test_commands", "run_instructions"],
            ),
        },
    ]
}

fn list_dir(root: &std::path::Path, rel: &str) -> AppResult<String> {
    let dir = resolve_within(root, rel)?;
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(&dir).hidden(false).git_ignore(false).max_depth(Some(6)).build();
    for e in walker.flatten() {
        if e.depth() == 0 {
            continue;
        }
        let p = e.path();
        let rel = p.strip_prefix(&dir).unwrap_or(p).to_string_lossy().replace('\\', "/");
        if ["node_modules", "target", ".git", "bin", "obj", "__pycache__", ".venv", "build", "dist"].iter().any(|skip| rel == *skip || rel.starts_with(&format!("{skip}/"))) {
            continue;
        }
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(if p.is_dir() { format!("{rel}/") } else { format!("{rel} ({size} B)") });
        if out.len() >= 500 {
            out.push("… (truncated)".into());
            break;
        }
    }
    Ok(if out.is_empty() { "(empty)".into() } else { out.join("\n") })
}

/// Execute one tool call. Errors become `is_error` tool results so the model can adapt.
async fn exec_tool(name: &str, input: &Value, ws: &Workspace, policy: &Policy, cancel: &Arc<AtomicBool>, reporter: &dyn Reporter, outcome: &mut AgentOutcome) -> Result<String, String> {
    let s = |k: &str| input.get(k).and_then(Value::as_str).map(String::from).ok_or_else(|| format!("missing '{k}'"));
    let split = |p: &str| -> (bool, String) {
        let p = p.replace('\\', "/");
        match p.strip_prefix("materials/").or_else(|| (p == "materials").then_some("")) {
            Some(rest) => (true, rest.to_string()),
            None => (false, p),
        }
    };
    match name {
        "list_files" => {
            let (mat, rel) = split(&s("path")?);
            list_dir(if mat { &ws.materials_dir } else { &ws.project_dir }, &rel).map_err(|e| e.to_string())
        }
        "read_file" => {
            let (mat, rel) = split(&s("path")?);
            let p = resolve_within(if mat { &ws.materials_dir } else { &ws.project_dir }, &rel).map_err(|e| e.to_string())?;
            let bytes = std::fs::read(&p).map_err(|e| format!("cannot read: {e}"))?;
            Ok(truncate_middle(&String::from_utf8_lossy(&bytes), 100_000))
        }
        "write_file" => {
            let path = s("path")?;
            let content = s("content")?;
            if split(&path).0 {
                return Err("materials/ is read-only".into());
            }
            if content.len() > MAX_FILE_BYTES {
                return Err("file too large (2 MB limit)".into());
            }
            let p = resolve_within(&ws.project_dir, &path).map_err(|e| e.to_string())?;
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&p, content.as_bytes()).map_err(|e| e.to_string())?;
            outcome.files_written += 1;
            reporter.report(Stage::Generate, Level::Info, &format!("Wrote {path}"));
            Ok(format!("wrote {} bytes to {path}", content.len()))
        }
        "delete_file" => {
            let path = s("path")?;
            let p = resolve_within(&ws.project_dir, &path).map_err(|e| e.to_string())?;
            if p == ws.project_dir {
                return Err("cannot delete the project root".into());
            }
            std::fs::remove_file(&p).map_err(|e| e.to_string())?;
            reporter.report(Stage::Generate, Level::Info, &format!("Deleted {path}"));
            Ok(format!("deleted {path}"))
        }
        "run_command" => {
            let cmd: CommandSpec = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;
            sandbox::classify(&cmd, policy).map_err(|reason| format!("command refused: {reason}"))?;
            reporter.report(Stage::Generate, Level::Info, &format!("$ {}", cmd.display()));
            let r = sandbox::run(&cmd, &ws.project_dir, &ws.tmp_dir, policy, cancel).await;
            let out = r.combined_output(20_000);
            if !out.trim().is_empty() {
                reporter.report(Stage::Generate, Level::Output, &truncate_middle(&out, 4000));
            }
            Ok(format!("exit_code: {:?}{}\n{}", r.exit_code, if r.timed_out { " (TIMED OUT)" } else { "" }, out))
        }
        "finish" => {
            outcome.summary = s("summary")?;
            outcome.run_instructions = s("run_instructions").unwrap_or_default();
            outcome.build_commands = serde_json::from_value(input.get("build_commands").cloned().unwrap_or(json!([]))).ok();
            outcome.test_commands = serde_json::from_value(input.get("test_commands").cloned().unwrap_or(json!([]))).ok();
            Ok("Recorded. EduTask will now verify the build.".into())
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

pub struct AgentRun<'a> {
    pub ai: &'a dyn AiProvider,
    pub ws: &'a Workspace,
    pub policy: &'a Policy,
    pub cancel: &'a Arc<AtomicBool>,
    pub reporter: &'a dyn Reporter,
    pub system: String,
    pub max_turns: u32,
}

impl AgentRun<'_> {
    pub async fn run(&self, initial: Vec<Block>) -> AppResult<AgentOutcome> {
        let mut messages = vec![Message { role: Role::User, content: initial }];
        let mut outcome = AgentOutcome::default();
        let mut nudged = false;
        for turn in 0..self.max_turns {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(AppError::Internal("cancelled".into()));
            }
            let req = ChatRequest { system: self.system.clone(), messages: messages.clone(), tools: tools(), max_tokens: 16000, json_schema: None };
            let resp = self.ai.complete(&req).await?;
            self.reporter.usage(self.ai.id(), self.ai.model(), &resp.usage);
            let text = resp.text();
            if !text.trim().is_empty() {
                self.reporter.report(Stage::Generate, Level::Info, &truncate_middle(text.trim(), 1500));
            }
            if resp.stop == StopReason::Refusal {
                return Err(AppError::Ai("The AI model declined this request (refusal). Review the assignment and try again or switch model.".into()));
            }
            let calls = resp.tool_uses();
            messages.push(Message { role: Role::Assistant, content: resp.content.clone() });

            if calls.is_empty() {
                if outcome.summary.is_empty() && !nudged {
                    nudged = true;
                    messages.push(Message::user_text("Continue. When the project is complete, call the finish tool."));
                    continue;
                }
                if outcome.summary.is_empty() {
                    outcome.summary = text;
                }
                return Ok(outcome);
            }

            let mut results = Vec::new();
            let mut finished = false;
            for (id, name, input) in calls {
                let r = if resp.stop == StopReason::MaxTokens {
                    // A truncated tool input may parse as a valid but incomplete object: never run it.
                    Err("Not executed: your response hit the output limit and this call may be truncated. Write large files in smaller parts.".into())
                } else {
                    exec_tool(&name, &input, self.ws, self.policy, self.cancel, self.reporter, &mut outcome).await
                };
                if name == "finish" && r.is_ok() {
                    finished = true;
                }
                let (content, is_error) = match r {
                    Ok(c) => (c, false),
                    Err(e) => (e, true),
                };
                results.push(Block::ToolResult { tool_use_id: id, content, is_error });
            }
            // All results go back in one user message.
            messages.push(Message { role: Role::User, content: results });
            if finished {
                return Ok(outcome);
            }
            if turn + 1 == self.max_turns {
                self.reporter.report(Stage::Generate, Level::Warn, "Reached the maximum number of AI turns for this pass");
            }
        }
        Ok(outcome)
    }
}
