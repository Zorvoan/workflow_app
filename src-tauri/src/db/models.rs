//! Row types shared between the database, the services and the UI (via serde).

use rusqlite::Row;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    New,
    NotStarted,
    Building,
    BuildFailed,
    ReadyToSubmit,
    Submitted,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::New => "new",
            Status::NotStarted => "not_started",
            Status::Building => "building",
            Status::BuildFailed => "build_failed",
            Status::ReadyToSubmit => "ready_to_submit",
            Status::Submitted => "submitted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl BuildOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            BuildOutcome::Succeeded => "succeeded",
            BuildOutcome::Failed => "failed",
            BuildOutcome::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionState {
    /// The platform acknowledged the submission (we hold a platform reference).
    Confirmed,
    Failed,
    /// The user handed the work in themselves and told us so; not verified by the platform.
    SelfReported,
}

impl SubmissionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SubmissionState::Confirmed => "confirmed",
            SubmissionState::Failed => "failed",
            SubmissionState::SelfReported => "self_reported",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentFilter {
    All,
    Open,
    InProgress,
    Completed,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum UpsertOutcome {
    New,
    Changed,
    Unchanged,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteAttachment {
    pub name: String,
    pub url: String,
}

/// A normalized assignment as produced by a platform integration.
#[derive(Debug, Clone)]
pub struct RemoteAssignmentRow {
    pub platform: String,
    pub external_id: String,
    pub title: String,
    pub subject: Option<String>,
    pub teacher: Option<String>,
    pub teacher_ref: Option<String>,
    pub description_html: String,
    pub description_text: String,
    pub due_at: Option<String>,
    pub assigned_at: Option<String>,
    pub content_hash: String,
    pub raw_json: String,
    pub platform_done: bool,
    pub attachments: Vec<RemoteAttachment>,
}

impl RemoteAssignmentRow {
    pub fn id(&self) -> String {
        format!("{}:{}", self.platform, self.external_id)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AssignmentSummary {
    pub id: String,
    pub title: String,
    pub subject: Option<String>,
    pub teacher: Option<String>,
    pub due_at: Option<String>,
    pub status: String,
    pub changed_since_seen: bool,
    pub repo_url: Option<String>,
    pub platform_done: bool,
    pub last_synced_at: String,
    pub first_seen_at: String,
    pub attachment_count: i64,
}

impl AssignmentSummary {
    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            title: r.get(1)?,
            subject: r.get(2)?,
            teacher: r.get(3)?,
            due_at: r.get(4)?,
            status: r.get(5)?,
            changed_since_seen: r.get::<_, i64>(6)? == 1,
            repo_url: r.get(7)?,
            platform_done: r.get::<_, i64>(8)? == 1,
            last_synced_at: r.get(9)?,
            first_seen_at: r.get(10)?,
            attachment_count: r.get(11)?,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Assignment {
    pub id: String,
    pub platform: String,
    pub external_id: String,
    pub title: String,
    pub subject: Option<String>,
    pub teacher: Option<String>,
    pub teacher_ref: Option<String>,
    pub description_html: String,
    pub description_text: String,
    pub due_at: Option<String>,
    pub assigned_at: Option<String>,
    pub status: String,
    pub platform_done: bool,
    pub changed_since_seen: bool,
    pub removed: bool,
    pub workspace_path: Option<String>,
    pub repo_url: Option<String>,
    pub repo_full_name: Option<String>,
    pub first_seen_at: String,
    pub last_synced_at: String,
    pub updated_at: String,
}

impl Assignment {
    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            platform: r.get(1)?,
            external_id: r.get(2)?,
            title: r.get(3)?,
            subject: r.get(4)?,
            teacher: r.get(5)?,
            teacher_ref: r.get(6)?,
            description_html: r.get(7)?,
            description_text: r.get(8)?,
            due_at: r.get(9)?,
            assigned_at: r.get(10)?,
            status: r.get(11)?,
            platform_done: r.get::<_, i64>(12)? == 1,
            changed_since_seen: r.get::<_, i64>(13)? == 1,
            removed: r.get::<_, i64>(14)? == 1,
            workspace_path: r.get(15)?,
            repo_url: r.get(16)?,
            repo_full_name: r.get(17)?,
            first_seen_at: r.get(18)?,
            last_synced_at: r.get(19)?,
            updated_at: r.get(20)?,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Attachment {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub local_path: Option<String>,
    pub size_bytes: Option<i64>,
    pub downloaded_at: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Build {
    pub id: i64,
    pub assignment_id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: String,
    pub plan_json: Option<String>,
    pub summary: Option<String>,
    pub error: Option<String>,
}

impl Build {
    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            assignment_id: r.get(1)?,
            started_at: r.get(2)?,
            finished_at: r.get(3)?,
            outcome: r.get(4)?,
            plan_json: r.get(5)?,
            summary: r.get(6)?,
            error: r.get(7)?,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildEvent {
    pub id: i64,
    pub build_id: i64,
    pub ts: String,
    pub stage: String,
    pub level: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildActivity {
    pub build_id: i64,
    pub assignment_id: String,
    pub title: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: String,
}

pub struct NewSubmission {
    pub assignment_id: String,
    pub method: String,
    pub state: SubmissionState,
    pub details_json: String,
    pub platform_ref: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Submission {
    pub id: i64,
    pub assignment_id: String,
    pub title: String,
    pub created_at: String,
    pub method: String,
    pub state: String,
    pub details_json: String,
    pub platform_ref: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryRow {
    pub assignment_id: String,
    pub title: String,
    pub subject: Option<String>,
    pub url: String,
    pub full_name: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiUsageSummary {
    pub provider: String,
    pub model: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncRun {
    pub id: i64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub ok: bool,
    pub new_count: i64,
    pub changed_count: i64,
    pub error: Option<String>,
}
