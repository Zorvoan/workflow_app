//! Application-wide error type.
//!
//! Every Tauri command returns `Result<T, AppError>`. The error serializes to a
//! `{ kind, message }` object so the UI can branch on `kind` (e.g. show a login
//! prompt for `not_authenticated`) without string matching.

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    NotAuthenticated(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    /// The remote platform answered in a way we do not understand. This usually
    /// means EduPage changed its web interface and the integration needs updating.
    #[error("{0}")]
    UnexpectedResponse(String),
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Policy(String),
    #[error("{0}")]
    Ai(String),
    #[error("{0}")]
    Secret(String),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Internal(String),
}

impl AppError {
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::NotAuthenticated(_) => "not_authenticated",
            AppError::InvalidInput(_) => "invalid_input",
            AppError::NotFound(_) => "not_found",
            AppError::Conflict(_) => "conflict",
            AppError::UnexpectedResponse(_) => "unexpected_response",
            AppError::Network(_) => "network",
            AppError::Policy(_) => "policy",
            AppError::Ai(_) => "ai",
            AppError::Secret(_) => "secret",
            AppError::Database(_) => "database",
            AppError::Io(_) => "io",
            AppError::Internal(_) => "internal",
        }
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        // Strip the URL: it can contain query parameters we do not want in logs/UI.
        AppError::Network(e.without_url().to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::UnexpectedResponse(format!("invalid JSON: {e}"))
    }
}

impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("AppError", 2)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("message", &self.to_string())?;
        st.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
