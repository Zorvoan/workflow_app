//! EduTask backend.
//!
//! Module map:
//! * `edupage`    – EduPage integration (login, sync, attachments, messages)
//! * `engine`     – BUILD ASSIGNMENT pipeline (analysis, generation, sandboxed build/test)
//! * `ai`         – provider-neutral AI interface (Anthropic, OpenAI-compatible)
//! * `github`     – repository creation and push, .gitignore, secret scanning
//! * `submission` – packaging, confirmation tokens, delivery
//! * `db`, `secrets`, `settings` – persistence and credential storage
//! * `services`, `commands` – application glue and the frontend API

pub mod ai;
pub mod commands;
pub mod db;
pub mod edupage;
pub mod engine;
pub mod error;
pub mod github;
pub mod secrets;
pub mod services;
pub mod settings;
pub mod submission;
pub mod util;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tauri::Manager;

static LOG_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

fn init_logging(dir: &std::path::Path) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let _ = std::fs::create_dir_all(dir);
    let (writer, guard) = tracing_appender::non_blocking(tracing_appender::rolling::daily(dir, "edutask.log"));
    let _ = LOG_GUARD.set(guard);
    let filter = EnvFilter::try_from_env("EDUTASK_LOG").unwrap_or_else(|_| EnvFilter::new("info,edutask_lib=debug,hyper=warn,reqwest=warn"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .with(fmt::layer().with_writer(std::io::stderr))
        .try_init();
}

/// Background synchronization at the configured interval.
fn spawn_scheduler(app: tauri::AppHandle, state: Arc<services::AppState>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(8)).await;
        let mut last: Option<std::time::Instant> = None;
        loop {
            let s = state.settings().unwrap_or_default();
            let interval = s.edupage.sync_interval_minutes;
            let configured = !s.edupage.subdomain.is_empty() && !s.edupage.username.is_empty();
            let due = last.is_none_or(|t| t.elapsed() >= Duration::from_secs(interval.max(5) as u64 * 60));
            if configured && interval > 0 && due {
                last = Some(std::time::Instant::now());
                if let Err(e) = services::sync(&app, &state).await {
                    tracing::warn!("scheduled sync failed: {e}");
                }
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let log_dir = app.path().app_log_dir()?;
            init_logging(&log_dir);
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db = db::Db::open(&data_dir.join("edutask.db")).map_err(|e| format!("database: {e}"))?;
            let state = Arc::new(services::AppState::new(db, Arc::new(secrets::KeyringStore)));
            app.manage(state.clone());
            spawn_scheduler(app.handle().clone(), state);
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "EduTask started");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::secret_status,
            commands::set_ai_key,
            commands::edupage_status,
            commands::edupage_login,
            commands::edupage_browser_login,
            commands::edupage_logout,
            commands::sync_now,
            commands::last_sync,
            commands::list_assignments,
            commands::get_assignment,
            commands::mark_seen,
            commands::raw_assignment_json,
            commands::download_materials,
            commands::project_files,
            commands::read_project_file,
            commands::open_workspace,
            commands::open_attachment,
            commands::open_external,
            commands::open_edupage,
            commands::start_build,
            commands::cancel_build,
            commands::build_events,
            commands::detect_toolchains,
            commands::ai_usage,
            commands::github_status,
            commands::github_connect_token,
            commands::github_device_start,
            commands::github_device_poll,
            commands::github_disconnect,
            commands::publish_to_github,
            commands::list_repositories,
            commands::prepare_submission,
            commands::submit_assignment,
            commands::record_manual_submission,
            commands::dashboard,
            commands::app_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running EduTask");
}
