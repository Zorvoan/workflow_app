// Typed wrappers around the Rust commands (src-tauri/src/commands.rs).
// Types mirror the serde structs; keep them in sync when the backend changes.
import { invoke } from "@tauri-apps/api/core";

export type Status = "new" | "not_started" | "building" | "build_failed" | "ready_to_submit" | "submitted";
export type Filter = "all" | "open" | "in_progress" | "completed";

export interface ApiError {
  kind: string;
  message: string;
}

export function errorMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as ApiError).message);
  return "Unexpected error";
}

export function errorKind(e: unknown): string | undefined {
  return e && typeof e === "object" && "kind" in e ? String((e as ApiError).kind) : undefined;
}

export interface AssignmentSummary {
  id: string;
  title: string;
  subject: string | null;
  teacher: string | null;
  due_at: string | null;
  status: Status;
  changed_since_seen: boolean;
  repo_url: string | null;
  platform_done: boolean;
  last_synced_at: string;
  first_seen_at: string;
  attachment_count: number;
}

export interface Assignment {
  id: string;
  platform: string;
  external_id: string;
  title: string;
  subject: string | null;
  teacher: string | null;
  teacher_ref: string | null;
  description_html: string;
  description_text: string;
  due_at: string | null;
  assigned_at: string | null;
  status: Status;
  platform_done: boolean;
  changed_since_seen: boolean;
  removed: boolean;
  workspace_path: string | null;
  repo_url: string | null;
  repo_full_name: string | null;
  first_seen_at: string;
  last_synced_at: string;
  updated_at: string;
}

export interface Attachment {
  id: number;
  name: string;
  url: string;
  local_path: string | null;
  size_bytes: number | null;
  downloaded_at: string | null;
  error: string | null;
}

export interface Build {
  id: number;
  assignment_id: string;
  started_at: string;
  finished_at: string | null;
  outcome: "running" | "succeeded" | "failed" | "cancelled";
  plan_json: string | null;
  summary: string | null;
  error: string | null;
}

export interface BuildEvent {
  id: number;
  build_id: number;
  ts: string;
  stage: string;
  level: "info" | "success" | "warn" | "error" | "output";
  message: string;
}

export interface Submission {
  id: number;
  assignment_id: string;
  title: string;
  created_at: string;
  method: string;
  state: "confirmed" | "failed" | "self_reported";
  details_json: string;
  platform_ref: string | null;
  error: string | null;
}

export interface CommandSpec {
  program: string;
  args: string[];
}

export interface Plan {
  kind: string;
  language: string;
  technology: string;
  summary: string;
  deliverables: string[];
  required_files: string[];
  expected_output: string;
  dependency_commands: CommandSpec[];
  build_commands: CommandSpec[];
  test_commands: CommandSpec[];
  submission_expects: string;
  submission_notes: string;
  ambiguities: string[];
  steps: string[];
}

export interface AssignmentDetail {
  assignment: Assignment;
  attachments: Attachment[];
  builds: Build[];
  latest_events: BuildEvent[];
  submissions: Submission[];
  workspace: { root: string; project: string; materials: string; exists: boolean };
  plan: Plan | null;
  running: boolean;
}

export interface SyncRun {
  id: number;
  started_at: string;
  finished_at: string | null;
  ok: boolean;
  new_count: number;
  changed_count: number;
  error: string | null;
}

export interface SyncReport {
  total: number;
  new: number;
  changed: number;
  attachments_downloaded: number;
  warnings: string[];
}

export interface BuildActivity {
  build_id: number;
  assignment_id: string;
  title: string;
  started_at: string;
  finished_at: string | null;
  outcome: Build["outcome"];
}

export interface Dashboard {
  upcoming: AssignmentSummary[];
  recent: AssignmentSummary[];
  attention: AssignmentSummary[];
  builds: BuildActivity[];
  submissions: Submission[];
  last_sync: SyncRun | null;
  counts: Record<string, number>;
}

export interface Settings {
  edupage: { subdomain: string; username: string; remember_password: boolean; sync_interval_minutes: number; history_days: number };
  github: { default_private: boolean; repo_name_template: string; auto_publish: boolean; oauth_client_id: string; login: string | null };
  ai: { provider: string; model: string; base_url: string; effort: string; max_fix_iterations: number; max_agent_turns: number };
  build: { projects_dir: string; command_timeout_secs: number; allow_dependency_install: boolean; enabled_languages: string[]; extra_gitignore: string };
  ui: { theme: "system" | "light" | "dark" };
}

export interface ConnectionStatus {
  connected: boolean;
  subdomain: string;
  username: string;
  user_id: string | null;
  has_saved_password: boolean;
  has_saved_session: boolean;
  message: string | null;
}

export type LoginOutcome =
  | { result: "logged_in"; user_id: string | null; subdomain: string }
  | { result: "two_factor_required" }
  | { result: "captcha_required" };

export interface GithubUser {
  login: string;
  name: string | null;
  avatar_url: string | null;
}

export interface DeviceCode {
  device_code: string;
  user_code: string;
  verification_uri: string;
  interval: number;
  expires_in: number;
}

export type DevicePoll = { state: "pending"; interval: number } | { state: "authorized" } | { state: "expired" } | { state: "denied" };

export interface RepositoryRow {
  assignment_id: string;
  title: string;
  subject: string | null;
  url: string;
  full_name: string | null;
  updated_at: string;
}

export interface Toolchain {
  name: string;
  path: string | null;
  note: string | null;
}

export interface AiUsage {
  provider: string;
  model: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
}

export interface MethodInfo {
  id: string;
  label: string;
  description: string;
  available: boolean;
  unavailable_reason: string | null;
  platform_confirmed: boolean;
}

export interface SubmissionPreview {
  assignment_id: string;
  title: string;
  teacher: string | null;
  repo_url: string | null;
  files: { name: string; path: string; size: number }[];
  methods: MethodInfo[];
  message: string;
  expectation: string | null;
  confirm_token: string;
  warnings: string[];
}

export interface PublishResult {
  url: string;
  full_name: string;
  commit: string;
  files: number;
  created: boolean;
}

export const api = {
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<Settings>("save_settings", { settings }),
  secretStatus: () => invoke<{ ai_keys: Record<string, boolean>; github: boolean }>("secret_status"),
  setAiKey: (provider: string, key: string) => invoke<void>("set_ai_key", { provider, key }),

  edupageStatus: () => invoke<ConnectionStatus>("edupage_status"),
  edupageLogin: (args: { subdomain: string; username: string; password: string; remember: boolean }) => invoke<LoginOutcome>("edupage_login", { args }),
  edupageBrowserLogin: (subdomain: string, username: string) => invoke<void>("edupage_browser_login", { subdomain, username }),
  edupageLogout: (forgetPassword: boolean) => invoke<void>("edupage_logout", { forgetPassword }),
  syncNow: () => invoke<SyncReport>("sync_now"),
  lastSync: () => invoke<SyncRun | null>("last_sync"),
  openEdupage: () => invoke<void>("open_edupage"),

  listAssignments: (filter: Filter) => invoke<AssignmentSummary[]>("list_assignments", { filter }),
  getAssignment: (id: string) => invoke<AssignmentDetail>("get_assignment", { id }),
  markSeen: (id: string) => invoke<void>("mark_seen", { id }),
  rawAssignmentJson: (id: string) => invoke<unknown>("raw_assignment_json", { id }),
  downloadMaterials: (id: string) => invoke<{ downloaded: number; warnings: string[] }>("download_materials", { id }),
  projectFiles: (id: string) => invoke<{ path: string; size: number }[]>("project_files", { id }),
  readProjectFile: (id: string, path: string) => invoke<string>("read_project_file", { id, path }),
  openWorkspace: (id: string, which: "root" | "project" | "materials" | "submission") => invoke<void>("open_workspace", { id, which }),
  openAttachment: (id: string, attachmentId: number) => invoke<void>("open_attachment", { id, attachmentId }),
  openExternal: (url: string) => invoke<void>("open_external", { url }),

  startBuild: (id: string) => invoke<number>("start_build", { id }),
  cancelBuild: (id: string) => invoke<void>("cancel_build", { id }),
  buildEvents: (buildId: number) => invoke<BuildEvent[]>("build_events", { buildId }),
  detectToolchains: () => invoke<Toolchain[]>("detect_toolchains"),
  aiUsage: () => invoke<AiUsage[]>("ai_usage"),

  githubStatus: () => invoke<{ connected: boolean; user: GithubUser | null; message: string | null }>("github_status"),
  githubConnectToken: (token: string) => invoke<GithubUser>("github_connect_token", { token }),
  githubDeviceStart: () => invoke<DeviceCode>("github_device_start"),
  githubDevicePoll: (deviceCode: string) => invoke<{ poll: DevicePoll; user: GithubUser | null }>("github_device_poll", { deviceCode }),
  githubDisconnect: () => invoke<void>("github_disconnect"),
  publishToGithub: (id: string, isPrivate: boolean | null) => invoke<PublishResult>("publish_to_github", { id, private: isPrivate }),
  listRepositories: () => invoke<RepositoryRow[]>("list_repositories"),

  prepareSubmission: (id: string) => invoke<SubmissionPreview>("prepare_submission", { id }),
  submitAssignment: (id: string, token: string, method: string, message: string) =>
    invoke<{ state: string; platform_ref: string | null; message: string }>("submit_assignment", { id, token, method, message }),
  recordManualSubmission: (id: string, token: string, note: string) => invoke<void>("record_manual_submission", { id, token, note }),

  dashboard: () => invoke<Dashboard>("dashboard"),
  appInfo: () => invoke<{ version: string; data_dir: string; log_dir: string }>("app_info"),
};
