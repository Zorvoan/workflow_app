import { useCallback, useEffect, useRef, useState } from "react";
import { useParams } from "react-router-dom";
import { listen } from "@tauri-apps/api/event";
import { api, errorMessage, type AssignmentDetail as Detail, type BuildEvent } from "../lib/api";
import { dueLabel, formatBytes, formatDateTime, relativeTime } from "../lib/format";
import { Banner, Empty, PageHeader, Section, StatusBadge } from "../components/ui";
import SubmitDialog from "../components/SubmitDialog";

const STAGES = ["prepare", "analyze", "generate", "dependencies", "build", "test", "fix", "verify", "done"];
const STAGE_LABEL: Record<string, string> = {
  prepare: "Reading assignment", analyze: "Analyzing", generate: "Writing files", dependencies: "Dependencies",
  build: "Building", test: "Testing", fix: "Fixing", verify: "Verifying", done: "Done",
};
type Tab = "overview" | "materials" | "files" | "log" | "submission";

function Progress({ events, running }: { events: BuildEvent[]; running: boolean }) {
  const seen = new Set(events.map((e) => e.stage));
  const last = events[events.length - 1];
  const failed = !running && last?.level === "error";
  return (
    <div className="progress">
      {STAGES.filter((s) => seen.has(s) || ["prepare", "analyze", "generate", "build", "verify", "done"].includes(s)).map((s) => {
        const cls = last?.stage === s ? (failed ? "failed" : running ? "current" : "done") : seen.has(s) ? "done" : "";
        return <span key={s} className={`step ${cls}`}>{STAGE_LABEL[s]}</span>;
      })}
    </div>
  );
}

function Log({ events }: { events: BuildEvent[] }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => { ref.current?.scrollTo({ top: ref.current.scrollHeight }); }, [events.length]);
  if (!events.length) return <Empty>No build has run yet.</Empty>;
  return (
    <div className="log" ref={ref}>
      {events.map((e) => (
        <div key={e.id} className={`log-line ${e.level}`}>
          <span className="ts">{new Date(e.ts).toLocaleTimeString()}</span>
          <span className="st">{e.stage}</span>
          <span className="m">{e.message}</span>
        </div>
      ))}
    </div>
  );
}

function Files({ id }: { id: string }) {
  const [files, setFiles] = useState<{ path: string; size: number }[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  const [content, setContent] = useState("");
  useEffect(() => { api.projectFiles(id).then((f) => { setFiles(f); if (f.length) setCurrent(f.find((x) => x.path === "README.md")?.path ?? f[0].path); }).catch(() => {}); }, [id]);
  useEffect(() => { if (current) api.readProjectFile(id, current).then(setContent).catch((e) => setContent(errorMessage(e))); }, [id, current]);
  if (!files.length) return <Empty>No generated files yet. Press BUILD ASSIGNMENT.</Empty>;
  return (
    <div className="file-viewer">
      <div className="file-list panel" style={{ padding: 4, maxHeight: 560, overflow: "auto" }}>
        {files.map((f) => (
          <button key={f.path} className={current === f.path ? "active" : ""} onClick={() => setCurrent(f.path)} title={`${f.path} (${formatBytes(f.size)})`}>{f.path}</button>
        ))}
      </div>
      <pre>{content}</pre>
    </div>
  );
}

export default function AssignmentDetail() {
  const id = decodeURIComponent(useParams().id ?? "");
  const [d, setD] = useState<Detail | null>(null);
  const [events, setEvents] = useState<BuildEvent[]>([]);
  const [tab, setTab] = useState<Tab>("overview");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [submitOpen, setSubmitOpen] = useState(false);
  const [makePublic, setMakePublic] = useState(false);

  const load = useCallback(() => {
    api.getAssignment(id).then((x) => { setD(x); setEvents(x.latest_events); }).catch((e) => setError(errorMessage(e)));
  }, [id]);

  useEffect(() => {
    api.markSeen(id).catch(() => {});
    load();
    const un = [
      listen<BuildEvent>("build-event", (e) => {
        setEvents((prev) => (prev.length && prev[0].build_id !== e.payload.build_id ? [e.payload] : prev.some((p) => p.id === e.payload.id) ? prev : [...prev, e.payload]));
      }),
      listen<{ assignment_id: string }>("build-finished", (e) => { if (e.payload.assignment_id === id) load(); }),
      listen("sync-finished", load),
    ];
    return () => { un.forEach((p) => p.then((f) => f())); };
  }, [id, load]);

  const run = async (label: string, fn: () => Promise<unknown>, ok?: (r: any) => string) => {
    setBusy(label);
    setError(null);
    setNotice(null);
    try {
      const r = await fn();
      if (ok) setNotice(ok(r));
      load();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  if (!d) return <div className="page">{error && <Banner tone="error">{error}</Banner>}</div>;
  const a = d.assignment;
  const due = dueLabel(a.due_at);
  const running = d.running || a.status === "building";
  const built = a.status === "ready_to_submit" || a.status === "submitted";
  const lastBuild = d.builds[0];

  const startBuild = () => run("build", async () => {
    setEvents([]);
    setTab("log");
    await api.startBuild(id);
  });

  return (
    <div className="page">
      <PageHeader
        title={a.title}
        subtitle={<span className="row-meta" style={{ fontSize: 13 }}>
          <StatusBadge status={a.status} />
          {a.subject && <span>{a.subject}</span>}
          {a.teacher && <span>{a.teacher}</span>}
          <span className={`tone-${due.tone}`}>{due.text}</span>
          {a.platform_done && <span>Marked done in EduPage</span>}
        </span>}
        actions={running ? (
          <button className="btn danger" onClick={() => run("cancel", () => api.cancelBuild(id))}>Cancel build</button>
        ) : (
          <button className="btn primary lg" onClick={startBuild} disabled={!!busy}>BUILD ASSIGNMENT</button>
        )}
      />
      {a.changed_since_seen && <Banner tone="warn">The teacher changed this assignment since you last opened it.</Banner>}
      {a.removed && <Banner tone="warn">This assignment no longer appears in EduPage.</Banner>}
      {error && <Banner tone="error">{error}</Banner>}
      {notice && <Banner tone="ok">{notice}</Banner>}
      {!running && lastBuild?.outcome === "failed" && lastBuild.error && <Banner tone="error">Last build failed: {lastBuild.error}</Banner>}

      {(running || events.length > 0) && <Progress events={events} running={running} />}

      <div className="tabs">
        {(["overview", "materials", "files", "log", "submission"] as Tab[]).map((t) => (
          <button key={t} className={`tab ${tab === t ? "active" : ""}`} onClick={() => setTab(t)}>
            {{ overview: "Overview", materials: `Materials (${d.attachments.length})`, files: "Project files", log: "Build log", submission: "GitHub & submission" }[t]}
          </button>
        ))}
      </div>

      {tab === "overview" && (
        <div className="grid-2">
          <Section title="Assignment">
            <div className="panel panel-pad desc">{a.description_text || <span className="muted">No description text. See the materials.</span>}</div>
          </Section>
          <div>
            <Section title="Details">
              <div className="panel panel-pad">
                <dl className="kv">
                  <dt>Deadline</dt><dd>{formatDateTime(a.due_at)}</dd>
                  <dt>Assigned</dt><dd>{formatDateTime(a.assigned_at)}</dd>
                  <dt>Last synced</dt><dd>{relativeTime(a.last_synced_at)}</dd>
                  <dt>Workspace</dt><dd><a onClick={() => api.openWorkspace(id, "root")}>{d.workspace.root}</a></dd>
                  <dt>Repository</dt><dd>{a.repo_url ? <a onClick={() => api.openExternal(a.repo_url!)}>{a.repo_full_name}</a> : <span className="muted">not published</span>}</dd>
                  <dt>Submission</dt><dd>{d.submissions[0] ? `${d.submissions[0].state.replace("_", "-")} · ${relativeTime(d.submissions[0].created_at)}` : <span className="muted">not submitted</span>}</dd>
                </dl>
              </div>
            </Section>
            {d.plan && (
              <Section title="Implementation plan" aside={`${d.plan.kind} · ${d.plan.technology}`}>
                <div className="panel panel-pad">
                  <p style={{ marginTop: 0 }}>{d.plan.summary}</p>
                  <dl className="kv">
                    <dt>Deliverables</dt><dd>{d.plan.deliverables.join(", ")}</dd>
                    <dt>Build</dt><dd><code>{d.plan.build_commands.map((c) => [c.program, ...c.args].join(" ")).join("  ·  ") || "—"}</code></dd>
                    <dt>Test</dt><dd><code>{d.plan.test_commands.map((c) => [c.program, ...c.args].join(" ")).join("  ·  ") || "—"}</code></dd>
                    <dt>Expected output</dt><dd>{d.plan.expected_output}</dd>
                    {d.plan.ambiguities.length > 0 && (<><dt>Assumptions</dt><dd>{d.plan.ambiguities.map((x) => <div key={x}>{x}</div>)}</dd></>)}
                  </dl>
                </div>
              </Section>
            )}
          </div>
        </div>
      )}

      {tab === "materials" && (
        <Section title="Attachments" aside={<a onClick={() => api.openWorkspace(id, "materials")}>Open folder</a>}>
          <div className="btn-row" style={{ marginBottom: 10 }}>
            <button className="btn" disabled={!!busy} onClick={() => run("dl", () => api.downloadMaterials(id), (r) => `Downloaded ${r.downloaded} file(s)${r.warnings.length ? `. ${r.warnings.join("; ")}` : ""}`)}>
              {busy === "dl" ? "Downloading…" : "Download materials"}
            </button>
          </div>
          <div className="panel">
            {d.attachments.length ? (
              <table>
                <thead><tr><th>Name</th><th>Size</th><th>Status</th><th /></tr></thead>
                <tbody>
                  {d.attachments.map((t) => (
                    <tr key={t.id}>
                      <td>{t.name}</td>
                      <td>{formatBytes(t.size_bytes)}</td>
                      <td>{t.error ? <span className="tone-danger">{t.error}</span> : t.downloaded_at ? "Downloaded" : t.url.includes("edupage.org") ? "Pending" : <span className="muted">External link</span>}</td>
                      <td style={{ textAlign: "right" }}><a onClick={() => api.openAttachment(id, t.id).catch((e) => setError(errorMessage(e)))}>{t.local_path ? "Show" : "Open"}</a></td>
                    </tr>
                  ))}
                </tbody>
              </table>
            ) : <Empty>No attachments.</Empty>}
          </div>
        </Section>
      )}

      {tab === "files" && (
        <Section title="Generated project" aside={<a onClick={() => api.openWorkspace(id, "project")}>Open folder</a>}>
          <Files key={lastBuild?.id ?? 0} id={id} />
        </Section>
      )}

      {tab === "log" && (
        <Section title="Build log" aside={lastBuild ? `Build #${lastBuild.id} · ${lastBuild.outcome} · ${relativeTime(lastBuild.started_at)}` : undefined}>
          <Log events={events} />
        </Section>
      )}

      {tab === "submission" && (
        <div className="grid-2">
          <Section title="GitHub">
            <div className="panel panel-pad">
              {a.repo_url ? (
                <p style={{ marginTop: 0 }}>Published to <a onClick={() => api.openExternal(a.repo_url!)}>{a.repo_full_name}</a>. Publishing again pushes a new commit.</p>
              ) : (
                <p style={{ marginTop: 0 }} className="muted">Creates a new repository with README, .gitignore and the project. Files matching .gitignore and anything that looks like a secret are never uploaded.</p>
              )}
              {!a.repo_url && (
                <label className="check" style={{ marginBottom: 10 }}>
                  <input type="checkbox" checked={makePublic} onChange={(e) => setMakePublic(e.target.checked)} /> Make this repository public
                </label>
              )}
              <button className="btn" disabled={!built || !!busy || running}
                onClick={() => run("gh", () => api.publishToGithub(id, a.repo_url ? null : !makePublic), (r) => `${r.created ? "Created" : "Updated"} ${r.full_name} (${r.files} files, commit ${r.commit.slice(0, 7)})`)}>
                {busy === "gh" ? "Publishing…" : a.repo_url ? "Push update to GitHub" : "Publish to GitHub"}
              </button>
              {!built && <p className="muted" style={{ marginBottom: 0 }}>Available after a successful build.</p>}
            </div>
          </Section>
          <Section title="EduPage submission">
            <div className="panel panel-pad">
              <p style={{ marginTop: 0 }} className="muted">You will review the files, link and method before anything is sent.</p>
              <button className="btn primary" disabled={!built || running} onClick={() => setSubmitOpen(true)}>Review & submit…</button>
              {d.submissions.length > 0 && (
                <table style={{ marginTop: 14 }}>
                  <thead><tr><th>When</th><th>Method</th><th>Result</th></tr></thead>
                  <tbody>
                    {d.submissions.map((s) => (
                      <tr key={s.id}>
                        <td>{relativeTime(s.created_at)}</td>
                        <td>{s.method === "edupage_message" ? "EduPage message" : "Manual hand-in"}</td>
                        <td className={s.state === "failed" ? "tone-danger" : ""}>
                          {s.state === "confirmed" ? `Confirmed by EduPage (#${s.platform_ref})` : s.state === "self_reported" ? "Self-reported" : `Failed: ${s.error}`}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>
          </Section>
        </div>
      )}

      {submitOpen && <SubmitDialog id={id} onClose={() => setSubmitOpen(false)} onDone={(m) => { setSubmitOpen(false); setNotice(m); load(); }} />}
    </div>
  );
}
