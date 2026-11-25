//! BUILD ASSIGNMENT pipeline:
//! prepare → analyze (structured plan) → generate (tool loop) → install deps →
//! build → test → fix loop → verify required files → done.
//!
//! The success decision is made here, from exit codes of commands EduTask ran
//! itself. The model's own claims ("it builds!") are never trusted.

pub mod agent;
pub mod materials;
pub mod plan;
pub mod sandbox;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;

use crate::ai::{AiProvider, Block, ChatRequest, Message, Role, StopReason, Usage};
use crate::db::models::Assignment;
use crate::error::{AppError, AppResult};
use crate::github::gitignore;
use plan::Plan;
use sandbox::{CommandResult, CommandSpec, Policy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Prepare,
    Analyze,
    Generate,
    Dependencies,
    Build,
    Test,
    Fix,
    Verify,
    Publish,
    Done,
}

impl Stage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stage::Prepare => "prepare",
            Stage::Analyze => "analyze",
            Stage::Generate => "generate",
            Stage::Dependencies => "dependencies",
            Stage::Build => "build",
            Stage::Test => "test",
            Stage::Fix => "fix",
            Stage::Verify => "verify",
            Stage::Publish => "publish",
            Stage::Done => "done",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warn,
    Error,
    Output,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Success => "success",
            Level::Warn => "warn",
            Level::Error => "error",
            Level::Output => "output",
        }
    }
}

pub trait Reporter: Send + Sync {
    fn report(&self, stage: Stage, level: Level, message: &str);
    fn usage(&self, provider: &str, model: &str, usage: &Usage);
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub materials_dir: PathBuf,
    pub project_dir: PathBuf,
    pub tmp_dir: PathBuf,
}

impl Workspace {
    pub fn for_assignment(projects_dir: &Path, a: &Assignment) -> Self {
        let slug = crate::util::slugify(&format!("{} {}", a.subject.as_deref().unwrap_or(""), a.title), 60);
        let short_id = crate::util::slugify(&a.external_id, 12);
        let root = match &a.workspace_path {
            Some(p) => PathBuf::from(p),
            None => projects_dir.join(format!("{}-{}", if slug.is_empty() { "assignment" } else { &slug }, short_id)),
        };
        Self { materials_dir: root.join("materials"), project_dir: root.join("project"), tmp_dir: root.join(".edutask").join("tmp"), root }
    }

    pub fn create(&self) -> AppResult<()> {
        for d in [&self.materials_dir, &self.project_dir, &self.tmp_dir] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildResult {
    pub succeeded: bool,
    pub plan: Option<Plan>,
    pub summary: String,
    pub run_instructions: String,
    pub verified_commands: Vec<String>,
    pub error: Option<String>,
}

pub struct BuildInput<'a> {
    pub assignment: &'a Assignment,
    pub ws: &'a Workspace,
    pub ai: &'a dyn AiProvider,
    pub policy: Policy,
    pub enabled_languages: Vec<String>,
    pub max_fix_iterations: u32,
    pub max_agent_turns: u32,
    pub cancel: Arc<AtomicBool>,
    pub reporter: &'a dyn Reporter,
}

pub fn assignment_markdown(a: &Assignment) -> String {
    format!(
        "# {}\n\n- Subject: {}\n- Teacher: {}\n- Deadline: {}\n\n## Assignment\n\n{}\n",
        a.title,
        a.subject.as_deref().unwrap_or("unknown"),
        a.teacher.as_deref().unwrap_or("unknown"),
        a.due_at.as_deref().unwrap_or("none"),
        if a.description_text.trim().is_empty() { "(no description text; see attachments)" } else { a.description_text.trim() }
    )
}

fn toolchain_summary() -> String {
    sandbox::detect_toolchains()
        .into_iter()
        .map(|t| match (&t.path, &t.note) {
            (Some(_), _) => format!("- {}: available", t.name),
            (None, Some(n)) => format!("- {}: NOT available ({n})", t.name),
            (None, None) => format!("- {}: not installed", t.name),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const ANALYZE_SYSTEM: &str = "You analyze school assignments for a student's desktop tool and produce an implementation plan as JSON. \
Read the assignment text and every attached material carefully; the assignment is often in Slovak, Czech or another language — keep the \
student's language for user-facing text in the deliverable, but write the plan fields in English. Choose a language/technology the assignment \
asks for; if it does not specify one, choose the most fitting one that is installed on the computer. Commands are executed without a shell \
in the project root (a fresh, empty folder unless files already exist), so give each as a program name plus an argument list; never use \
pipes, redirection, &&, absolute paths or '..'. Build commands must fail (non-zero exit) when the project is broken. Test commands should \
exercise the program, e.g. run it with sample input, run a unit test suite, or for Python use `python -m py_compile`. For documents or \
presentations, plan to produce the file with an installed tool (pandoc, or python with python-docx/python-pptx when available), or as \
Markdown/HTML if no converter exists, and say so in ambiguities. Always include README.md in required_files.";

const GENERATE_SYSTEM: &str = "You are implementing a school assignment inside an isolated project folder using the provided tools. \
Write complete, working, well-structured code or content — no placeholders or TODOs. Follow the plan, but fix it if the materials show it \
is wrong. Keep the student's language for program output and documents when the assignment uses it. Create a README.md describing the \
assignment, how to build and run the result, and the project structure. Do not create files containing secrets, API keys or personal \
data. Use run_command to compile and test as you go; only allowlisted build tools can run, without a shell. Do not try to work around a \
refused command. Never claim the build succeeded unless a command you ran exited with code 0 — EduTask re-runs the final build and test \
commands itself. When done, call finish.";

async fn analyze(input: &BuildInput<'_>, assignment_md: &str, mats: &materials::Materials) -> AppResult<Plan> {
    let mut content = vec![Block::Text(format!(
        "{assignment_md}\n\n## Installed toolchains\n{}\n\n## Languages the student allows\n{}\n\n## Attached materials\n{}\n",
        toolchain_summary(),
        input.enabled_languages.join(", "),
        if mats.inventory.is_empty() { "(none)".to_string() } else { mats.inventory.join("\n") }
    ))];
    content.extend(mats.blocks.iter().cloned());
    let existing = agent_list(&input.ws.project_dir);
    content.push(Block::Text(format!("## Existing project files\n{existing}\n\nProduce the plan.")));
    let req = ChatRequest {
        system: ANALYZE_SYSTEM.into(),
        messages: vec![Message { role: Role::User, content }],
        tools: vec![],
        max_tokens: 16000,
        json_schema: Some(plan::schema()),
    };
    let resp = input.ai.complete(&req).await?;
    input.reporter.usage(input.ai.id(), input.ai.model(), &resp.usage);
    match resp.stop {
        StopReason::Refusal => return Err(AppError::Ai("The AI model declined to analyze this assignment (refusal).".into())),
        StopReason::MaxTokens => return Err(AppError::Ai("The analysis was cut off (max tokens). Try again.".into())),
        _ => {}
    }
    let text = resp.text();
    let json = text.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    serde_json::from_str::<Plan>(json).map_err(|e| AppError::Ai(format!("AI returned an invalid plan: {e}")))
}

fn agent_list(dir: &Path) -> String {
    let mut v: Vec<String> = ignore::WalkBuilder::new(dir)
        .hidden(false)
        .max_depth(Some(4))
        .build()
        .flatten()
        .filter(|e| e.depth() > 0 && e.path().is_file())
        .map(|e| e.path().strip_prefix(dir).unwrap_or(e.path()).to_string_lossy().replace('\\', "/"))
        .take(200)
        .collect();
    v.sort();
    if v.is_empty() { "(empty folder)".into() } else { v.join("\n") }
}

/// Validate commands up front; refused ones are reported and dropped.
fn admissible(cmds: &[CommandSpec], policy: &Policy, stage: Stage, reporter: &dyn Reporter) -> Vec<CommandSpec> {
    cmds.iter()
        .filter(|c| match sandbox::classify(c, policy) {
            Ok(_) => true,
            Err(reason) => {
                reporter.report(stage, Level::Warn, &format!("Skipped `{}`: {reason}", c.display()));
                false
            }
        })
        .cloned()
        .collect()
}

async fn run_all(cmds: &[CommandSpec], stage: Stage, input: &BuildInput<'_>) -> Result<Vec<String>, CommandResult> {
    let mut ok = Vec::new();
    for c in cmds {
        if input.cancel.load(Ordering::Relaxed) {
            break;
        }
        input.reporter.report(stage, Level::Info, &format!("$ {}", c.display()));
        let r = sandbox::run(c, &input.ws.project_dir, &input.ws.tmp_dir, &input.policy, &input.cancel).await;
        let out = r.combined_output(6000);
        if !out.trim().is_empty() {
            input.reporter.report(stage, Level::Output, &out);
        }
        if !r.success() {
            let why = if r.timed_out { "timed out".to_string() } else if r.cancelled { "cancelled".into() } else { format!("exit code {:?}", r.exit_code) };
            input.reporter.report(stage, Level::Error, &format!("`{}` failed ({why})", c.display()));
            return Err(r);
        }
        input.reporter.report(stage, Level::Success, &format!("`{}` exited with code 0 in {} ms", c.display(), r.duration_ms));
        ok.push(c.display());
    }
    Ok(ok)
}

fn ensure_project_files(ws: &Workspace, a: &Assignment, plan: &Plan, summary: &str, run_instructions: &str, extra_gitignore: &str, reporter: &dyn Reporter) -> AppResult<()> {
    let gi = ws.project_dir.join(".gitignore");
    let content = gitignore::generate(&plan.language, extra_gitignore);
    let merged = match std::fs::read_to_string(&gi) {
        Ok(existing) => gitignore::merge(&existing, &content),
        Err(_) => content,
    };
    std::fs::write(&gi, merged)?;
    let readme = ws.project_dir.join("README.md");
    if !readme.exists() {
        reporter.report(Stage::Verify, Level::Info, "README.md was missing; generated one");
        let build = plan.build_commands.iter().map(|c| format!("    {}", c.display())).collect::<Vec<_>>().join("\n");
        std::fs::write(
            &readme,
            format!(
                "# {}\n\n{}\n\n## Assignment\n\n- Subject: {}\n- Teacher: {}\n- Deadline: {}\n\n{}\n\n## Build\n\n{}\n\n## Run\n\n{}\n",
                a.title,
                summary,
                a.subject.as_deref().unwrap_or("-"),
                a.teacher.as_deref().unwrap_or("-"),
                a.due_at.as_deref().unwrap_or("-"),
                a.description_text.trim(),
                if build.is_empty() { "No build step.".into() } else { build },
                if run_instructions.is_empty() { &plan.expected_output } else { run_instructions },
            ),
        )?;
    }
    Ok(())
}

pub async fn build(input: BuildInput<'_>, extra_gitignore: &str) -> BuildResult {
    let r = input.reporter;
    let mut result = BuildResult { succeeded: false, plan: None, summary: String::new(), run_instructions: String::new(), verified_commands: vec![], error: None };
    let fail = |mut res: BuildResult, msg: String| {
        r.report(Stage::Done, Level::Error, &msg);
        res.error = Some(msg);
        res
    };

    // 1. Prepare
    r.report(Stage::Prepare, Level::Info, "Reading assignment…");
    if let Err(e) = input.ws.create() {
        return fail(result, format!("Could not create workspace: {e}"));
    }
    let md = assignment_markdown(input.assignment);
    let _ = std::fs::write(input.ws.materials_dir.join("assignment.md"), &md);
    let mats = materials::collect(&input.ws.materials_dir, input.ai.supports_documents());
    r.report(Stage::Prepare, Level::Info, &format!("{} material file(s) prepared", mats.inventory.len()));

    // 2. Analyze
    r.report(Stage::Analyze, Level::Info, "Analyzing requirements…");
    let plan = match analyze(&input, &md, &mats).await {
        Ok(p) => p,
        Err(e) => return fail(result, format!("Analysis failed: {e}")),
    };
    r.report(Stage::Analyze, Level::Success, &format!("Plan: {} ({}, {})", plan.summary, plan.kind, plan.technology));
    for a in &plan.ambiguities {
        r.report(Stage::Analyze, Level::Warn, &format!("Assumption: {a}"));
    }
    result.plan = Some(plan.clone());
    if input.cancel.load(Ordering::Relaxed) {
        return fail(result, "Cancelled".into());
    }

    // 3. Generate
    r.report(Stage::Generate, Level::Info, "Creating project and writing source files…");
    let plan_json = serde_json::to_string_pretty(&plan).unwrap_or_default();
    let mut initial = vec![Block::Text(format!(
        "{md}\n\n## Implementation plan\n```json\n{plan_json}\n```\n\n## Installed toolchains\n{}\n\n## Existing project files\n{}\n\n## Materials\n{}\n\nImplement the assignment now.",
        toolchain_summary(),
        agent_list(&input.ws.project_dir),
        mats.inventory.join("\n")
    ))];
    initial.extend(mats.blocks.iter().cloned());
    let runner = agent::AgentRun {
        ai: input.ai,
        ws: input.ws,
        policy: &input.policy,
        cancel: &input.cancel,
        reporter: r,
        system: GENERATE_SYSTEM.into(),
        max_turns: input.max_agent_turns,
    };
    let mut outcome = match runner.run(initial).await {
        Ok(o) => o,
        Err(e) => return fail(result, format!("Generation failed: {e}")),
    };
    r.report(Stage::Generate, Level::Success, &format!("{} file write(s)", outcome.files_written));

    let mut build_cmds = outcome.build_commands.clone().filter(|v| !v.is_empty()).unwrap_or_else(|| plan.build_commands.clone());
    let mut test_cmds = outcome.test_commands.clone().filter(|v| !v.is_empty()).unwrap_or_else(|| plan.test_commands.clone());

    // 4. Dependencies (once)
    let deps = admissible(&plan.dependency_commands, &input.policy, Stage::Dependencies, r);
    if !deps.is_empty() {
        r.report(Stage::Dependencies, Level::Info, "Installing dependencies…");
        if let Err(res) = run_all(&deps, Stage::Dependencies, &input).await {
            r.report(Stage::Dependencies, Level::Warn, &format!("Dependency step failed: {}", res.command));
        }
    }

    // 5–7. Build, test, fix loop
    let mut iteration = 0;
    loop {
        if input.cancel.load(Ordering::Relaxed) {
            return fail(result, "Cancelled".into());
        }
        let builds = admissible(&build_cmds, &input.policy, Stage::Build, r);
        let tests = admissible(&test_cmds, &input.policy, Stage::Test, r);
        let mut failure: Option<(Stage, CommandResult)> = None;
        let mut verified = Vec::new();

        if !builds.is_empty() {
            r.report(Stage::Build, Level::Info, "Building project…");
        }
        match run_all(&builds, Stage::Build, &input).await {
            Ok(v) => verified.extend(v),
            Err(res) => failure = Some((Stage::Build, res)),
        }
        if failure.is_none() {
            if !tests.is_empty() {
                r.report(Stage::Test, Level::Info, "Running tests…");
            }
            match run_all(&tests, Stage::Test, &input).await {
                Ok(v) => verified.extend(v),
                Err(res) => failure = Some((Stage::Test, res)),
            }
        }
        let missing: Vec<&String> = plan
            .required_files
            .iter()
            .filter(|f| f.as_str() != "README.md")
            .filter(|f| crate::util::resolve_within(&input.ws.project_dir, f).map(|p| !p.exists()).unwrap_or(true))
            .collect();

        if failure.is_none() && missing.is_empty() {
            result.verified_commands = verified;
            break;
        }
        if input.cancel.load(Ordering::Relaxed) {
            return fail(result, "Cancelled".into());
        }
        if iteration >= input.max_fix_iterations {
            let msg = match &failure {
                Some((stage, res)) => format!("{} failed after {iteration} fix attempt(s): `{}`", if *stage == Stage::Build { "Build" } else { "Tests" }, res.command),
                None => format!("Required files still missing: {}", missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")),
            };
            return fail(result, msg);
        }
        iteration += 1;
        r.report(Stage::Fix, Level::Info, &format!("Fixing problems (attempt {iteration}/{})…", input.max_fix_iterations));
        let problem = match &failure {
            Some((stage, res)) => format!(
                "The {} command `{}` failed ({}). Output:\n```\n{}\n```",
                stage.as_str(),
                res.command,
                if res.timed_out { "timeout".to_string() } else { format!("exit code {:?}", res.exit_code) },
                res.combined_output(15_000)
            ),
            None => format!("These required files are missing: {}", missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")),
        };
        let fix_prompt = format!(
            "{md}\n\n## Plan\n```json\n{plan_json}\n```\n\n## Current project files\n{}\n\n## Problem\n{problem}\n\nFix the project so that the build and test commands succeed. Read the relevant files first. Call finish when done.",
            agent_list(&input.ws.project_dir)
        );
        match runner.run(vec![Block::Text(fix_prompt)]).await {
            Ok(o) => {
                if let Some(b) = o.build_commands.clone().filter(|v| !v.is_empty()) {
                    build_cmds = b;
                }
                if let Some(t) = o.test_commands.clone().filter(|v| !v.is_empty()) {
                    test_cmds = t;
                }
                if !o.summary.is_empty() {
                    outcome.summary = o.summary;
                }
                if !o.run_instructions.is_empty() {
                    outcome.run_instructions = o.run_instructions;
                }
            }
            Err(e) => return fail(result, format!("Fix attempt failed: {e}")),
        }
    }

    // 8. Verify + project hygiene
    r.report(Stage::Verify, Level::Info, "Verifying project…");
    if let Err(e) = ensure_project_files(input.ws, input.assignment, &plan, &outcome.summary, &outcome.run_instructions, extra_gitignore, r) {
        return fail(result, format!("Could not finalize project files: {e}"));
    }
    if result.verified_commands.is_empty() {
        if matches!(plan.kind.as_str(), "programming" | "database") {
            return fail(
                result,
                "Project generated but NOT verified: no allowed build or test command could be run. Review it manually or install the required toolchain.".into(),
            );
        }
        r.report(Stage::Verify, Level::Warn, "No build or test commands apply to this assignment; verified that the required files exist.");
    }
    result.succeeded = true;
    result.summary = outcome.summary;
    result.run_instructions = outcome.run_instructions;
    r.report(Stage::Done, Level::Success, "Build successful.");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{ChatResponse, StopReason};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Mutex;

    /// Scripted provider: returns canned responses in order.
    struct Scripted(Mutex<Vec<ChatResponse>>);

    #[async_trait]
    impl AiProvider for Scripted {
        fn id(&self) -> &'static str { "test" }
        fn model(&self) -> &str { "scripted" }
        fn supports_documents(&self) -> bool { false }
        async fn complete(&self, _req: &ChatRequest) -> AppResult<ChatResponse> {
            let mut v = self.0.lock().unwrap();
            if v.is_empty() { return Err(AppError::Ai("script exhausted".into())) }
            Ok(v.remove(0))
        }
    }

    #[derive(Default)]
    struct Log(Mutex<Vec<(String, String, String)>>);
    impl Reporter for Log {
        fn report(&self, stage: Stage, level: Level, message: &str) {
            self.0.lock().unwrap().push((stage.as_str().into(), level.as_str().into(), message.into()));
        }
        fn usage(&self, _: &str, _: &str, _: &Usage) {}
    }

    fn resp(content: Vec<Block>, stop: StopReason) -> ChatResponse {
        ChatResponse { content, stop, usage: Usage::default(), model: "scripted".into() }
    }

    fn plan_json(build: serde_json::Value) -> String {
        json!({"kind":"programming","language":"python","technology":"Python 3","summary":"Adder","deliverables":["main.py"],
               "required_files":["main.py","README.md"],"expected_output":"5","dependency_commands":[],"build_commands":build,
               "test_commands":[],"submission_expects":"link","submission_notes":"","ambiguities":[],"steps":["write"]}).to_string()
    }

    fn write(id: &str, path: &str, content: &str) -> Block {
        Block::ToolUse { id: id.into(), name: "write_file".into(), input: json!({"path": path, "content": content}) }
    }
    fn finish(id: &str) -> Block {
        Block::ToolUse { id: id.into(), name: "finish".into(), input: json!({"summary":"done","build_commands":[],"test_commands":[],"run_instructions":"python main.py"}) }
    }

    fn assignment() -> Assignment {
        Assignment {
            id: "edupage:1".into(), platform: "edupage".into(), external_id: "1".into(), title: "Adder".into(), subject: Some("PRG".into()),
            teacher: None, teacher_ref: None, description_html: String::new(), description_text: "Add two numbers".into(), due_at: None,
            assigned_at: None, status: "new".into(), platform_done: false, changed_since_seen: false, removed: false, workspace_path: None,
            repo_url: None, repo_full_name: None, first_seen_at: String::new(), last_synced_at: String::new(), updated_at: String::new(),
        }
    }

    async fn run_with(script: Vec<ChatResponse>, max_fix: u32) -> (BuildResult, Workspace, Vec<(String, String, String)>) {
        let dir = tempfile::tempdir().unwrap().keep();
        let a = assignment();
        let ws = Workspace::for_assignment(&dir, &a);
        let ai = Scripted(Mutex::new(script));
        let log = Log::default();
        let input = BuildInput {
            assignment: &a, ws: &ws, ai: &ai,
            policy: Policy { allow_dependency_install: false, timeout: std::time::Duration::from_secs(20) },
            enabled_languages: vec!["python".into()], max_fix_iterations: max_fix, max_agent_turns: 5,
            cancel: Arc::new(AtomicBool::new(false)), reporter: &log,
        };
        let r = build(input, "").await;
        let events = log.0.lock().unwrap().clone();
        (r, ws, events)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn successful_build_requires_real_exit_code_zero() {
        let compile = json!([{"program":"python3","args":["-m","py_compile","main.py"]}]);
        let script = vec![
            resp(vec![Block::Text(plan_json(compile))], StopReason::EndTurn),
            resp(vec![write("t1", "main.py", "print(2+3)\n"), finish("t2")], StopReason::ToolUse),
        ];
        let (r, ws, _) = run_with(script, 0).await;
        assert!(r.succeeded, "{r:?}");
        assert_eq!(r.verified_commands, vec!["python3 -m py_compile main.py"]);
        assert!(ws.project_dir.join("README.md").exists(), "README generated when missing");
        assert!(std::fs::read_to_string(ws.project_dir.join(".gitignore")).unwrap().contains(".env"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failing_build_is_never_reported_as_success_even_if_ai_claims_it() {
        let compile = json!([{"program":"python3","args":["-m","py_compile","main.py"]}]);
        let script = vec![
            resp(vec![Block::Text(plan_json(compile))], StopReason::EndTurn),
            resp(vec![Block::Text("All done, build successful!".into()), write("t1", "main.py", "print(2+\n"), finish("t2")], StopReason::ToolUse),
        ];
        let (r, _, events) = run_with(script, 0).await;
        assert!(!r.succeeded);
        assert!(r.error.unwrap().contains("Build failed"));
        assert!(!events.iter().any(|e| e.2 == "Build successful."));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fix_loop_repairs_broken_build() {
        let compile = json!([{"program":"python3","args":["-m","py_compile","main.py"]}]);
        let script = vec![
            resp(vec![Block::Text(plan_json(compile))], StopReason::EndTurn),
            resp(vec![write("t1", "main.py", "print(2+\n"), finish("t2")], StopReason::ToolUse),
            resp(vec![write("t3", "main.py", "print(2+3)\n"), finish("t4")], StopReason::ToolUse),
        ];
        let (r, _, events) = run_with(script, 2).await;
        assert!(r.succeeded, "{r:?}");
        assert!(events.iter().any(|e| e.0 == "fix"));
    }

    #[tokio::test]
    async fn refused_commands_and_path_escapes_do_not_run() {
        let evil = json!([{"program":"powershell","args":["-c","Remove-Item C:\\ -Recurse"]}]);
        let script = vec![
            resp(vec![Block::Text(plan_json(evil))], StopReason::EndTurn),
            resp(vec![write("t1", "../outside.txt", "x"), write("t2", "main.py", "print(1)"), finish("t3")], StopReason::ToolUse),
        ];
        let (r, ws, events) = run_with(script, 0).await;
        assert!(!ws.root.join("outside.txt").exists());
        assert!(events.iter().any(|e| e.1 == "warn" && e.2.contains("powershell")));
        // A programming project with no runnable build/test command is not reported as built.
        assert!(!r.succeeded);
        assert!(r.error.unwrap().contains("NOT verified"));
    }

    #[tokio::test]
    async fn ai_refusal_fails_the_build() {
        let script = vec![resp(vec![], StopReason::Refusal)];
        let (r, _, _) = run_with(script, 0).await;
        assert!(!r.succeeded && r.error.unwrap().contains("refusal"));
    }
}
