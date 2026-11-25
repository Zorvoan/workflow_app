//! Provider for any service exposing an OpenAI-style `/chat/completions`
//! endpoint (other vendors, or a local model server such as Ollama / LM Studio).
//! This exists so EduTask is not tied to one vendor; it is not used for Claude.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{AiProvider, Block, ChatRequest, ChatResponse, Role, StopReason, Usage};
use crate::error::{AppError, AppResult};

pub struct OpenAiCompatibleProvider {
    http: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    model: String,
}

impl OpenAiCompatibleProvider {
    pub fn new(api_key: Option<String>, base_url: String, model: String) -> AppResult<Self> {
        if !base_url.starts_with("http") {
            return Err(AppError::Ai("OpenAI-compatible provider needs a base URL (e.g. http://localhost:11434/v1)".into()));
        }
        let http = reqwest::Client::builder().timeout(Duration::from_secs(600)).build()?;
        Ok(Self { http, api_key, base_url: base_url.trim_end_matches('/').to_string(), model })
    }
}

pub fn build_messages(req: &ChatRequest) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": req.system})];
    for m in &req.messages {
        match m.role {
            Role::Assistant => {
                let text: String = m.content.iter().filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None }).collect();
                let calls: Vec<Value> = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        Block::ToolUse { id, name, input } => {
                            Some(json!({"id": id, "type": "function", "function": {"name": name, "arguments": input.to_string()}}))
                        }
                        _ => None,
                    })
                    .collect();
                let mut msg = json!({"role": "assistant", "content": text});
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                out.push(msg);
            }
            Role::User => {
                let mut parts = Vec::new();
                for b in &m.content {
                    match b {
                        Block::ToolResult { tool_use_id, content, is_error } => {
                            let c = if *is_error { format!("ERROR: {content}") } else { content.clone() };
                            out.push(json!({"role": "tool", "tool_call_id": tool_use_id, "content": c}));
                        }
                        Block::Text(t) => parts.push(json!({"type": "text", "text": t})),
                        Block::Image { media_type, data_b64 } => {
                            parts.push(json!({"type": "image_url", "image_url": {"url": format!("data:{media_type};base64,{data_b64}")}}))
                        }
                        Block::Pdf { title, .. } => parts.push(json!({"type": "text", "text": format!("[PDF '{title}' attached, but this provider cannot read PDFs]")})),
                        Block::ToolUse { .. } | Block::Opaque(_) => {}
                    }
                }
                if !parts.is_empty() {
                    out.push(json!({"role": "user", "content": parts}));
                }
            }
        }
    }
    out
}

pub fn parse_response(v: &Value) -> AppResult<ChatResponse> {
    let choice = v.pointer("/choices/0").ok_or_else(|| AppError::Ai("response has no choices".into()))?;
    let msg = &choice["message"];
    let mut content = Vec::new();
    if let Some(t) = msg.get("content").and_then(Value::as_str) {
        if !t.is_empty() {
            content.push(Block::Text(t.to_string()));
        }
    }
    for call in msg.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
        let args = call.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("{}");
        content.push(Block::ToolUse {
            id: call.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            name: call.pointer("/function/name").and_then(Value::as_str).unwrap_or("").to_string(),
            input: serde_json::from_str(args).unwrap_or(Value::Null),
        });
    }
    let stop = match choice.get("finish_reason").and_then(Value::as_str) {
        Some("stop") => StopReason::EndTurn,
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::Refusal,
        _ if content.iter().any(|b| matches!(b, Block::ToolUse { .. })) => StopReason::ToolUse,
        _ => StopReason::Other,
    };
    let n = |p: &str| v.pointer(p).and_then(Value::as_i64).unwrap_or(0);
    Ok(ChatResponse {
        content,
        stop,
        usage: Usage { input_tokens: n("/usage/prompt_tokens"), output_tokens: n("/usage/completion_tokens"), ..Default::default() },
        model: v.get("model").and_then(Value::as_str).unwrap_or("").to_string(),
    })
}

#[async_trait]
impl AiProvider for OpenAiCompatibleProvider {
    fn id(&self) -> &'static str {
        "openai_compatible"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn supports_documents(&self) -> bool {
        false
    }

    async fn complete(&self, req: &ChatRequest) -> AppResult<ChatResponse> {
        let mut body = json!({"model": self.model, "max_tokens": req.max_tokens, "messages": build_messages(req)});
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}}))
                    .collect(),
            );
        }
        if let Some(schema) = &req.json_schema {
            body["response_format"] = json!({"type": "json_schema", "json_schema": {"name": "result", "schema": schema, "strict": true}});
        }
        let mut rb = self.http.post(format!("{}/chat/completions", self.base_url)).json(&body);
        if let Some(k) = &self.api_key {
            rb = rb.bearer_auth(k);
        }
        let resp = rb.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(AppError::Ai(format!("AI provider error ({status}): {}", crate::util::truncate_middle(&text, 500))));
        }
        parse_response(&serde_json::from_str(&text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::Message;

    #[test]
    fn tool_round_trip_mapping() {
        let req = ChatRequest {
            system: "s".into(),
            messages: vec![
                Message::user_text("go"),
                Message { role: Role::Assistant, content: vec![Block::ToolUse { id: "c1".into(), name: "list_files".into(), input: json!({"path": ""}) }] },
                Message { role: Role::User, content: vec![Block::ToolResult { tool_use_id: "c1".into(), content: "a.txt".into(), is_error: false }] },
            ],
            tools: vec![],
            max_tokens: 10,
            json_schema: None,
        };
        let m = build_messages(&req);
        assert_eq!(m[2]["tool_calls"][0]["function"]["name"], "list_files");
        assert_eq!(m[3]["role"], "tool");
        let r = parse_response(&json!({"choices":[{"finish_reason":"tool_calls","message":{"content":null,"tool_calls":[{"id":"x","function":{"name":"f","arguments":"{\"a\":1}"}}]}}]})).unwrap();
        assert_eq!(r.stop, StopReason::ToolUse);
        assert_eq!(r.tool_uses()[0].2["a"], 1);
    }
}
