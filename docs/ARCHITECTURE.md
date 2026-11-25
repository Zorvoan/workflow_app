# Architecture

```
React UI (src/)  ──invoke/events──▶  Tauri commands (commands.rs)
                                          │
                                     services.rs  (orchestration, AppState)
          ┌──────────────┬───────────────┼───────────────┬──────────────┐
      edupage/        engine/           ai/           github/       submission.rs
  login, sync,     analyze → generate  provider      repo, push,    packaging, tokens,
  attachments,     → build → test →    trait:        .gitignore,    delivery
  upload, message  fix (sandbox.rs)    anthropic,    secret scan
                                       openai-compat
          └──────────────┴──────── db/ (SQLite) · secrets.rs (Credential Manager) · settings.rs
```

**Why this stack:** Tauri 2 produces a small native Windows installer that uses WebView2. A Rust backend gives safe process control (timeouts, killing whole process trees) and direct access to Windows Credential Manager. SQLite is embedded, so nothing extra needs installing. There are no services or daemons. Browser automation (Playwright/Selenium) is not needed: plain HTTP covers sync, and the Tauri webview covers interactive login.

## Data model (`src-tauri/src/db/schema.sql`)

* `assignments`: one row per platform assignment (`platform:external_id`). `status` is one of `new`, `not_started`, `building`, `build_failed`, `ready_to_submit`, `submitted` (enforced by a CHECK constraint). It also stores `content_hash` (change detection), the workspace path and the repo.
* `attachments`: unique per (assignment, url), with local path and download error.
* `builds` and `build_events`: every pipeline run with its plan and the complete log.
* `submissions`: method plus state (`confirmed`, `failed`, `self_reported`) and the platform reference.
* `ai_usage`: token counts per request. `sync_runs`: sync history. `settings`: JSON with no secrets.

## Build pipeline (`engine/mod.rs`)

1. **Prepare**: workspace folders, `assignment.md`, attachment refresh. Materials are converted for the AI: PDFs and images sent natively, Office files as text, ZIPs listed but not extracted.
2. **Analyze**: structured-output JSON `Plan`, using the toolchains actually installed and the languages you allow.
3. **Generate**: tool loop (`list_files`, `read_file`, `write_file`, `delete_file`, `run_command`, `finish`), confined to `project/`. `materials/` is read-only.
4. **Dependencies → Build → Test**: EduTask runs the commands itself. Any non-zero exit, timeout or cancel counts as a failure.
5. **Fix**: the failure output goes back to the AI, up to N attempts.
6. **Verify**: required files must exist. `.gitignore` and `README.md` are ensured. A programming or database project with no runnable build/test command is reported as *not verified*, never as successful.

## Extending

* **AI provider**: implement `ai::AiProvider` and register it in `ai::create`.
* **School platform**: add a module that produces `RemoteAssignmentRow` values (see `edupage::parse`) and wire it into `services::sync`.
* **Language / build tool**: add the program and its subcommand rules to `engine/sandbox.rs::classify`, plus a `.gitignore` block in `github/gitignore.rs`.
