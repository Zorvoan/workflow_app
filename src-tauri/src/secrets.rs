//! Credential storage.
//!
//! On Windows this is the Windows Credential Manager (via the `keyring` crate's
//! `windows-native` backend); on macOS the Keychain; on Linux the Secret Service.
//! Nothing in this module ever writes a secret to disk, SQLite or logs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

const SERVICE: &str = "EduTask";

/// Every secret the app knows about. Using an enum (not free-form strings)
/// keeps the set of stored credentials auditable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SecretKey {
    /// EduPage password, stored only if the user opts in to "remember password".
    EdupagePassword { subdomain: String, username: String },
    /// EduPage `PHPSESSID` session cookie, so the user does not need to log in on every start.
    EdupageSession { subdomain: String, username: String },
    GithubToken,
    AiApiKey { provider: String },
}

impl SecretKey {
    fn account(&self) -> String {
        match self {
            SecretKey::EdupagePassword { subdomain, username } => format!("edupage-password:{subdomain}:{username}"),
            SecretKey::EdupageSession { subdomain, username } => format!("edupage-session:{subdomain}:{username}"),
            SecretKey::GithubToken => "github-token".into(),
            SecretKey::AiApiKey { provider } => format!("ai-key:{provider}"),
        }
    }
}

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &SecretKey) -> AppResult<Option<String>>;
    fn set(&self, key: &SecretKey, value: &str) -> AppResult<()>;
    fn delete(&self, key: &SecretKey) -> AppResult<()>;
}

pub type SharedSecrets = Arc<dyn SecretStore>;

/// OS credential store.
pub struct KeyringStore;

impl KeyringStore {
    fn entry(key: &SecretKey) -> AppResult<keyring::Entry> {
        keyring::Entry::new(SERVICE, &key.account()).map_err(|e| AppError::Secret(format!("credential store unavailable: {e}")))
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, key: &SecretKey) -> AppResult<Option<String>> {
        match Self::entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AppError::Secret(format!("could not read credential: {e}"))),
        }
    }

    fn set(&self, key: &SecretKey, value: &str) -> AppResult<()> {
        Self::entry(key)?
            .set_password(value)
            .map_err(|e| AppError::Secret(format!("could not store credential: {e}")))
    }

    fn delete(&self, key: &SecretKey) -> AppResult<()> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AppError::Secret(format!("could not delete credential: {e}"))),
        }
    }
}

/// In-memory store for tests.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, key: &SecretKey) -> AppResult<Option<String>> {
        Ok(self.0.lock().unwrap().get(&key.account()).cloned())
    }
    fn set(&self, key: &SecretKey, value: &str) -> AppResult<()> {
        self.0.lock().unwrap().insert(key.account(), value.to_string());
        Ok(())
    }
    fn delete(&self, key: &SecretKey) -> AppResult<()> {
        self.0.lock().unwrap().remove(&key.account());
        Ok(())
    }
}

/// Collects every secret value currently stored, so outgoing uploads can be
/// scanned for accidental inclusion of the user's own credentials.
pub fn known_secret_values(store: &dyn SecretStore, keys: &[SecretKey]) -> Vec<String> {
    keys.iter()
        .filter_map(|k| store.get(k).ok().flatten())
        .filter(|v| v.len() >= 6)
        .collect()
}
