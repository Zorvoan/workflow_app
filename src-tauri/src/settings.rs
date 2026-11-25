//! Non-secret application settings, persisted as one JSON document in SQLite.

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::error::{AppError, AppResult};

const KEY: &str = "app_settings";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub edupage: EdupageSettings,
    pub github: GithubSettings,
    pub ai: AiSettings,
    pub build: BuildSettings,
    pub ui: UiSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EdupageSettings {
    /// `myschool` for https://myschool.edupage.org
    pub subdomain: String,
    pub username: String,
    pub remember_password: bool,
    /// 0 disables automatic synchronization.
    pub sync_interval_minutes: u32,
    /// How far back to look for homework in the timeline history.
    pub history_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GithubSettings {
    pub default_private: bool,
    /// Tokens: {subject} {title} {year} {date} {id}
    pub repo_name_template: String,
    /// OAuth App client id for the device flow. Empty = only personal access tokens.
    pub oauth_client_id: String,
    pub login: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AiSettings {
    /// "anthropic" or "openai_compatible"
    pub provider: String,
    pub model: String,
    /// Only for openai_compatible (e.g. a local Ollama or another vendor).
    pub base_url: String,
    /// low | medium | high | xhigh | max (Anthropic effort)
    pub effort: String,
    /// How many times the engine feeds build/test failures back for fixing.
    pub max_fix_iterations: u32,
    /// Upper bound on tool-use turns in one generation pass.
    pub max_agent_turns: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BuildSettings {
    pub projects_dir: String,
    pub command_timeout_secs: u64,
    /// Allow package-manager installs (npm install, pip install, dotnet add package, …).
    pub allow_dependency_install: bool,
    /// Languages the engine is allowed to choose.
    pub enabled_languages: Vec<String>,
    /// Extra .gitignore lines appended to every generated project.
    pub extra_gitignore: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UiSettings {
    /// system | light | dark
    pub theme: String,
}

impl Default for EdupageSettings {
    fn default() -> Self {
        Self { subdomain: String::new(), username: String::new(), remember_password: true, sync_interval_minutes: 30, history_days: 60 }
    }
}

impl Default for GithubSettings {
    fn default() -> Self {
        Self { default_private: true, repo_name_template: "{subject}-{title}".into(), oauth_client_id: String::new(), login: None }
    }
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            model: "claude-opus-5-5".into(),
            base_url: String::new(),
            effort: "high".into(),
            max_fix_iterations: 3,
            max_agent_turns: 40,
        }
    }
}

pub const ALL_LANGUAGES: &[&str] = &["rust", "cpp", "c", "csharp", "java", "python", "php", "javascript", "typescript", "html", "sql", "document"];

impl Default for BuildSettings {
    fn default() -> Self {
        let projects_dir = dirs::document_dir()
            .or_else(dirs::home_dir)
            .map(|d| d.join("EduTask"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "EduTask".into());
        Self {
            projects_dir,
            command_timeout_secs: 300,
            allow_dependency_install: true,
            enabled_languages: ALL_LANGUAGES.iter().map(|s| s.to_string()).collect(),
            extra_gitignore: String::new(),
        }
    }
}

impl Default for UiSettings {
    fn default() -> Self {
        Self { theme: "system".into() }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            edupage: Default::default(),
            github: Default::default(),
            ai: Default::default(),
            build: Default::default(),
            ui: Default::default(),
        }
    }
}

impl Settings {
    pub fn load(db: &Db) -> AppResult<Self> {
        match db.get_setting(KEY)? {
            Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            None => Ok(Self::default()),
        }
    }

    pub fn save(&self, db: &Db) -> AppResult<()> {
        self.validate()?;
        db.set_setting(KEY, &serde_json::to_string(self)?)
    }

    pub fn validate(&self) -> AppResult<()> {
        let sub = self.edupage.subdomain.trim();
        if !sub.is_empty() && !sub.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(AppError::InvalidInput(
                "School URL must be the EduPage subdomain only, e.g. \"myschool\" for myschool.edupage.org".into(),
            ));
        }
        if !["low", "medium", "high", "xhigh", "max"].contains(&self.ai.effort.as_str()) {
            return Err(AppError::InvalidInput("Unknown AI effort level".into()));
        }
        if !["anthropic", "openai_compatible"].contains(&self.ai.provider.as_str()) {
            return Err(AppError::InvalidInput("Unknown AI provider".into()));
        }
        if self.ai.provider == "openai_compatible" && !self.ai.base_url.starts_with("http") {
            return Err(AppError::InvalidInput("An OpenAI-compatible provider needs a base URL".into()));
        }
        if !(10..=3600).contains(&self.build.command_timeout_secs) {
            return Err(AppError::InvalidInput("Build timeout must be between 10 and 3600 seconds".into()));
        }
        if self.build.projects_dir.trim().is_empty() {
            return Err(AppError::InvalidInput("Project directory is required".into()));
        }
        Ok(())
    }
}

/// Accepts "myschool", "myschool.edupage.org" or "https://myschool.edupage.org/..." and returns "myschool".
pub fn normalize_subdomain(input: &str) -> String {
    let s = input.trim().trim_start_matches("https://").trim_start_matches("http://");
    let host = s.split('/').next().unwrap_or("");
    host.trim_end_matches(".edupage.org").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdomain_normalization() {
        assert_eq!(normalize_subdomain("https://MySchool.edupage.org/user/"), "myschool");
        assert_eq!(normalize_subdomain("myschool.edupage.org"), "myschool");
        assert_eq!(normalize_subdomain(" gymba "), "gymba");
    }

    #[test]
    fn settings_round_trip_and_defaults() {
        let db = Db::open_in_memory().unwrap();
        let mut s = Settings::load(&db).unwrap();
        assert!(s.github.default_private, "repositories must be private by default");
        s.edupage.subdomain = "gymba".into();
        s.save(&db).unwrap();
        assert_eq!(Settings::load(&db).unwrap(), s);
    }

    #[test]
    fn rejects_bad_subdomain() {
        let mut s = Settings::default();
        s.edupage.subdomain = "evil.com/x".into();
        assert!(s.validate().is_err());
    }
}
