//! Application services: the glue between integrations, the engine and the DB.
//! Tauri commands in `commands.rs` are thin wrappers around these.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::ai::{self, Usage};
use crate::db::models::*;
use crate::db::Db;
use crate::edupage::{self, EdupageClient, SyncReport};
use crate::engine::{self, sandbox::Policy, Level, Reporter, Stage, Workspace};
use crate::error::{AppError, AppResult};
use crate::github::{self, files as ghfiles, GitHub};
use crate::secrets::{known_secret_values, SecretKey, SharedSecrets};
use crate::settings::Settings;
use crate::submission::{self, Confirmations, SubmissionPreview};

pub struct AppState {
    pub db: Db,
    pub secrets: SharedSecrets,
    pub edupage: tokio::sync::Mutex<Option<EdupageClient>>,
    pub builds: std::sync::Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub confirmations: Confirmations,
    pub sync_lock: tokio::sync::Mutex<()>,
}

impl AppState {
    pub fn new(db: Db, secrets: SharedSecrets) -> Self {
        Self {
            db,
            secrets,
            edupage: tokio::sync::Mutex::new(None),
            builds: Default::default(),
            confirmations: Confirmations::default(),
            sync_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn settings(&self) -> AppResult<Settings> {
        Settings::load(&self.db)
    }

    /// Get a connected EduPage client, restoring the session if needed.
    pub async fn edupage(&self) -> AppResult<tokio::sync::MutexGuard<'_, Option<EdupageClient>>> {
        let mut guard = self.edupage.lock().await;
        if guard.is_none() {
            let s = self.settings()?;
            let client = edupage::restore(self.secrets.as_ref(), &s.edupage.subdomain, &s.edupage.username).await?;
            *guard = Some(client);
        }
        Ok(guard)
    }

    pub fn all_secret_values(&self) -> Vec<String> {
        let s = self.settings().unwrap_or_default();
        let keys = vec![
            SecretKey::GithubToken,
            ai::key_for("anthropic"),
            ai::key_for("openai_compatible"),
            edupage::password_key(&s.edupage.subdomain, &s.edupage.username),
            edupage::session_key(&s.edupage.subdomain, &s.edupage.username),
        ];
        known_secret_values(self.secrets.as_ref(), &keys)
    }

    pub fn workspace(&self, a: &Assignment) -> AppResult<Workspace> {
        let s = self.settings()?;
        let ws = Workspace::for_assignment(&PathBuf::from(&s.build.projects_dir), a);
        if a.workspace_path.is_none() {
            self.db.set_workspace(&a.id, &ws.root.to_string_lossy())?;
        }
        Ok(ws)
    }
}

// --------------------------------------------------------------------- sync

pub async fn sync(app: &AppHandle, state: &AppState) -> AppResult<SyncReport> {
    let Ok(_running) = state.sync_lock.try_lock() else {
        return Err(AppError::Conflict("A synchronization is already running".into()));
    };
    let run_id = state.db.start_sync_run()?;
    let result = sync_inner(state).await;
    match &result {
        Ok(r) => {
            state.db.finish_sync_run(run_id, true, r.new as i64, r.changed as i64, None)?;
            let _ = app.emit("sync-finished", r);
        }
        Err(e) => {
            if matches!(e, AppError::NotAuthenticated(_)) {
                *state.edupage.lock().await = None;
            }
            state.db.finish_sync_run(run_id, false, 0, 0, Some(&e.to_string()))?;
            let _ = app.emit("sync-failed", e);
        }
    }
    result
}

async fn sync_inner(state: &AppState) -> AppResult<SyncReport> {
    let s = state.settings()?;
    let mut guard = state.edupage().await?;
    let client = guard.as_mut().expect("edupage() guarantees a client");
    let synced = match edupage::sync(client, &state.db, s.edupage.history_days).await {
        Ok(r) => r,
        Err(AppError::NotAuthenticated(m)) => {
            // Session expired mid-run: drop it and retry once with saved credentials.
            *guard = None;
            drop(guard);
            tracing::info!("EduPage session expired during sync: {m}");
            let mut guard = state.edupage().await?;
            let client = guard.as_mut().expect("client");
            let r = edupage::sync(client, &state.db, s.edupage.history_days).await?;
            return finish_sync(state, client, r).await;
        }
        Err(e) => return Err(e),
    };
    if let Some(sid) = client.session_id() {
        let _ = state.secrets.set(&edupage::session_key(&s.edupage.subdomain, &s.edupage.username), &sid);
    }
    finish_sync(state, client, synced).await
}

async fn finish_sync(state: &AppState, client: &EdupageClient, (mut report, new_ids): (SyncReport, Vec<String>)) -> AppResult<SyncReport> {
    for id in new_ids {
        let a = state.db.get_assignment(&id)?;
        let ws = state.workspace(&a)?;
        match edupage::download_attachments(client, &state.db, &id, &ws.materials_dir).await {
            Ok((n, warnings)) => {
                report.attachments_downloaded += n;
                report.warnings.extend(warnings);
            }
            Err(e) => report.warnings.push(format!("{}: {e}", a.title)),
        }
    }
    tracing::info!(total = report.total, new = report.new, changed = report.changed, "sync complete");
    Ok(report)
}

pub async fn download_materials(state: &AppState, id: &str) -> AppResult<(usize, Vec<String>)> {
    let a = state.db.get_assignment(id)?;
    let ws = state.workspace(&a)?;
    std::fs::create_dir_all(&ws.materials_dir)?;
    std::fs::write(ws.materials_dir.join("assignment.md"), engine::assignment_markdown(&a))?;
    let guard = state.edupage().await?;
    edupage::download_attachments(guard.as_ref().expect("client"), &state.db, id, &ws.materials_dir).await
}

// -------------------------------------------------------------------- build

struct DbReporter {
    db: Db,
    app: AppHandle,
    build_id: i64,
    assignment_id: String,
}

impl Reporter for DbReporter {
    fn report(&self, stage: Stage, level: Level, message: &str) {
        match self.db.add_build_event(self.build_id, stage.as_str(), level.as_str(), message) {
            Ok(ev) => {
                let _ = self.app.emit("build-event", &ev);
            }
            Err(e) => tracing::error!("could not store build event: {e}"),
        }
    }
    fn usage(&self, provider: &str, model: &str, usage: &Usage) {
        if let Err(e) = self.db.record_ai_usage(provider, model, usage, Some(&self.assignment_id)) {
            tracing::error!("could not record AI usage: {e}");
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildFinished {
    pub assignment_id: String,
    pub build_id: i64,
    pub succeeded: bool,
    pub error: Option<String>,
}

/// Start a build in the background; returns the build id immediately.
pub fn start_build(app: AppHandle, state: Arc<AppState>, id: String) -> AppResult<i64> {
    let a = state.db.get_assignment(&id)?;
    let s = state.settings()?;
    let provider = ai::create(&s.ai, state.secrets.as_ref())?; // fail fast if no key
    let build_id = state.db.create_build(&id)?;
    state.db.set_status(&id, Status::Building)?;
    let cancel = Arc::new(AtomicBool::new(false));
    state.builds.lock().unwrap_or_else(|p| p.into_inner()).insert(id.clone(), cancel.clone());

    tauri::async_runtime::spawn(async move {
        let reporter = DbReporter { db: state.db.clone(), app: app.clone(), build_id, assignment_id: id.clone() };
        let outcome = async {
            let ws = state.workspace(&a)?;
            ws.create()?;
            // Make sure materials are present (best effort; offline builds still work from what exists).
            reporter.report(Stage::Prepare, Level::Info, "Downloading and preparing assignment materials…");
            match download_materials(&state, &id).await {
                Ok((n, warnings)) => {
                    if n > 0 {
                        reporter.report(Stage::Prepare, Level::Info, &format!("Downloaded {n} attachment(s)"));
                    }
                    for w in warnings {
                        reporter.report(Stage::Prepare, Level::Warn, &w);
                    }
                }
                Err(e) => reporter.report(Stage::Prepare, Level::Warn, &format!("Could not refresh attachments: {e}")),
            }
            let input = engine::BuildInput {
                assignment: &a,
                ws: &ws,
                ai: provider.as_ref(),
                policy: Policy { allow_dependency_install: s.build.allow_dependency_install, timeout: std::time::Duration::from_secs(s.build.command_timeout_secs) },
                enabled_languages: s.build.enabled_languages.clone(),
                max_fix_iterations: s.ai.max_fix_iterations,
                max_agent_turns: s.ai.max_agent_turns,
                cancel: cancel.clone(),
                reporter: &reporter,
            };
            Ok::<_, AppError>(engine::build(input, &s.build.extra_gitignore).await)
        }
        .await;

        let (succeeded, error) = match outcome {
            Ok(r) => {
                if let Some(plan) = &r.plan {
                    let _ = state.db.set_build_plan(build_id, &serde_json::to_string(plan).unwrap_or_default());
                }
                let cancelled = cancel.load(std::sync::atomic::Ordering::Relaxed);
                let o = if r.succeeded { BuildOutcome::Succeeded } else if cancelled { BuildOutcome::Cancelled } else { BuildOutcome::Failed };
                let _ = state.db.finish_build(build_id, o, Some(&r.summary), r.error.as_deref());
                (r.succeeded, r.error)
            }
            Err(e) => {
                reporter.report(Stage::Done, Level::Error, &e.to_string());
                let _ = state.db.finish_build(build_id, BuildOutcome::Failed, None, Some(&e.to_string()));
                (false, Some(e.to_string()))
            }
        };
        let _ = state.db.set_status(&id, if succeeded { Status::ReadyToSubmit } else { Status::BuildFailed });
        if succeeded && s.github.auto_publish {
            auto_publish(&state, &reporter, &id).await;
        }
        state.builds.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
        let _ = app.emit("build-finished", BuildFinished { assignment_id: id, build_id, succeeded, error });
    });
    Ok(build_id)
}

/// After a successful build: push to GitHub if connected. Failures are logged, never fatal.
async fn auto_publish(state: &AppState, reporter: &DbReporter, id: &str) {
    if !matches!(state.secrets.get(&SecretKey::GithubToken), Ok(Some(_))) {
        reporter.report(Stage::Publish, Level::Warn, "GitHub is not connected; skipped automatic publishing. Connect it in Settings → GitHub.");
        return;
    }
    reporter.report(Stage::Publish, Level::Info, "Publishing to GitHub…");
    match publish(state, id, None).await {
        Ok(r) => reporter.report(
            Stage::Publish,
            Level::Success,
            &format!("{} {} ({} files, commit {})", if r.created { "Created" } else { "Updated" }, r.url, r.files, &r.commit[..r.commit.len().min(7)]),
        ),
        Err(e) => reporter.report(Stage::Publish, Level::Error, &format!("GitHub publishing failed: {e}")),
    }
}

pub fn cancel_build(state: &AppState, id: &str) -> AppResult<()> {
    let builds = state.builds.lock().unwrap_or_else(|p| p.into_inner());
    let flag = builds.get(id).ok_or_else(|| AppError::NotFound("No running build for this assignment".into()))?;
    flag.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

// ------------------------------------------------------------------- github

#[derive(Debug, Clone, Serialize)]
pub struct PublishResult {
    pub url: String,
    pub full_name: String,
    pub commit: String,
    pub files: usize,
    pub created: bool,
}

pub async fn publish(state: &AppState, id: &str, private_override: Option<bool>) -> AppResult<PublishResult> {
    let s = state.settings()?;
    let a = state.db.get_assignment(id)?;
    if !matches!(a.status.as_str(), "ready_to_submit" | "submitted") {
        return Err(AppError::Policy("Only successfully built assignments can be published. Build it first.".into()));
    }
    let token = state.secrets.get(&SecretKey::GithubToken)?.ok_or_else(|| AppError::NotAuthenticated("Connect GitHub in Settings first".into()))?;
    let ws = state.workspace(&a)?;
    let list = ghfiles::collect(&ws.project_dir)?;
    if list.is_empty() {
        return Err(AppError::InvalidInput("Project folder is empty".into()));
    }
    let findings = ghfiles::scan_files(&list, &state.all_secret_values());
    if let Some(f) = findings.first() {
        return Err(AppError::Policy(format!(
            "Upload blocked: possible {} in {} (line {}). Remove it or add the file to .gitignore.",
            f.kind, f.path, f.line
        )));
    }
    let gh = GitHub::new(&token)?;
    let existing = match &a.repo_full_name {
        Some(n) => gh.get_repo(n).await?,
        None => None,
    };
    let (repo, created) = match existing {
        Some(r) => (r, false),
        None => {
            let name = github::repo_name(&s.github.repo_name_template, a.subject.as_deref(), &a.title, &a.external_id);
            let desc = format!("{}{}", a.subject.as_deref().map(|s| format!("{s}: ")).unwrap_or_default(), a.title);
            (gh.create_repo(&name, private_override.unwrap_or(s.github.default_private), &desc).await?, true)
        }
    };
    let msg = if created { format!("Initial commit: {}", a.title) } else { format!("Update: {}", a.title) };
    let commit = gh.push_snapshot(&repo, &list, &msg).await?;
    state.db.set_repo(id, &repo.html_url, &repo.full_name)?;
    tracing::info!(repo = %repo.full_name, files = list.len(), "published to GitHub");
    Ok(PublishResult { url: repo.html_url, full_name: repo.full_name, commit, files: list.len(), created })
}

// --------------------------------------------------------------- submission

pub async fn prepare_submission(state: &AppState, id: &str) -> AppResult<SubmissionPreview> {
    let a = state.db.get_assignment(id)?;
    if !matches!(a.status.as_str(), "ready_to_submit" | "submitted") {
        return Err(AppError::Policy("Build the assignment successfully before submitting".into()));
    }
    let ws = state.workspace(&a)?;
    let slug = crate::util::slugify(&a.title, 50);
    let slug = if slug.is_empty() { "assignment".to_string() } else { slug };
    let (files, mut warnings) = submission::package(&ws.project_dir, &ws.root.join("submission"), &slug, &state.all_secret_values())?;
    let connected = state.edupage().await.is_ok();
    if a.repo_url.is_none() {
        warnings.push("Not published to GitHub yet; the message will not include a repository link.".into());
    }
    let expectation = state
        .db
        .latest_successful_plan(id)?
        .and_then(|p| serde_json::from_str::<engine::plan::Plan>(&p).ok())
        .map(|p| format!("{}{}", p.submission_expects, if p.submission_notes.is_empty() { String::new() } else { format!(" — {}", p.submission_notes) }));
    let token = state.confirmations.issue(id, files.clone());
    Ok(SubmissionPreview {
        assignment_id: id.into(),
        title: a.title.clone(),
        teacher: a.teacher.clone(),
        repo_url: a.repo_url.clone(),
        message: submission::default_message(&a, a.repo_url.as_deref()),
        methods: submission::methods(&a, connected),
        files,
        expectation,
        confirm_token: token,
        warnings,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct SubmitResult {
    pub state: String,
    pub platform_ref: Option<String>,
    pub message: String,
}

pub async fn submit(state: &AppState, id: &str, token: &str, method: &str, message: &str) -> AppResult<SubmitResult> {
    let pending = state.confirmations.redeem(token, id)?;
    let a = state.db.get_assignment(id)?;
    if method != submission::METHOD_MESSAGE {
        return Err(AppError::InvalidInput("Use \"Mark as handed in\" for manual hand-in".into()));
    }
    let teacher = a.teacher_ref.clone().ok_or_else(|| AppError::Policy("Teacher's EduPage account unknown".into()))?;
    if message.trim().is_empty() {
        return Err(AppError::InvalidInput("Message cannot be empty".into()));
    }
    let details = |extra: serde_json::Value| {
        serde_json::json!({"files": pending.files, "repo_url": a.repo_url, "message": message, "teacher": a.teacher, "extra": extra}).to_string()
    };
    let result = async {
        let guard = state.edupage().await?;
        let client = guard.as_ref().expect("client");
        let mut uploaded = Vec::new();
        for f in &pending.files {
            uploaded.push(client.upload(std::path::Path::new(&f.path)).await?);
        }
        let mut text = message.trim().to_string();
        if !uploaded.is_empty() {
            text.push_str("\n\n");
            for u in &uploaded {
                text.push_str(&format!("{}: {}\n", u.name, u.url));
            }
        }
        let timeline_id = client.send_message(&teacher, &text, &uploaded).await?;
        Ok::<_, AppError>((timeline_id, uploaded))
    }
    .await;

    match result {
        Ok((timeline_id, uploaded)) => {
            state.db.add_submission(&NewSubmission {
                assignment_id: id.into(),
                method: method.into(),
                state: SubmissionState::Confirmed,
                details_json: details(serde_json::json!({"uploaded": uploaded})),
                platform_ref: Some(timeline_id.clone()),
                error: None,
            })?;
            state.db.set_status(id, Status::Submitted)?;
            Ok(SubmitResult {
                state: "confirmed".into(),
                platform_ref: Some(timeline_id.clone()),
                message: format!("EduPage accepted the message (timeline item {timeline_id})."),
            })
        }
        Err(e) => {
            state.db.add_submission(&NewSubmission {
                assignment_id: id.into(),
                method: method.into(),
                state: SubmissionState::Failed,
                details_json: details(serde_json::Value::Null),
                platform_ref: None,
                error: Some(e.to_string()),
            })?;
            Err(e)
        }
    }
}

pub fn record_manual(state: &AppState, id: &str, token: &str, note: &str) -> AppResult<()> {
    let pending = state.confirmations.redeem(token, id)?;
    let a = state.db.get_assignment(id)?;
    state.db.add_submission(&NewSubmission {
        assignment_id: id.into(),
        method: submission::METHOD_MANUAL.into(),
        state: SubmissionState::SelfReported,
        details_json: serde_json::json!({"files": pending.files, "repo_url": a.repo_url, "note": note}).to_string(),
        platform_ref: None,
        error: None,
    })?;
    state.db.set_status(id, Status::Submitted)?;
    Ok(())
}
