import { useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorMessage, type AssignmentSummary, type Filter } from "../lib/api";
import { AssignmentRow, Banner, Empty, PageHeader } from "../components/ui";

export default function Assignments({ filter, title }: { filter: Filter; title: string }) {
  const [items, setItems] = useState<AssignmentSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [subject, setSubject] = useState("");

  useEffect(() => {
    const load = () => api.listAssignments(filter).then(setItems).catch((e) => setError(errorMessage(e)));
    setItems(null);
    load();
    const un = ["sync-finished", "build-finished"].map((ev) => listen(ev, load));
    return () => { un.forEach((p) => p.then((f) => f())); };
  }, [filter]);

  const subjects = useMemo(() => Array.from(new Set((items ?? []).map((a) => a.subject).filter(Boolean) as string[])).sort(), [items]);
  const shown = (items ?? []).filter((a) => {
    const q = query.trim().toLowerCase();
    return (!subject || a.subject === subject) && (!q || `${a.title} ${a.subject ?? ""} ${a.teacher ?? ""}`.toLowerCase().includes(q));
  });

  return (
    <div className="page">
      <PageHeader title={title} subtitle={items ? `${shown.length} assignment${shown.length === 1 ? "" : "s"}` : "Loading…"} />
      {error && <Banner tone="error">{error}</Banner>}
      <div className="btn-row" style={{ marginBottom: 12 }}>
        <input type="text" placeholder="Search title, subject, teacher" value={query} onChange={(e) => setQuery(e.target.value)} style={{ maxWidth: 320 }} />
        <select value={subject} onChange={(e) => setSubject(e.target.value)} style={{ maxWidth: 220 }}>
          <option value="">All subjects</option>
          {subjects.map((s) => <option key={s}>{s}</option>)}
        </select>
      </div>
      <div className="panel list">
        {items && !shown.length ? <Empty>No assignments here.</Empty> : shown.map((a) => <AssignmentRow key={a.id} a={a} />)}
      </div>
    </div>
  );
}
