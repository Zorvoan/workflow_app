//! SQLite persistence. All SQL lives in this module.
//!
//! The connection is wrapped in a `std::sync::Mutex`; every access is a short,
//! synchronous critical section (no `.await` while holding the lock).

pub mod models;

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{AppError, AppResult};
use models::*;

const SCHEMA_V1: &str = include_str!("schema.sql");

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Db {
    pub fn open(path: &Path) -> AppResult<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> AppResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> AppResult<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(SCHEMA_V1)?;
            conn.pragma_update(None, "user_version", 1)?;
        }
        // A build that was running when the app quit can never finish.
        conn.execute(
            "UPDATE builds SET outcome='cancelled', finished_at=?1, error='Application closed during build' WHERE outcome='running'",
            params![now()],
        )?;
        conn.execute(
            "UPDATE assignments SET status='build_failed' WHERE status='building'",
            [],
        )?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A poisoned lock means a panic mid-query; the connection itself is still usable.
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ---------------------------------------------------------------- settings

    pub fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        Ok(self
            .lock()
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.lock().execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------- assignments

    /// Insert or update an assignment coming from a platform sync.
    /// Returns what happened so the sync can report new/changed counts.
    pub fn upsert_remote_assignment(&self, a: &RemoteAssignmentRow) -> AppResult<UpsertOutcome> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let ts = now();
        let existing: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT id, content_hash, removed FROM assignments WHERE platform=?1 AND external_id=?2",
                params![a.platform, a.external_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let outcome = match existing {
            None => {
                tx.execute(
                    "INSERT INTO assignments(id,platform,external_id,title,subject,teacher,teacher_ref,description_html,description_text,due_at,assigned_at,content_hash,raw_json,platform_done,first_seen_at,last_synced_at,updated_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15,?15)",
                    params![
                        a.id(), a.platform, a.external_id, a.title, a.subject, a.teacher, a.teacher_ref,
                        a.description_html, a.description_text, a.due_at, a.assigned_at, a.content_hash,
                        a.raw_json, a.platform_done as i64, ts
                    ],
                )?;
                UpsertOutcome::New
            }
            Some((id, hash, removed)) => {
                let changed = hash != a.content_hash;
                tx.execute(
                    "UPDATE assignments SET title=?2, subject=?3, teacher=?4, teacher_ref=?5, description_html=?6,
                        description_text=?7, due_at=?8, assigned_at=?9, content_hash=?10, raw_json=?11, platform_done=?12,
                        removed=0, last_synced_at=?13,
                        changed_since_seen = CASE WHEN ?14 THEN 1 ELSE changed_since_seen END,
                        updated_at = CASE WHEN ?14 THEN ?13 ELSE updated_at END
                     WHERE id=?1",
                    params![
                        id, a.title, a.subject, a.teacher, a.teacher_ref, a.description_html, a.description_text,
                        a.due_at, a.assigned_at, a.content_hash, a.raw_json, a.platform_done as i64, ts, changed
                    ],
                )?;
                if changed || removed == 1 { UpsertOutcome::Changed } else { UpsertOutcome::Unchanged }
            }
        };
        // Attachments: add new ones; never delete downloaded files on sync.
        for att in &a.attachments {
            tx.execute(
                "INSERT INTO attachments(assignment_id,name,url) VALUES(?1,?2,?3)
                 ON CONFLICT(assignment_id,url) DO UPDATE SET name=excluded.name",
                params![a.id(), att.name, att.url],
            )?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    /// Flag assignments of `platform` that no longer appear in the remote list.
    /// Only call this with a list that is known to be complete.
    pub fn mark_missing_removed(&self, platform: &str, seen_ids: &[String]) -> AppResult<usize> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id FROM assignments WHERE platform=?1 AND removed=0")?;
        let ids: Vec<String> = stmt.query_map([platform], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let mut n = 0;
        for id in ids.iter().filter(|id| !seen_ids.contains(id)) {
            n += conn.execute("UPDATE assignments SET removed=1 WHERE id=?1", [id])?;
        }
        Ok(n)
    }

    pub fn list_assignments(&self, filter: AssignmentFilter) -> AppResult<Vec<AssignmentSummary>> {
        let where_clause = match filter {
            AssignmentFilter::All => "removed=0",
            AssignmentFilter::InProgress => "removed=0 AND status IN ('building','build_failed','ready_to_submit')",
            AssignmentFilter::Completed => "removed=0 AND status='submitted'",
            AssignmentFilter::Open => "removed=0 AND status!='submitted'",
        };
        let sql = format!(
            "SELECT a.id,a.title,a.subject,a.teacher,a.due_at,a.status,a.changed_since_seen,a.repo_url,a.platform_done,a.last_synced_at,a.first_seen_at,
                    (SELECT COUNT(*) FROM attachments t WHERE t.assignment_id=a.id)
             FROM assignments a WHERE {where_clause}
             ORDER BY CASE WHEN a.due_at IS NULL THEN 1 ELSE 0 END, a.due_at ASC, a.first_seen_at DESC"
        );
        let conn = self.lock();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], AssignmentSummary::from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_assignment(&self, id: &str) -> AppResult<Assignment> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id,platform,external_id,title,subject,teacher,teacher_ref,description_html,description_text,due_at,assigned_at,
                    status,platform_done,changed_since_seen,removed,workspace_path,repo_url,repo_full_name,first_seen_at,last_synced_at,updated_at
             FROM assignments WHERE id=?1",
            [id],
            Assignment::from_row,
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("assignment {id} not found")))
    }

    pub fn raw_json(&self, id: &str) -> AppResult<String> {
        Ok(self.lock().query_row("SELECT raw_json FROM assignments WHERE id=?1", [id], |r| r.get(0))?)
    }

    pub fn set_status(&self, id: &str, status: Status) -> AppResult<()> {
        self.lock().execute(
            "UPDATE assignments SET status=?2, updated_at=?3 WHERE id=?1",
            params![id, status.as_str(), now()],
        )?;
        Ok(())
    }

    /// Opening an assignment acknowledges it: `new` becomes `not_started`, change flag clears.
    pub fn mark_seen(&self, id: &str) -> AppResult<()> {
        self.lock().execute(
            "UPDATE assignments SET status = CASE WHEN status='new' THEN 'not_started' ELSE status END, changed_since_seen=0 WHERE id=?1",
            [id],
        )?;
        Ok(())
    }

    pub fn set_workspace(&self, id: &str, path: &str) -> AppResult<()> {
        self.lock().execute("UPDATE assignments SET workspace_path=?2 WHERE id=?1", params![id, path])?;
        Ok(())
    }

    pub fn set_repo(&self, id: &str, url: &str, full_name: &str) -> AppResult<()> {
        self.lock().execute(
            "UPDATE assignments SET repo_url=?2, repo_full_name=?3, updated_at=?4 WHERE id=?1",
            params![id, url, full_name, now()],
        )?;
        Ok(())
    }

    pub fn list_repositories(&self) -> AppResult<Vec<RepositoryRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id,title,subject,repo_url,repo_full_name,updated_at FROM assignments WHERE repo_url IS NOT NULL ORDER BY updated_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RepositoryRow {
                    assignment_id: r.get(0)?,
                    title: r.get(1)?,
                    subject: r.get(2)?,
                    url: r.get(3)?,
                    full_name: r.get(4)?,
                    updated_at: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------------- attachments

    pub fn attachments(&self, assignment_id: &str) -> AppResult<Vec<Attachment>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id,name,url,local_path,size_bytes,downloaded_at,error FROM attachments WHERE assignment_id=?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([assignment_id], |r| {
                Ok(Attachment {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    url: r.get(2)?,
                    local_path: r.get(3)?,
                    size_bytes: r.get(4)?,
                    downloaded_at: r.get(5)?,
                    error: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn attachment_downloaded(&self, id: i64, local_path: &str, size: i64) -> AppResult<()> {
        self.lock().execute(
            "UPDATE attachments SET local_path=?2, size_bytes=?3, downloaded_at=?4, error=NULL WHERE id=?1",
            params![id, local_path, size, now()],
        )?;
        Ok(())
    }

    pub fn attachment_failed(&self, id: i64, error: &str) -> AppResult<()> {
        self.lock().execute("UPDATE attachments SET error=?2 WHERE id=?1", params![id, error])?;
        Ok(())
    }

    // ------------------------------------------------------------------ builds

    pub fn create_build(&self, assignment_id: &str) -> AppResult<i64> {
        let conn = self.lock();
        let running: i64 = conn.query_row(
            "SELECT COUNT(*) FROM builds WHERE assignment_id=?1 AND outcome='running'",
            [assignment_id],
            |r| r.get(0),
        )?;
        if running > 0 {
            return Err(AppError::Conflict("A build is already running for this assignment".into()));
        }
        conn.execute(
            "INSERT INTO builds(assignment_id,started_at) VALUES(?1,?2)",
            params![assignment_id, now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn finish_build(&self, build_id: i64, outcome: BuildOutcome, summary: Option<&str>, error: Option<&str>) -> AppResult<()> {
        self.lock().execute(
            "UPDATE builds SET outcome=?2, finished_at=?3, summary=?4, error=?5 WHERE id=?1",
            params![build_id, outcome.as_str(), now(), summary, error],
        )?;
        Ok(())
    }

    pub fn set_build_plan(&self, build_id: i64, plan_json: &str) -> AppResult<()> {
        self.lock().execute("UPDATE builds SET plan_json=?2 WHERE id=?1", params![build_id, plan_json])?;
        Ok(())
    }

    pub fn add_build_event(&self, build_id: i64, stage: &str, level: &str, message: &str) -> AppResult<BuildEvent> {
        let conn = self.lock();
        let ts = now();
        conn.execute(
            "INSERT INTO build_events(build_id,ts,stage,level,message) VALUES(?1,?2,?3,?4,?5)",
            params![build_id, ts, stage, level, message],
        )?;
        Ok(BuildEvent { id: conn.last_insert_rowid(), build_id, ts, stage: stage.into(), level: level.into(), message: message.into() })
    }

    pub fn builds_for(&self, assignment_id: &str) -> AppResult<Vec<Build>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id,assignment_id,started_at,finished_at,outcome,plan_json,summary,error FROM builds WHERE assignment_id=?1 ORDER BY id DESC LIMIT 20",
        )?;
        let rows = stmt.query_map([assignment_id], Build::from_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn latest_successful_plan(&self, assignment_id: &str) -> AppResult<Option<String>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT plan_json FROM builds WHERE assignment_id=?1 AND outcome='succeeded' AND plan_json IS NOT NULL ORDER BY id DESC LIMIT 1",
                [assignment_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn build_events(&self, build_id: i64) -> AppResult<Vec<BuildEvent>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id,build_id,ts,stage,level,message FROM build_events WHERE build_id=?1 ORDER BY id")?;
        let rows = stmt
            .query_map([build_id], |r| {
                Ok(BuildEvent { id: r.get(0)?, build_id: r.get(1)?, ts: r.get(2)?, stage: r.get(3)?, level: r.get(4)?, message: r.get(5)? })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn recent_builds(&self, limit: i64) -> AppResult<Vec<BuildActivity>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT b.id,b.assignment_id,a.title,b.started_at,b.finished_at,b.outcome FROM builds b JOIN assignments a ON a.id=b.assignment_id ORDER BY b.id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit], |r| {
                Ok(BuildActivity {
                    build_id: r.get(0)?,
                    assignment_id: r.get(1)?,
                    title: r.get(2)?,
                    started_at: r.get(3)?,
                    finished_at: r.get(4)?,
                    outcome: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------------- submissions

    pub fn add_submission(&self, s: &NewSubmission) -> AppResult<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO submissions(assignment_id,created_at,method,state,details_json,platform_ref,error) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![s.assignment_id, now(), s.method, s.state.as_str(), s.details_json, s.platform_ref, s.error],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn submissions_for(&self, assignment_id: &str) -> AppResult<Vec<Submission>> {
        self.query_submissions("WHERE s.assignment_id=?1", params![assignment_id], 50)
    }

    pub fn recent_submissions(&self, limit: i64) -> AppResult<Vec<Submission>> {
        self.query_submissions("", params![], limit)
    }

    fn query_submissions(&self, where_clause: &str, p: &[&dyn rusqlite::ToSql], limit: i64) -> AppResult<Vec<Submission>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT s.id,s.assignment_id,a.title,s.created_at,s.method,s.state,s.details_json,s.platform_ref,s.error
             FROM submissions s JOIN assignments a ON a.id=s.assignment_id {where_clause} ORDER BY s.id DESC LIMIT {limit}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(p, |r| {
                Ok(Submission {
                    id: r.get(0)?,
                    assignment_id: r.get(1)?,
                    title: r.get(2)?,
                    created_at: r.get(3)?,
                    method: r.get(4)?,
                    state: r.get(5)?,
                    details_json: r.get(6)?,
                    platform_ref: r.get(7)?,
                    error: r.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---------------------------------------------------------------- ai usage

    pub fn record_ai_usage(&self, provider: &str, model: &str, u: &crate::ai::Usage, assignment_id: Option<&str>) -> AppResult<()> {
        self.lock().execute(
            "INSERT INTO ai_usage(ts,provider,model,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,assignment_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![now(), provider, model, u.input_tokens, u.output_tokens, u.cache_read_tokens, u.cache_write_tokens, assignment_id],
        )?;
        Ok(())
    }

    pub fn ai_usage_summary(&self) -> AppResult<Vec<AiUsageSummary>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT provider, model, COUNT(*), SUM(input_tokens), SUM(output_tokens), SUM(cache_read_tokens), SUM(cache_write_tokens)
             FROM ai_usage WHERE ts >= ?1 GROUP BY provider, model ORDER BY SUM(output_tokens) DESC",
        )?;
        let since = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let rows = stmt
            .query_map([since], |r| {
                Ok(AiUsageSummary {
                    provider: r.get(0)?,
                    model: r.get(1)?,
                    requests: r.get(2)?,
                    input_tokens: r.get(3)?,
                    output_tokens: r.get(4)?,
                    cache_read_tokens: r.get(5)?,
                    cache_write_tokens: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------------------- sync

    pub fn start_sync_run(&self) -> AppResult<i64> {
        let conn = self.lock();
        conn.execute("INSERT INTO sync_runs(started_at) VALUES(?1)", [now()])?;
        Ok(conn.last_insert_rowid())
    }

    pub fn finish_sync_run(&self, id: i64, ok: bool, new_count: i64, changed: i64, error: Option<&str>) -> AppResult<()> {
        self.lock().execute(
            "UPDATE sync_runs SET finished_at=?2, ok=?3, new_count=?4, changed_count=?5, error=?6 WHERE id=?1",
            params![id, now(), ok as i64, new_count, changed, error],
        )?;
        Ok(())
    }

    pub fn last_sync(&self) -> AppResult<Option<SyncRun>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT id,started_at,finished_at,ok,new_count,changed_count,error FROM sync_runs WHERE finished_at IS NOT NULL ORDER BY id DESC LIMIT 1",
                [],
                |r| {
                    Ok(SyncRun {
                        id: r.get(0)?,
                        started_at: r.get(1)?,
                        finished_at: r.get(2)?,
                        ok: r.get::<_, i64>(3)? == 1,
                        new_count: r.get(4)?,
                        changed_count: r.get(5)?,
                        error: r.get(6)?,
                    })
                },
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(hash: &str) -> RemoteAssignmentRow {
        RemoteAssignmentRow {
            platform: "edupage".into(),
            external_id: "42".into(),
            title: "Calculator".into(),
            subject: Some("Programming".into()),
            teacher: Some("J. Novak".into()),
            teacher_ref: Some("Ucitel7".into()),
            description_html: "<p>Write a calculator</p>".into(),
            description_text: "Write a calculator".into(),
            due_at: Some("2026-12-01T23:59:00".into()),
            assigned_at: None,
            content_hash: hash.into(),
            raw_json: "{}".into(),
            platform_done: false,
            attachments: vec![RemoteAttachment { name: "spec.pdf".into(), url: "https://x.edupage.org/f/1".into() }],
        }
    }

    #[test]
    fn upsert_detects_new_changed_unchanged() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.upsert_remote_assignment(&sample("a")).unwrap(), UpsertOutcome::New);
        assert_eq!(db.upsert_remote_assignment(&sample("a")).unwrap(), UpsertOutcome::Unchanged);
        db.mark_seen("edupage:42").unwrap();
        assert_eq!(db.upsert_remote_assignment(&sample("b")).unwrap(), UpsertOutcome::Changed);
        let a = db.get_assignment("edupage:42").unwrap();
        assert!(a.changed_since_seen);
        assert_eq!(a.status, "not_started");
        assert_eq!(db.attachments("edupage:42").unwrap().len(), 1);
    }

    #[test]
    fn status_check_constraint_rejects_garbage() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_remote_assignment(&sample("a")).unwrap();
        let r = db.lock().execute("UPDATE assignments SET status='done-ish' WHERE id='edupage:42'", []);
        assert!(r.is_err());
    }

    #[test]
    fn only_one_running_build() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_remote_assignment(&sample("a")).unwrap();
        let b = db.create_build("edupage:42").unwrap();
        assert!(matches!(db.create_build("edupage:42"), Err(AppError::Conflict(_))));
        db.finish_build(b, BuildOutcome::Failed, None, Some("x")).unwrap();
        assert!(db.create_build("edupage:42").is_ok());
    }

    #[test]
    fn missing_assignments_are_flagged_not_deleted() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_remote_assignment(&sample("a")).unwrap();
        assert_eq!(db.mark_missing_removed("edupage", &[]).unwrap(), 1);
        assert!(db.list_assignments(AssignmentFilter::All).unwrap().is_empty());
        assert!(db.get_assignment("edupage:42").unwrap().removed);
    }
}
