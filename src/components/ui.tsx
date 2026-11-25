import { Link } from "react-router-dom";
import type { ReactNode } from "react";
import type { AssignmentSummary, Status } from "../lib/api";
import { STATUS_LABEL, dueLabel } from "../lib/format";

export function StatusBadge({ status }: { status: Status }) {
  return <span className={`badge b-${status}`}>{STATUS_LABEL[status] ?? status}</span>;
}

export function PageHeader({ title, subtitle, actions }: { title: ReactNode; subtitle?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="page-header">
      <div>
        <h1>{title}</h1>
        {subtitle && <p>{subtitle}</p>}
      </div>
      {actions && <div className="btn-row">{actions}</div>}
    </div>
  );
}

export function Banner({ tone = "info", children }: { tone?: "info" | "error" | "warn" | "ok"; children: ReactNode }) {
  return <div className={`banner ${tone}`}>{children}</div>;
}

export function Empty({ children }: { children: ReactNode }) {
  return <div className="empty">{children}</div>;
}

export function Section({ title, aside, children }: { title: string; aside?: ReactNode; children: ReactNode }) {
  return (
    <div className="section">
      <div className="section-head">
        <h2>{title}</h2>
        {aside && <span>{aside}</span>}
      </div>
      {children}
    </div>
  );
}

export function AssignmentRow({ a }: { a: AssignmentSummary }) {
  const due = dueLabel(a.due_at);
  return (
    <Link className="row" to={`/assignment/${encodeURIComponent(a.id)}`}>
      <div style={{ minWidth: 0 }}>
        <div className="row-title">
          <span className="t">{a.title}</span>
          {a.changed_since_seen && <span className="tag">Updated</span>}
        </div>
        <div className="row-meta">
          {a.subject && <span>{a.subject}</span>}
          {a.teacher && <span>{a.teacher}</span>}
          {a.attachment_count > 0 && <span>{a.attachment_count} attachment{a.attachment_count === 1 ? "" : "s"}</span>}
        </div>
      </div>
      <div className="row-right">
        {a.status !== "submitted" && <span className={`tone-${due.tone}`} style={{ fontSize: 12 }}>{due.text}</span>}
        <StatusBadge status={a.status} />
      </div>
    </Link>
  );
}

export function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <div className="field">
      <label>{label}</label>
      {children}
      {hint && <div className="hint">{hint}</div>}
    </div>
  );
}
