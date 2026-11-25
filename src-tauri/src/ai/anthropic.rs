//! Anthropic Messages API (`POST /v1/messages`) over raw HTTP — there is no
//! official Anthropic SDK for Rust.
//!
//! * adaptive thinking + `output_config.effort` on models that support them
//! * thinking / fallback blocks are echoed back unchanged (`Block::Opaque`)
//! * `stop_reason: "refusal"` is surfaced, never treated as success
//! * server-side refusal fallbacks (`fallbacks: "default"`) on models that support them
//! * prompt caching on the system prompt and the growing conversation prefix

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{AiProvider, Block, ChatRequest, ChatResponse, Role, StopReason, Usage};
use crate::error::{AppError, AppResult};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

pub struct AnthropicProvider {
    http: reqwest::Client,
    api_key: String,
    model: String,
    effort: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String, effort: String) -> AppResult<Self> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(600)).connect_timeout(Duration::from_secs(20)).build()?;
        Ok(Self { http, api_key, model, effort })
    }
}

/// Models that accept adaptive thinking and `output_config.effort`.
fn supports_adaptive(model: &str) -> bool {
    !(model.starts_with("claude-3") || model.contains("haiku-4") || model.contains("sonnet-4-5") || model.contains("opus-4-5") || model.contains("opus-4-1"))
}

/// Models that accept the server-side refusal `fallbacks` parameter.
fn supports_fallbacks(model: &str) -> bool {
    matches!(model, "claude-fable-5-2" | "claude-fable-5-1" | "claude-opus-5-5" | "claude-opus-5" | "claude-sonnet-5-5")
}

pub fn block_to_json(b: &Block) -> Value {
    match b {
        Block::Text(t) => json!({"type": "text", "text": t}),
        Block::Image { media_type, data_b64 } => json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data_b64}}),
        Block::Pdf { data_b64, title } => json!({"type": "document", "title": title, "source": {"type": "base64", "media_type": "application/pdf", "data": data_b64}}),
        Block::ToolUse { id, name, input } => json!({"type": "tool_use", "id": id, "name": name, "input": input}),
        Block::ToolResult { tool_use_id, content, is_error } => json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error}),
        Block::Opaque(v) => v.clone(),
    }
}

pub fn build_body(model: &str, effort: &str, req: &ChatRequest) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                "content": m.content.iter().map(block_to_json).collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut body = json!({
        "model": model,
        "max_tokens": req.max_tokens,
        "system": [{"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}],
        "messages": messages,
        // Auto-caches the conversation prefix, which matters for the tool loop.
        "cache_control": {"type": "ephemeral"},
    });
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema, "strict": true}))
                .collect(),
        );
    }
    let mut output_config = serde_json::Map::new();
    if supports_adaptive(model) {
        body["thinking"] = json!({"type": "adaptive"});
        output_config.insert("effort".into(), json!(effort));
    }
    if let Some(schema) = &req.json_schema {
        output_config.insert("format".into(), json!({"type": "json_schema", "schema": schema}));
    }
    if !output_config.is_empty() {
        body["output_config"] = Value::Object(output_config);
    }
    if supports_fallbacks(model) {
        body["fallbacks"] = json!("default");
    }
    body
}

pub fn parse_response(v: &Value) -> AppResult<ChatResponse> {
    let content = v
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::Ai("Anthropic response has no content".into()))?
        .iter()
        .map(|b| match b.get("type").and_then(Value::as_str) {
            Some("text") => Block::Text(b.get("text").and_then(Value::as_str).unwrap_or("").to_string()),
            Some("tool_use") => Block::ToolUse {
                id: b.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: b.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                input: b.get("input").cloned().unwrap_or(Value::Null),
            },
            _ => Block::Opaque(b.clone()),
        })
        .collect();
    let stop = match v.get("stop_reason").and_then(Value::as_str) {
        Some("end_turn") | Some("stop_sequence") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("refusal") => StopReason::Refusal,
        _ => StopReason::Other,
    };
    let u = v.get("usage").cloned().unwrap_or(Value::Null);
    let n = |k: &str| u.get(k).and_then(Value::as_i64).unwrap_or(0);
    Ok(ChatResponse {
        content,
        stop,
        usage: Usage {
            input_tokens: n("input_tokens"),
            output_tokens: n("output_tokens"),
            cache_read_tokens: n("cache_read_input_tokens"),
            cache_write_tokens: n("cache_creation_input_tokens"),
        },
        model: v.get("model").and_then(Value::as_str).unwrap_or("").to_string(),
    })
}

#[async_trait]
impl AiProvider for AnthropicProvider {
    fn id(&self) -> &'static str {
        "anthropic"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn supports_documents(&self) -> bool {
        true
    }

    async fn complete(&self, req: &ChatRequest) -> AppResult<ChatResponse> {
        let body = build_body(&self.model, &self.effort, req);
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut rb = self
                .http
                .post(API_URL)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json");
            if supports_fallbacks(&self.model) {
                rb = rb.header("anthropic-beta", FALLBACK_BETA);
            }
            let resp = match rb.json(&body).send().await {
                Ok(r) => r,
                Err(e) if attempt < 4 && (e.is_timeout() || e.is_connect()) => {
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let status = resp.status();
            let retry_after = resp.headers().get("retry-after").and_then(|h| h.to_str().ok()).and_then(|s| s.parse::<u64>().ok());
            let should_retry_hdr = resp.headers().get("x-should-retry").and_then(|h| h.to_str().ok()).map(|s| s == "true");
            let text = resp.text().await?;
            if status.is_success() {
                let v: Value = serde_json::from_str(&text)?;
                return parse_response(&v);
            }
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(String::from))
                .unwrap_or_else(|| format!("HTTP {status}"));
            let retryable = should_retry_hdr.unwrap_or(status.as_u16() == 429 || status.as_u16() == 408 || status.is_server_error());
            if retryable && attempt < 4 {
                let wait = retry_after.unwrap_or(2u64.pow(attempt)).min(60);
                tracing::warn!(%status, "Anthropic API retry in {wait}s: {message}");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            return Err(match status.as_u16() {
                401 => AppError::Ai("Anthropic rejected the API key (401). Update it in Settings → AI.".into()),
                403 => AppError::Ai(format!("Anthropic API permission error: {message}")),
                404 => AppError::Ai(format!("Model not found: {message}")),
                429 => AppError::Ai(format!("Anthropic rate limit reached: {message}")),
                _ => AppError::Ai(format!("Anthropic API error ({status}): {message}")),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{Message, ToolDef};

    #[test]
    fn body_has_expected_shape() {
        let req = ChatRequest {
            system: "sys".into(),
            messages: vec![Message::user_text("hi")],
            tools: vec![ToolDef { name: "t".into(), description: "d".into(), input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}) }],
            max_tokens: 1000,
            json_schema: None,
        };
        let b = build_body("claude-opus-5-5", "high", &req);
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["output_config"]["effort"], "high");
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["tools"][0]["strict"], true);
        assert!(b.get("temperature").is_none(), "sampling params are rejected by current models");
        let old = build_body("claude-haiku-4-5", "high", &req);
        assert!(old.get("thinking").is_none() && old.get("fallbacks").is_none());
    }

    #[test]
    fn parses_tool_use_and_preserves_unknown_blocks() {
        let v = json!({
            "model": "claude-opus-5-5", "stop_reason": "tool_use",
            "content": [
                {"type":"thinking","thinking":"","signature":"abc"},
                {"type":"text","text":"ok"},
                {"type":"tool_use","id":"toolu_1","name":"write_file","input":{"path":"a"}}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 3}
        });
        let r = parse_response(&v).unwrap();
        assert_eq!(r.stop, StopReason::ToolUse);
        assert!(matches!(&r.content[0], Block::Opaque(o) if o["signature"] == "abc"));
        assert_eq!(block_to_json(&r.content[0]), v["content"][0], "thinking echoed unchanged");
        assert_eq!(r.tool_uses()[0].1, "write_file");
        assert_eq!(r.usage.cache_read_tokens, 3);
    }

    #[test]
    fn refusal_is_detected() {
        let r = parse_response(&json!({"stop_reason":"refusal","content":[],"usage":{}})).unwrap();
        assert_eq!(r.stop, StopReason::Refusal);
    }
}
