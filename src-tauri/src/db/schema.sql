-- EduTask schema, migration 1.
-- Secrets (passwords, tokens, API keys) are NEVER stored here; see secrets.rs.

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS assignments (
    id                 TEXT PRIMARY KEY NOT NULL,           -- "{platform}:{external_id}"
    platform           TEXT NOT NULL,
    external_id        TEXT NOT NULL,
    title              TEXT NOT NULL,
    subject            TEXT,
    teacher            TEXT,
    teacher_ref        TEXT,                                 -- platform user id of the teacher, if known
    description_html   TEXT NOT NULL DEFAULT '',
    description_text   TEXT NOT NULL DEFAULT '',
    due_at             TEXT,                                 -- ISO-8601
    assigned_at        TEXT,
    content_hash       TEXT NOT NULL,
    raw_json           TEXT NOT NULL,
    status             TEXT NOT NULL DEFAULT 'new'
        CHECK (status IN ('new','not_started','building','build_failed','ready_to_submit','submitted')),
    platform_done      INTEGER NOT NULL DEFAULT 0 CHECK (platform_done IN (0,1)),
    changed_since_seen INTEGER NOT NULL DEFAULT 0 CHECK (changed_since_seen IN (0,1)),
    removed            INTEGER NOT NULL DEFAULT 0 CHECK (removed IN (0,1)),
    workspace_path     TEXT,
    repo_url           TEXT,
    repo_full_name     TEXT,
    first_seen_at      TEXT NOT NULL,
    last_synced_at     TEXT NOT NULL,
    updated_at         TEXT NOT NULL,
    UNIQUE (platform, external_id)
) STRICT;
CREATE INDEX IF NOT EXISTS idx_assignments_due ON assignments(due_at);
CREATE INDEX IF NOT EXISTS idx_assignments_status ON assignments(status);

CREATE TABLE IF NOT EXISTS attachments (
    id             INTEGER PRIMARY KEY,
    assignment_id  TEXT NOT NULL REFERENCES assignments(id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    url            TEXT NOT NULL,
    local_path     TEXT,
    size_bytes     INTEGER,
    downloaded_at  TEXT,
    error          TEXT,
    UNIQUE (assignment_id, url)
) STRICT;
CREATE INDEX IF NOT EXISTS idx_attachments_assignment ON attachments(assignment_id);

CREATE TABLE IF NOT EXISTS builds (
    id             INTEGER PRIMARY KEY,
    assignment_id  TEXT NOT NULL REFERENCES assignments(id) ON DELETE CASCADE,
    started_at     TEXT NOT NULL,
    finished_at    TEXT,
    outcome        TEXT NOT NULL DEFAULT 'running'
        CHECK (outcome IN ('running','succeeded','failed','cancelled')),
    plan_json      TEXT,
    summary        TEXT,
    error          TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS idx_builds_assignment ON builds(assignment_id, id DESC);

CREATE TABLE IF NOT EXISTS build_events (
    id        INTEGER PRIMARY KEY,
    build_id  INTEGER NOT NULL REFERENCES builds(id) ON DELETE CASCADE,
    ts        TEXT NOT NULL,
    stage     TEXT NOT NULL,
    level     TEXT NOT NULL CHECK (level IN ('info','success','warn','error','output')),
    message   TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS idx_build_events_build ON build_events(build_id, id);

CREATE TABLE IF NOT EXISTS submissions (
    id             INTEGER PRIMARY KEY,
    assignment_id  TEXT NOT NULL REFERENCES assignments(id) ON DELETE CASCADE,
    created_at     TEXT NOT NULL,
    method         TEXT NOT NULL,
    state          TEXT NOT NULL CHECK (state IN ('confirmed','failed','self_reported')),
    details_json   TEXT NOT NULL,
    platform_ref   TEXT,
    error          TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS idx_submissions_assignment ON submissions(assignment_id, id DESC);

CREATE TABLE IF NOT EXISTS ai_usage (
    id                  INTEGER PRIMARY KEY,
    ts                  TEXT NOT NULL,
    provider            TEXT NOT NULL,
    model               TEXT NOT NULL,
    input_tokens        INTEGER NOT NULL,
    output_tokens       INTEGER NOT NULL,
    cache_read_tokens   INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens  INTEGER NOT NULL DEFAULT 0,
    assignment_id       TEXT REFERENCES assignments(id) ON DELETE SET NULL
) STRICT;

CREATE TABLE IF NOT EXISTS sync_runs (
    id             INTEGER PRIMARY KEY,
    started_at     TEXT NOT NULL,
    finished_at    TEXT,
    ok             INTEGER NOT NULL DEFAULT 0 CHECK (ok IN (0,1)),
    new_count      INTEGER NOT NULL DEFAULT 0,
    changed_count  INTEGER NOT NULL DEFAULT 0,
    error          TEXT
) STRICT;
