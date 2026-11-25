//! EduPage integration: session lifecycle + assignment sync + attachments.

pub mod client;
pub mod codec;
pub mod parse;

use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::db::models::{RemoteAssignmentRow, UpsertOutcome};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::secrets::{SecretKey, SecretStore};
use crate::util::safe_file_name;
pub use client::{EdupageClient, LoginOutcome};

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub subdomain: String,
    pub username: String,
    pub user_id: Option<String>,
    pub has_saved_password: bool,
    pub has_saved_session: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SyncReport {
    pub total: usize,
    pub new: usize,
    pub changed: usize,
    pub attachments_downloaded: usize,
    pub warnings: Vec<String>,
}

pub fn session_key(subdomain: &str, username: &str) -> SecretKey {
    SecretKey::EdupageSession { subdomain: subdomain.into(), username: username.into() }
}

pub fn password_key(subdomain: &str, username: &str) -> SecretKey {
    SecretKey::EdupagePassword { subdomain: subdomain.into(), username: username.into() }
}

/// Re-establish a connection on startup or after expiry: saved session first,
/// then the saved password (only if the user opted in to saving it).
pub async fn restore(secrets: &dyn SecretStore, subdomain: &str, username: &str) -> AppResult<EdupageClient> {
    if subdomain.is_empty() || username.is_empty() {
        return Err(AppError::NotAuthenticated("EduPage is not configured. Sign in from Settings.".into()));
    }
    let mut client = EdupageClient::new(subdomain)?;
    if let Some(sid) = secrets.get(&session_key(subdomain, username))? {
        match client.resume(&sid).await {
            Ok(()) => return Ok(client),
            Err(AppError::NotAuthenticated(_)) => {
                tracing::info!("saved EduPage session expired");
                let _ = secrets.delete(&session_key(subdomain, username));
            }
            Err(e) => return Err(e),
        }
    }
    let Some(password) = secrets.get(&password_key(subdomain, username))? else {
        return Err(AppError::NotAuthenticated("EduPage session expired. Sign in again from Settings.".into()));
    };
    let mut client = EdupageClient::new(subdomain)?;
    match client.login(username, &password).await? {
        LoginOutcome::LoggedIn { .. } => {
            if let Some(sid) = client.session_id() {
                secrets.set(&session_key(subdomain, username), &sid)?;
            }
            Ok(client)
        }
        LoginOutcome::TwoFactorRequired | LoginOutcome::CaptchaRequired => Err(AppError::NotAuthenticated(
            "EduPage requires verification (2FA or CAPTCHA). Use \"Sign in with browser\" in Settings.".into(),
        )),
    }
}

/// Fetch every homework item and store it. Returns new/changed counts.
pub async fn sync(client: &mut EdupageClient, db: &Db, history_days: u32) -> AppResult<(SyncReport, Vec<String>)> {
    client.refresh().await?;
    let home = client.userhome.clone().unwrap_or(Value::Null);
    let mut items = parse::userhome_items(&home);
    let mut props = home.get("userProps").cloned().unwrap_or(Value::Null);
    let dbi = home.get("dbi").cloned().unwrap_or(Value::Null);
    let mut report = SyncReport::default();

    match client.timeline_history(history_days).await {
        Ok((older, older_props)) => {
            items.extend(older);
            if let (Some(p), Some(op)) = (props.as_object_mut(), older_props.as_object()) {
                for (k, v) in op {
                    p.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        Err(e) => report.warnings.push(format!("Older timeline history unavailable: {e}")),
    }

    let assignments: Vec<RemoteAssignmentRow> = parse::homework_from_items(&items, &dbi, &props, &client.subdomain);
    report.total = assignments.len();
    let mut new_ids = Vec::new();
    for a in &assignments {
        match db.upsert_remote_assignment(a)? {
            UpsertOutcome::New => {
                report.new += 1;
                new_ids.push(a.id());
            }
            UpsertOutcome::Changed => report.changed += 1,
            UpsertOutcome::Unchanged => {}
        }
    }
    // Deliberately no "mark removed" here: the timeline only covers a time
    // window, so absence does not mean the teacher deleted the homework.
    Ok((report, new_ids))
}

/// Download all not-yet-downloaded school-hosted attachments into `materials_dir`.
/// Non-EduPage links are listed in `links.md` instead of being fetched.
pub async fn download_attachments(client: &EdupageClient, db: &Db, assignment_id: &str, materials_dir: &Path) -> AppResult<(usize, Vec<String>)> {
    tokio::fs::create_dir_all(materials_dir).await?;
    let mut downloaded = 0;
    let mut warnings = Vec::new();
    let mut external = Vec::new();
    for att in db.attachments(assignment_id)? {
        if !parse::is_school_host(&att.url) {
            external.push(format!("- [{}]({})", att.name, att.url));
            continue;
        }
        if let Some(p) = &att.local_path {
            if Path::new(p).exists() {
                continue;
            }
        }
        let file_name = format!("{:02}-{}", att.id % 100, safe_file_name(&att.name));
        let dest = materials_dir.join(&file_name);
        match client.download(&att.url, &dest).await {
            Ok(size) => {
                db.attachment_downloaded(att.id, &dest.to_string_lossy(), size as i64)?;
                downloaded += 1;
            }
            Err(e) => {
                tracing::warn!(attachment = att.id, "download failed: {e}");
                db.attachment_failed(att.id, &e.to_string())?;
                warnings.push(format!("{}: {e}", att.name));
            }
        }
    }
    if !external.is_empty() {
        let body = format!("# External links\n\nThese links were not downloaded automatically.\n\n{}\n", external.join("\n"));
        tokio::fs::write(materials_dir.join("links.md"), body).await?;
    }
    Ok((downloaded, warnings))
}
