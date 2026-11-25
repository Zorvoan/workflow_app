import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { listen } from "@tauri-apps/api/event";
import { api, errorMessage, type Dashboard as Data } from "../lib/api";
import { relativeTime } from "../lib/format";
import { AssignmentRow, Banner, Empty, PageHeader, Section } from "../components/ui";

const OUTCOME: Record<string, string> = { running: "Running", succeeded: "Succeeded", failed: "Failed", cancelled: "Cancelled" };
const SUB_STATE: Record<string, string> = { confirmed: "Confirmed by EduPage", self_reported: "Self-reported", failed: "Failed" };

export default function Dashboard() {
  const [data, setData] = useState<Data | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = () => api.dashboard().then(setData).catch((e) => setError(errorMessage(e)));
  useEffect(() => {
    load();
    const un = ["sync-finished", "build-finished", "build-event"].map((ev) => listen(ev, load));
    return () => { un.forEach((p) => p.then((f) => f())); };
  }, []);

  if (error) return <div className="page"><Banner tone="error">{error}</Banner></div>;
  if (!data) return <div className="page" />;
  const total = Object.values(data.counts).reduce((a, b) => a + b, 0);

  return (
    <div className="page">
      <PageHeader title="Dashboard" subtitle={data.last_sync ? `Synchronized ${relativeTime(data.last_sync.finished_at)}` : "Not synchronized yet"} />
      {total === 0 && (
        <Banner>
          No assignments yet. <Link to="/settings">Connect EduPage in Settings</Link>, then press “Sync now”.
        </Banner>
      )}
      {data.last_sync && !data.last_sync.ok && <Banner tone="error">Last synchronization failed: {data.last_sync.error}</Banner>}

      <div className="stats">
        <div className="stat"><b>{total - (data.counts.submitted ?? 0)}</b><span>Open</span></div>
        <div className="stat"><b>{data.counts.new ?? 0}</b><span>New</span></div>
        <div className="stat"><b>{data.counts.build_failed ?? 0}</b><span>Build failed</span></div>
        <div className="stat"><b>{data.counts.ready_to_submit ?? 0}</b><span>Ready to submit</span></div>
        <div className="stat"><b>{data.counts.submitted ?? 0}</b><span>Submitted</span></div>
      </div>

      <div className="grid-2">
        <div>
          <Section title="Upcoming deadlines" aside="next 14 days">
            <div className="panel list">
              {data.upcoming.length ? data.upcoming.map((a) => <AssignmentRow key={a.id} a={a} />) : <Empty>Nothing due soon.</Empty>}
            </div>
          </Section>
          <Section title="Needs attention">
            <div className="panel list">
              {data.attention.length ? data.attention.map((a) => <AssignmentRow key={a.id} a={a} />) : <Empty>All clear.</Empty>}
            </div>
          </Section>
        </div>
        <div>
          <Section title="Recently synchronized">
            <div className="panel list">
              {data.recent.length ? data.recent.map((a) => <AssignmentRow key={a.id} a={a} />) : <Empty>No assignments.</Empty>}
            </div>
          </Section>
          <Section title="Build activity">
            <div className="panel list">
              {data.builds.length ? (
                data.builds.map((b) => (
                  <Link className="row" key={b.build_id} to={`/assignment/${encodeURIComponent(b.assignment_id)}`}>
                    <div className="row-title"><span className="t">{b.title}</span></div>
                    <div className="row-right">
                      <span className="muted" style={{ fontSize: 12 }}>{relativeTime(b.started_at)}</span>
                      <span className={b.outcome === "succeeded" ? "tone-muted" : b.outcome === "failed" ? "tone-danger" : "tone-warn"}>{OUTCOME[b.outcome]}</span>
                    </div>
                  </Link>
                ))
              ) : (
                <Empty>No builds yet.</Empty>
              )}
            </div>
          </Section>
          <Section title="Submission history">
            <div className="panel list">
              {data.submissions.length ? (
                data.submissions.map((s) => (
                  <Link className="row" key={s.id} to={`/assignment/${encodeURIComponent(s.assignment_id)}`}>
                    <div className="row-title"><span className="t">{s.title}</span></div>
                    <div className="row-right">
                      <span className="muted" style={{ fontSize: 12 }}>{relativeTime(s.created_at)}</span>
                      <span className={s.state === "failed" ? "tone-danger" : "tone-muted"}>{SUB_STATE[s.state]}</span>
                    </div>
                  </Link>
                ))
              ) : (
                <Empty>Nothing submitted yet.</Empty>
              )}
            </div>
          </Section>
        </div>
      </div>
    </div>
  );
}
