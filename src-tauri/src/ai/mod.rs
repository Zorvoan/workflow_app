//! Provider-neutral AI interface. The engine only talks to `AiProvider`; new
//! vendors are added by implementing the trait and registering in `create`.

pub mod anthropic;
pub mod openai;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::secrets::{SecretKey, SecretStore};
use crate::settings::AiSettings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text(String),
    /// base64 image (png/jpeg/gif/webp)
    Image { media_type: String, data_b64: String },
    /// base64 PDF
    Pdf { data_b64: String, title: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
    /// Provider-specific block that must be echoed back unchanged
    /// (e.g. Anthropic `thinking` or `fallback` blocks).
    Opaque(Value),
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

impl Message {
    pub fn user_text(t: impl Into<String>) -> Self {
        Self { role: Role::User, content: vec![Block::Text(t.into())] }
    }
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub max_tokens: u32,
    /// Constrain the reply to this JSON schema (structured output).
    pub json_schema: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    Other,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: Vec<Block>,
    pub stop: StopReason,
    pub usage: Usage,
    pub model: String,
}

impl ChatResponse {
    pub fn text(&self) -> String {
        self.content.iter().filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None }).collect::<Vec<_>>().join("")
    }
    pub fn tool_uses(&self) -> Vec<(String, String, Value)> {
        self.content
            .iter()
            .filter_map(|b| if let Block::ToolUse { id, name, input } = b { Some((id.clone(), name.clone(), input.clone())) } else { None })
            .collect()
    }
}

#[async_trait]
pub trait AiProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn model(&self) -> &str;
    /// Whether PDFs/images can be sent natively.
    fn supports_documents(&self) -> bool;
    async fn complete(&self, req: &ChatRequest) -> AppResult<ChatResponse>;
}

pub type SharedProvider = Arc<dyn AiProvider>;

pub fn key_for(provider: &str) -> SecretKey {
    SecretKey::AiApiKey { provider: provider.to_string() }
}

pub fn create(settings: &AiSettings, secrets: &dyn SecretStore) -> AppResult<SharedProvider> {
    let key = secrets.get(&key_for(&settings.provider))?;
    match settings.provider.as_str() {
        "anthropic" => {
            let key = key.ok_or_else(|| AppError::Ai("No Anthropic API key configured. Add one in Settings → AI.".into()))?;
            Ok(Arc::new(anthropic::AnthropicProvider::new(key, settings.model.clone(), settings.effort.clone())?))
        }
        "openai_compatible" => Ok(Arc::new(openai::OpenAiCompatibleProvider::new(key, settings.base_url.clone(), settings.model.clone())?)),
        other => Err(AppError::Ai(format!("Unknown AI provider '{other}'"))),
    }
}
