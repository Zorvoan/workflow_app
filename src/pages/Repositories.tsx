import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { api, errorMessage, type RepositoryRow } from "../lib/api";
import { relativeTime } from "../lib/format";
import { Banner, Empty, PageHeader } from "../components/ui";

export default function Repositories() {
  const [rows, setRows] = useState<RepositoryRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [login, setLogin] = useState<string | null>(null);

  useEffect(() => {
    api.listRepositories().then(setRows).catch((e) => setError(errorMessage(e)));
    api.githubStatus().then((s) => setLogin(s.user?.login ?? null)).catch(() => {});
  }, []);

  return (
    <div className="page">
      <PageHeader title="GitHub repositories" subtitle={login ? `Connected as ${login}` : <>GitHub is not connected. <Link to="/settings">Connect it in Settings</Link>.</>} />
      {error && <Banner tone="error">{error}</Banner>}
      <div className="panel list">
        {rows && !rows.length && <Empty>No repositories yet. Publish a built assignment from its workspace.</Empty>}
        {rows?.map((r) => (
          <div className="row" key={r.assignment_id}>
            <div style={{ minWidth: 0 }}>
              <div className="row-title"><Link to={`/assignment/${encodeURIComponent(r.assignment_id)}`}>{r.title}</Link></div>
              <div className="row-meta"><span>{r.full_name}</span>{r.subject && <span>{r.subject}</span>}<span>updated {relativeTime(r.updated_at)}</span></div>
            </div>
            <button className="btn" onClick={() => api.openExternal(r.url).catch((e) => setError(errorMessage(e)))}>Open on GitHub</button>
          </div>
        ))}
      </div>
    </div>
  );
}
