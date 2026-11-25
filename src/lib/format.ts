import type { Status } from "./api";

export const STATUS_LABEL: Record<Status, string> = {
  new: "New",
  not_started: "Not started",
  building: "Building",
  build_failed: "Build failed",
  ready_to_submit: "Ready to submit",
  submitted: "Submitted",
};

/** Backend stores naive local due dates ("2026-12-01T23:59:00") and UTC RFC3339 timestamps. */
export function parseDate(s: string | null | undefined): Date | null {
  if (!s) return null;
  const d = new Date(s);
  return Number.isNaN(d.getTime()) ? null : d;
}

const DAY = 86_400_000;

export function dueLabel(due: string | null, now: Date = new Date()): { text: string; tone: "muted" | "warn" | "danger" } {
  const d = parseDate(due);
  if (!d) return { text: "No deadline", tone: "muted" };
  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const startOfDue = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((startOfDue - startOfToday) / DAY);
  if (d.getTime() < now.getTime()) {
    const late = Math.max(1, -days);
    return { text: days === 0 ? "Due earlier today" : `Overdue by ${late} day${late === 1 ? "" : "s"}`, tone: "danger" };
  }
  if (days === 0) return { text: "Due today", tone: "danger" };
  if (days === 1) return { text: "Due tomorrow", tone: "warn" };
  if (days < 7) return { text: `Due in ${days} days`, tone: days <= 3 ? "warn" : "muted" };
  return { text: `Due ${d.toLocaleDateString(undefined, { day: "numeric", month: "short" })}`, tone: "muted" };
}

export function relativeTime(s: string | null | undefined, now: Date = new Date()): string {
  const d = parseDate(s);
  if (!d) return "never";
  const diff = now.getTime() - d.getTime();
  const min = Math.round(diff / 60_000);
  if (min < 1) return "just now";
  if (min < 60) return `${min} min ago`;
  const h = Math.round(min / 60);
  if (h < 24) return `${h} h ago`;
  const days = Math.round(h / 24);
  if (days < 30) return `${days} d ago`;
  return d.toLocaleDateString();
}

export function formatDateTime(s: string | null | undefined): string {
  const d = parseDate(s);
  return d ? d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }) : "—";
}

export function formatBytes(n: number | null | undefined): string {
  if (n == null) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}
