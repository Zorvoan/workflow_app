import { useCallback, useEffect, useState } from "react";
import { NavLink, Route, Routes } from "react-router-dom";
import { listen } from "@tauri-apps/api/event";
import { api, errorMessage, type Dashboard as DashboardData, type Settings } from "./lib/api";
import { relativeTime } from "./lib/format";
import Dashboard from "./pages/Dashboard";
import Assignments from "./pages/Assignments";
import AssignmentDetail from "./pages/AssignmentDetail";
import Repositories from "./pages/Repositories";
import SettingsPage from "./pages/Settings";

function applyTheme(theme: Settings["ui"]["theme"]) {
  const root = document.documentElement;
  if (theme === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", theme);
}

export default function App() {
  const [counts, setCounts] = useState<DashboardData["counts"]>({});
  const [lastSync, setLastSync] = useState<DashboardData["last_sync"]>(null);
  const [syncing, setSyncing] = useState(false);
  const [syncError, setSyncError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api.dashboard().then((d) => {
      setCounts(d.counts);
      setLastSync(d.last_sync);
    }).catch(() => {});
  }, []);

  useEffect(() => {
    api.getSettings().then((s) => applyTheme(s.ui.theme)).catch(() => {});
    refresh();
    const un = [
      listen("sync-finished", () => { setSyncing(false); setSyncError(null); refresh(); }),
      listen("sync-failed", (e) => { setSyncing(false); setSyncError(errorMessage(e.payload)); refresh(); }),
      listen("build-finished", refresh),
      listen<string>("theme-changed", (e) => applyTheme(e.payload as Settings["ui"]["theme"])),
    ];
    return () => { un.forEach((p) => p.then((f) => f())); };
  }, [refresh]);

  const syncNow = async () => {
    setSyncing(true);
    setSyncError(null);
    try {
      await api.syncNow();
    } catch (e) {
      setSyncError(errorMessage(e));
    } finally {
      setSyncing(false);
      refresh();
    }
  };

  const inProgress = (counts.building ?? 0) + (counts.build_failed ?? 0) + (counts.ready_to_submit ?? 0);
  const total = Object.values(counts).reduce((a, b) => a + b, 0);

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand"><div className="brand-mark">E</div>EduTask</div>
        <NavLink to="/" end className="nav-item">Dashboard</NavLink>
        <NavLink to="/assignments" className="nav-item">All assignments <span className="nav-count">{total || ""}</span></NavLink>
        <NavLink to="/in-progress" className="nav-item">In progress <span className="nav-count">{inProgress || ""}</span></NavLink>
        <NavLink to="/completed" className="nav-item">Completed <span className="nav-count">{counts.submitted || ""}</span></NavLink>
        <NavLink to="/repositories" className="nav-item">GitHub repositories</NavLink>
        <NavLink to="/settings" className="nav-item">Settings</NavLink>
        <div className="sidebar-footer">
          <button className="btn" onClick={syncNow} disabled={syncing}>{syncing ? "Syncing…" : "Sync now"}</button>
          <span>Last sync: {lastSync ? `${relativeTime(lastSync.finished_at)}${lastSync.ok ? "" : " (failed)"}` : "never"}</span>
          {syncError && <span className="tone-danger">{syncError}</span>}
        </div>
      </aside>
      <main className="main">
        <Routes>
          <Route path="/" element={<Dashboard />} />
          <Route path="/assignments" element={<Assignments filter="all" title="All assignments" />} />
          <Route path="/in-progress" element={<Assignments filter="in_progress" title="In progress" />} />
          <Route path="/completed" element={<Assignments filter="completed" title="Completed" />} />
          <Route path="/assignment/:id" element={<AssignmentDetail />} />
          <Route path="/repositories" element={<Repositories />} />
          <Route path="/settings" element={<SettingsPage />} />
        </Routes>
      </main>
    </div>
  );
}
