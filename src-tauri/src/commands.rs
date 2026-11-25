//! Tauri command handlers (the frontend's API). Thin wrappers over services.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

use crate::ai;
use crate::db::models::*;
use crate::edupage::{self, ConnectionStatus, EdupageClient, LoginOutcome, SyncReport};
use crate::engine::{plan::Plan, sandbox};
use crate::error::{AppError, AppResult};
use crate::github::{self, DeviceCode, DevicePoll, GitHub, GithubUser};
use crate::secrets::SecretKey;
use crate::services::{self, AppState, PublishResult, SubmitResult};
use crate::settings::{normalize_subdomain, Settings};
use crate::submission::SubmissionPreview;

type S<'a> = State<'a, Arc<AppState>>;

// ------------------------------------------------------------------ settings

#[tauri::command]
pub fn get_settings(state: S<'_>) -> AppResult<Settings> {
    state.settings()
}

#[tauri::command]
pub fn save_settings(state: S<'_>, mut settings: Settings) -> AppResult<Settings> {
    settings.edupage.subdomain = normalize_subdomain(&settings.edupage.subdomain);
    // The GitHub login is set by the backend, not the UI.
    settings.github.login = state.settings()?.github.login;
    settings.save(&state.db)?;
    Ok(settings)
}

#[derive(Serialize)]
pub struct SecretStatus {
    pub ai_keys: std::collections::HashMap<String, bool>,
    pub github: bool,
}

#[tauri::command]
pub fn secret_status(state: S<'_>) -> AppResult<SecretStatus> {
    let mut ai_keys = std::collections::HashMap::new();
    for p in ["anthropic", "openai_compatible"] {
        ai_keys.insert(p.to_string(), state.secrets.get(&ai::key_for(p))?.is_some());
    }
    Ok(SecretStatus { ai_keys, github: state.secrets.get(&SecretKey::GithubToken)?.is_some() })
}

#[tauri::command]
pub fn set_ai_key(state: S<'_>, provider: String, key: String) -> AppResult<()> {
    if !["anthropic", "openai_compatible"].contains(&provider.as_str()) {
        return Err(AppError::InvalidInput("unknown provider".into()));
    }
    let key = key.trim();
    if key.is_empty() {
        return state.secrets.delete(&ai::key_for(&provider));
    }
    state.secrets.set(&ai::key_for(&provider), key)
}

// ------------------------------------------------------------------- edupage

#[tauri::command]
pub async fn edupage_status(state: S<'_>) -> AppResult<ConnectionStatus> {
    let s = state.settings()?;
    let (sub, user) = (s.edupage.subdomain.clone(), s.edupage.username.clone());
    let has_saved_password = !user.is_empty() && state.secrets.get(&edupage::password_key(&sub, &user))?.is_some();
    let has_saved_session = !user.is_empty() && state.secrets.get(&edupage::session_key(&sub, &user))?.is_some();
    let (connected, user_id, message) = match state.edupage().await {
        Ok(g) => (true, g.as_ref().and_then(|c| c.user_id()), None),
        Err(e) => (false, None, Some(e.to_string())),
    };
    Ok(ConnectionStatus { connected, subdomain: sub, username: user, user_id, has_saved_password, has_saved_session, message })
}

#[derive(Deserialize)]
pub struct LoginArgs {
    pub subdomain: String,
    pub username: String,
    pub password: String,
    pub remember: bool,
}

#[tauri::command]
pub async fn edupage_login(app: AppHandle, state: S<'_>, args: LoginArgs) -> AppResult<LoginOutcome> {
    let subdomain = normalize_subdomain(&args.subdomain);
    let username = args.username.trim().to_string();
    if username.is_empty() || args.password.is_empty() {
        return Err(AppError::InvalidInput("Username and password are required".into()));
    }
    let mut client = EdupageClient::new(&subdomain)?;
    let outcome = client.login(&username, &args.password).await?;
    if let LoginOutcome::LoggedIn { subdomain: actual, .. } = &outcome {
        let mut s = state.settings()?;
        s.edupage.subdomain = actual.clone();
        s.edupage.username = username.clone();
        s.edupage.remember_password = args.remember;
        s.save(&state.db)?;
        if let Some(sid) = client.session_id() {
            state.secrets.set(&edupage::session_key(actual, &username), &sid)?;
        }
        if args.remember {
            state.secrets.set(&edupage::password_key(actual, &username), &args.password)?;
        } else {
            state.secrets.delete(&edupage::password_key(actual, &username))?;
        }
        *state.edupage.lock().await = Some(client);
        let st = state.inner().clone();
        tauri::async_runtime::spawn(async move {
            let _ = services::sync(&app, &st).await;
        });
    }
    Ok(outcome)
}

/// Open EduPage's own login page in a separate window. When the user reaches
/// the logged-in area, EduTask reads the session cookie for that school only
/// and closes the window. 2FA, CAPTCHA and SSO are handled by EduPage itself.
#[tauri::command]
pub async fn edupage_browser_login(app: AppHandle, state: S<'_>, subdomain: String, username: String) -> AppResult<()> {
    let subdomain = normalize_subdomain(&subdomain);
    let mut s = state.settings()?;
    s.edupage.subdomain = subdomain.clone();
    if !username.trim().is_empty() {
        s.edupage.username = username.trim().to_string();
    }
    s.validate()?;
    if subdomain.is_empty() {
        return Err(AppError::InvalidInput("Enter your school's EduPage address first".into()));
    }
    s.save(&state.db)?;

    if let Some(w) = app.get_webview_window("edupage-login") {
        let _ = w.close();
    }
    let start = url::Url::parse(&format!("https://{subdomain}.edupage.org/login/")).map_err(|e| AppError::InvalidInput(e.to_string()))?;
    let school_host = format!("{subdomain}.edupage.org");
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let app2 = app.clone();
    WebviewWindowBuilder::new(&app, "edupage-login", WebviewUrl::External(start))
        .title("Sign in to EduPage")
        .inner_size(520.0, 720.0)
        .incognito(true)
        // Remote page: only https navigation; it has no access to EduTask's IPC
        // (capabilities are granted to the "main" window only).
        .on_navigation(|u| u.scheme() == "https")
        .on_page_load(move |win, payload| {
            let u = payload.url().clone();
            if payload.event() != tauri::webview::PageLoadEvent::Finished
                || u.host_str() != Some(school_host.as_str())
                || !u.path().starts_with("/user")
                || done.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return;
            }
            let app = app2.clone();
            let done = done.clone();
            // Reading cookies from an event handler deadlocks on Windows (WebView2); use a thread.
            std::thread::spawn(move || {
                let sid = win
                    .cookies_for_url(u.clone())
                    .ok()
                    .and_then(|cs| cs.into_iter().find(|c| c.name() == "PHPSESSID").map(|c| c.value().to_string()));
                let Some(sid) = sid else {
                    done.store(false, std::sync::atomic::Ordering::SeqCst);
                    let _ = app.emit("edupage-browser-login", serde_json::json!({"ok": false, "message": "Logged in, but no session cookie was found."}));
                    return;
                };
                let _ = win.close();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<Arc<AppState>>().inner().clone();
                    let r = finish_browser_login(&state, &sid).await;
                    let payload = match &r {
                        Ok(()) => serde_json::json!({"ok": true}),
                        Err(e) => serde_json::json!({"ok": false, "message": e.to_string()}),
                    };
                    let _ = app.emit("edupage-browser-login", payload);
                    if r.is_ok() {
                        let _ = services::sync(&app, &state).await;
                    }
                });
            });
        })
        .build()
        .map_err(|e| AppError::Internal(format!("could not open login window: {e}")))?;
    Ok(())
}

async fn finish_browser_login(state: &AppState, sid: &str) -> AppResult<()> {
    let mut s = state.settings()?;
    let mut client = EdupageClient::new(&s.edupage.subdomain)?;
    client.resume(sid).await?;
    if s.edupage.username.is_empty() {
        s.edupage.username = client.user_id().unwrap_or_else(|| "edupage-user".into());
        s.save(&state.db)?;
    }
    state.secrets.set(&edupage::session_key(&s.edupage.subdomain, &s.edupage.username), sid)?;
    *state.edupage.lock().await = Some(client);
    Ok(())
}

#[tauri::command]
pub async fn edupage_logout(state: S<'_>, forget_password: bool) -> AppResult<()> {
    let s = state.settings()?;
    if let Some(mut c) = state.edupage.lock().await.take() {
        c.logout().await;
    }
    state.secrets.delete(&edupage::session_key(&s.edupage.subdomain, &s.edupage.username))?;
    if forget_password {
        state.secrets.delete(&edupage::password_key(&s.edupage.subdomain, &s.edupage.username))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn sync_now(app: AppHandle, state: S<'_>) -> AppResult<SyncReport> {
    services::sync(&app, &state).await
}

#[tauri::command]
pub fn last_sync(state: S<'_>) -> AppResult<Option<SyncRun>> {
    state.db.last_sync()
}

// --------------------------------------------------------------- assignments

#[tauri::command]
pub fn list_assignments(state: S<'_>, filter: AssignmentFilter) -> AppResult<Vec<AssignmentSummary>> {
    state.db.list_assignments(filter)
}

#[derive(Serialize)]
pub struct WorkspaceInfo {
    pub root: String,
    pub project: String,
    pub materials: String,
    pub exists: bool,
}

#[derive(Serialize)]
pub struct AssignmentDetail {
    pub assignment: Assignment,
    pub attachments: Vec<Attachment>,
    pub builds: Vec<Build>,
    pub latest_events: Vec<BuildEvent>,
    pub submissions: Vec<Submission>,
    pub workspace: WorkspaceInfo,
    pub plan: Option<Plan>,
    pub running: bool,
}

#[tauri::command]
pub fn get_assignment(state: S<'_>, id: String) -> AppResult<AssignmentDetail> {
    let assignment = state.db.get_assignment(&id)?;
    let ws = state.workspace(&assignment)?;
    let builds = state.db.builds_for(&id)?;
    let latest_events = match builds.first() {
        Some(b) => state.db.build_events(b.id)?,
        None => vec![],
    };
    let plan = builds.iter().find_map(|b| b.plan_json.as_deref()).and_then(|p| serde_json::from_str(p).ok());
    let running = state.builds.lock().unwrap_or_else(|p| p.into_inner()).contains_key(&id);
    Ok(AssignmentDetail {
        attachments: state.db.attachments(&id)?,
        submissions: state.db.submissions_for(&id)?,
        workspace: WorkspaceInfo {
            root: ws.root.to_string_lossy().into(),
            project: ws.project_dir.to_string_lossy().into(),
            materials: ws.materials_dir.to_string_lossy().into(),
            exists: ws.project_dir.exists(),
        },
        assignment: state.db.get_assignment(&id)?,
        builds,
        latest_events,
        plan,
        running,
    })
}

#[tauri::command]
pub fn mark_seen(state: S<'_>, id: String) -> AppResult<()> {
    state.db.mark_seen(&id)
}

#[tauri::command]
pub fn raw_assignment_json(state: S<'_>, id: String) -> AppResult<Value> {
    Ok(serde_json::from_str(&state.db.raw_json(&id)?)?)
}

#[derive(Serialize)]
pub struct DownloadResult {
    pub downloaded: usize,
    pub warnings: Vec<String>,
}

#[tauri::command]
pub async fn download_materials(state: S<'_>, id: String) -> AppResult<DownloadResult> {
    let (downloaded, warnings) = services::download_materials(&state, &id).await?;
    Ok(DownloadResult { downloaded, warnings })
}

#[derive(Serialize)]
pub struct ProjectFile {
    pub path: String,
    pub size: u64,
}

#[tauri::command]
pub fn project_files(state: S<'_>, id: String) -> AppResult<Vec<ProjectFile>> {
    let a = state.db.get_assignment(&id)?;
    let ws = state.workspace(&a)?;
    if !ws.project_dir.exists() {
        return Ok(vec![]);
    }
    Ok(github::files::collect(&ws.project_dir)?.into_iter().map(|f| ProjectFile { path: f.rel_path, size: f.size }).collect())
}

#[tauri::command]
pub fn read_project_file(state: S<'_>, id: String, path: String) -> AppResult<String> {
    let a = state.db.get_assignment(&id)?;
    let ws = state.workspace(&a)?;
    let p = crate::util::resolve_within(&ws.project_dir, &path)?;
    let bytes = std::fs::read(p)?;
    if bytes.contains(&0) {
        return Ok("(binary file)".into());
    }
    Ok(crate::util::truncate_middle(&String::from_utf8_lossy(&bytes), 400_000))
}

#[tauri::command]
pub fn open_workspace(app: AppHandle, state: S<'_>, id: String, which: String) -> AppResult<()> {
    let a = state.db.get_assignment(&id)?;
    let ws = state.workspace(&a)?;
    let dir = match which.as_str() {
        "project" => ws.project_dir,
        "materials" => ws.materials_dir,
        "submission" => ws.root.join("submission"),
        _ => ws.root,
    };
    std::fs::create_dir_all(&dir)?;
    app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(|e| AppError::Internal(e.to_string()))
}

#[tauri::command]
pub fn open_attachment(app: AppHandle, state: S<'_>, id: String, attachment_id: i64) -> AppResult<()> {
    let att = state.db.attachments(&id)?.into_iter().find(|a| a.id == attachment_id).ok_or_else(|| AppError::NotFound("attachment".into()))?;
    match att.local_path.filter(|p| std::path::Path::new(p).exists()) {
        Some(p) => app.opener().reveal_item_in_dir(p).map_err(|e| AppError::Internal(e.to_string())),
        None => open_external(app, att.url),
    }
}

/// Open an https URL in the default browser.
#[tauri::command]
pub fn open_external(app: AppHandle, url: String) -> AppResult<()> {
    let u = url::Url::parse(&url).map_err(|_| AppError::InvalidInput("invalid URL".into()))?;
    if u.scheme() != "https" {
        return Err(AppError::Policy("Only https links can be opened".into()));
    }
    app.opener().open_url(u.as_str(), None::<&str>).map_err(|e| AppError::Internal(e.to_string()))
}

#[tauri::command]
pub fn open_edupage(app: AppHandle, state: S<'_>) -> AppResult<()> {
    let s = state.settings()?;
    if s.edupage.subdomain.is_empty() {
        return Err(AppError::InvalidInput("EduPage is not configured".into()));
    }
    open_external(app, format!("https://{}.edupage.org/user/", s.edupage.subdomain))
}

// --------------------------------------------------------------------- build

#[tauri::command]
pub fn start_build(app: AppHandle, state: S<'_>, id: String) -> AppResult<i64> {
    services::start_build(app, state.inner().clone(), id)
}

#[tauri::command]
pub fn cancel_build(state: S<'_>, id: String) -> AppResult<()> {
    services::cancel_build(&state, &id)
}

#[tauri::command]
pub fn build_events(state: S<'_>, build_id: i64) -> AppResult<Vec<BuildEvent>> {
    state.db.build_events(build_id)
}

#[tauri::command]
pub fn detect_toolchains() -> Vec<sandbox::Toolchain> {
    sandbox::detect_toolchains()
}

#[tauri::command]
pub fn ai_usage(state: S<'_>) -> AppResult<Vec<AiUsageSummary>> {
    state.db.ai_usage_summary()
}

// -------------------------------------------------------------------- github

#[derive(Serialize)]
pub struct GithubStatus {
    pub connected: bool,
    pub user: Option<GithubUser>,
    pub message: Option<String>,
}

#[tauri::command]
pub async fn github_status(state: S<'_>) -> AppResult<GithubStatus> {
    let Some(token) = state.secrets.get(&SecretKey::GithubToken)? else {
        return Ok(GithubStatus { connected: false, user: None, message: None });
    };
    match GitHub::new(&token)?.user().await {
        Ok(u) => Ok(GithubStatus { connected: true, user: Some(u), message: None }),
        Err(e) => Ok(GithubStatus { connected: false, user: None, message: Some(e.to_string()) }),
    }
}

async fn store_github_token(state: &AppState, token: &str) -> AppResult<GithubUser> {
    let user = GitHub::new(token)?.user().await?;
    state.secrets.set(&SecretKey::GithubToken, token)?;
    let mut s = state.settings()?;
    s.github.login = Some(user.login.clone());
    s.save(&state.db)?;
    Ok(user)
}

#[tauri::command]
pub async fn github_connect_token(state: S<'_>, token: String) -> AppResult<GithubUser> {
    let token = token.trim();
    if token.is_empty() {
        return Err(AppError::InvalidInput("Token is empty".into()));
    }
    store_github_token(&state, token).await
}

#[tauri::command]
pub async fn github_device_start(state: S<'_>) -> AppResult<DeviceCode> {
    let s = state.settings()?;
    if s.github.oauth_client_id.trim().is_empty() {
        return Err(AppError::InvalidInput("Set an OAuth App client id first (or use a personal access token)".into()));
    }
    github::device_start(s.github.oauth_client_id.trim()).await
}

#[derive(Serialize)]
pub struct DevicePollResult {
    pub poll: DevicePoll,
    pub user: Option<GithubUser>,
}

#[tauri::command]
pub async fn github_device_poll(state: S<'_>, device_code: String) -> AppResult<DevicePollResult> {
    let s = state.settings()?;
    let (poll, token) = github::device_poll(s.github.oauth_client_id.trim(), &device_code).await?;
    let user = match token {
        Some(t) => Some(store_github_token(&state, &t).await?),
        None => None,
    };
    Ok(DevicePollResult { poll, user })
}

#[tauri::command]
pub fn github_disconnect(state: S<'_>) -> AppResult<()> {
    state.secrets.delete(&SecretKey::GithubToken)?;
    let mut s = state.settings()?;
    s.github.login = None;
    s.save(&state.db)
}

#[tauri::command]
pub async fn publish_to_github(state: S<'_>, id: String, private: Option<bool>) -> AppResult<PublishResult> {
    services::publish(&state, &id, private).await
}

#[tauri::command]
pub fn list_repositories(state: S<'_>) -> AppResult<Vec<RepositoryRow>> {
    state.db.list_repositories()
}

// ---------------------------------------------------------------- submission

#[tauri::command]
pub async fn prepare_submission(state: S<'_>, id: String) -> AppResult<SubmissionPreview> {
    services::prepare_submission(&state, &id).await
}

#[tauri::command]
pub async fn submit_assignment(state: S<'_>, id: String, token: String, method: String, message: String) -> AppResult<SubmitResult> {
    services::submit(&state, &id, &token, &method, &message).await
}

#[tauri::command]
pub fn record_manual_submission(state: S<'_>, id: String, token: String, note: String) -> AppResult<()> {
    services::record_manual(&state, &id, &token, &note)
}

// ----------------------------------------------------------------- dashboard

#[derive(Serialize)]
pub struct Dashboard {
    pub upcoming: Vec<AssignmentSummary>,
    pub recent: Vec<AssignmentSummary>,
    pub attention: Vec<AssignmentSummary>,
    pub builds: Vec<BuildActivity>,
    pub submissions: Vec<Submission>,
    pub last_sync: Option<SyncRun>,
    pub counts: std::collections::BTreeMap<String, usize>,
}

#[tauri::command]
pub fn dashboard(state: S<'_>) -> AppResult<Dashboard> {
    let all = state.db.list_assignments(AssignmentFilter::All)?;
    let now = chrono::Local::now().naive_local();
    let parse = |d: &Option<String>| d.as_deref().and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok());
    let mut counts = std::collections::BTreeMap::new();
    for a in &all {
        *counts.entry(a.status.clone()).or_insert(0) += 1;
    }
    let open: Vec<&AssignmentSummary> = all.iter().filter(|a| a.status != "submitted").collect();
    let upcoming = open.iter().filter(|a| parse(&a.due_at).is_some_and(|d| d >= now && d <= now + chrono::Duration::days(14))).take(8).map(|a| (*a).clone()).collect();
    let attention = open
        .iter()
        .filter(|a| {
            a.status == "build_failed"
                || a.changed_since_seen
                || parse(&a.due_at).is_some_and(|d| d < now)
                || (a.status == "new" && parse(&a.due_at).is_some_and(|d| d <= now + chrono::Duration::days(3)))
        })
        .take(8)
        .map(|a| (*a).clone())
        .collect();
    let mut recent = all.clone();
    recent.sort_by(|a, b| b.first_seen_at.cmp(&a.first_seen_at));
    recent.truncate(6);
    Ok(Dashboard {
        upcoming,
        recent,
        attention,
        builds: state.db.recent_builds(8)?,
        submissions: state.db.recent_submissions(6)?,
        last_sync: state.db.last_sync()?,
        counts,
    })
}

#[derive(Serialize)]
pub struct AppInfo {
    pub version: String,
    pub data_dir: String,
    pub log_dir: String,
}

#[tauri::command]
pub fn app_info(app: AppHandle) -> AppInfo {
    let p = app.path();
    AppInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        data_dir: p.app_data_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default(),
        log_dir: p.app_log_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default(),
    }
}
