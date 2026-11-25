import { describe, expect, it } from "vitest";
import { dueLabel, formatBytes, relativeTime } from "./format";

describe("dueLabel", () => {
  const now = new Date(2026, 10, 25, 12, 0, 0);
  it("handles missing deadlines", () => expect(dueLabel(null, now)).toEqual({ text: "No deadline", tone: "muted" }));
  it("flags overdue", () => expect(dueLabel("2026-11-23T23:59:00", now).tone).toBe("danger"));
  it("today and tomorrow", () => {
    expect(dueLabel("2026-11-25T23:59:00", now).text).toBe("Due today");
    expect(dueLabel("2026-11-26T23:59:00", now).text).toBe("Due tomorrow");
  });
  it("soon is a warning", () => expect(dueLabel("2026-11-28T23:59:00", now)).toEqual({ text: "Due in 3 days", tone: "warn" }));
  it("later is muted", () => expect(dueLabel("2026-12-20T23:59:00", now).tone).toBe("muted"));
});

describe("misc", () => {
  it("bytes", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KB");
  });
  it("relative", () => {
    const now = new Date("2026-11-25T12:00:00Z");
    expect(relativeTime("2026-11-25T11:30:00Z", now)).toBe("30 min ago");
    expect(relativeTime(null, now)).toBe("never");
  });
});
