import { useEffect, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { api, errorMessage, type AiUsage, type ConnectionStatus, type DeviceCode, type GithubUser, type Settings, type Toolchain } from "../lib/api";
import { formatTokens } from "../lib/format";
import { Banner, Field, PageHeader } from "../components/ui";

const LANGS: [string, string][] = [
  ["rust", "Rust"], ["cpp", "C++"], ["c", "C"], ["csharp", "C#"], ["java", "Java"], ["python", "Python"], ["php", "PHP"],
  ["javascript", "JavaScript"], ["typescript", "TypeScript"], ["html", "HTML/CSS"], ["sql", "SQL"], ["document", "Documents"],
];
const MODELS = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-2"];

function EdupageSection({ s, onSaved }: { s: Settings; onSaved: () => void }) {
  const [status, setStatus] = useState<ConnectionStatus | null>(null);
  const [sub, setSub] = useState(s.edupage.subdomain);
  const [user, setUser] = useState(s.edupage.username);
  const [pw, setPw] = useState("");
  const [remember, setRemember] = useState(s.edupage.remember_password);
  const [msg, setMsg] = useState<{ tone: "ok" | "error" | "warn"; text: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = () => api.edupageStatus().then(setStatus).catch(() => {});
  useEffect(() => {
    refresh();
    const un = listen<{ ok: boolean; message?: string }>("edupage-browser-login", (e) => {
      setMsg(e.payload.ok ? { tone: "ok", text: "Signed in through the browser." } : { tone: "error", text: e.payload.message ?? "Browser sign-in failed" });
      refresh();
      onSaved();
    });
    return () => { un.then((f) => f()); };
  }, []);

  const login = async () => {
    setBusy(true);
    setMsg(null);
    try {
      const r = await api.edupageLogin({ subdomain: sub, username: user, password: pw, remember });
      if (r.result === "logged_in") {
        setPw("");
        setMsg({ tone: "ok", text: "Connected. Synchronizing assignments…" });
      } else {
        setMsg({ tone: "warn", text: `EduPage requires ${r.result === "two_factor_required" ? "two-factor verification" : "a CAPTCHA"}. Use “Sign in with browser” to complete it on EduPage's own page.` });
      }
      refresh();
      onSaved();
    } catch (e) {
      setMsg({ tone: "error", text: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="settings-section" id="edupage">
      <h2>EduPage</h2>
      <p>EduPage has no official student API. EduTask signs in the same way the EduPage website does; your password is stored only in Windows Credential Manager (if you choose to remember it).</p>
      {status && (
        <Banner tone={status.connected ? "ok" : "warn"}>
          {status.connected ? `Connected to ${status.subdomain}.edupage.org as ${status.username}${status.user_id ? ` (${status.user_id})` : ""}` : status.message ?? "Not connected"}
        </Banner>
      )}
      {msg && <Banner tone={msg.tone}>{msg.text}</Banner>}
      <div className="form">
        <Field label="School URL" hint="e.g. myschool or https://myschool.edupage.org">
          <input type="text" value={sub} onChange={(e) => setSub(e.target.value)} placeholder="myschool" />
        </Field>
        <Field label="Username or e-mail"><input type="text" value={user} onChange={(e) => setUser(e.target.value)} autoComplete="username" /></Field>
        <Field label="Password"><input type="password" value={pw} onChange={(e) => setPw(e.target.value)} autoComplete="current-password" /></Field>
        <label className="check"><input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} /> Remember password (Windows Credential Manager) so expired sessions renew automatically</label>
        <div className="btn-row">
          <button className="btn primary" disabled={busy || !sub || !user || !pw} onClick={login}>{busy ? "Signing in…" : "Sign in"}</button>
          <button className="btn" disabled={!sub} onClick={() => api.edupageBrowserLogin(sub, user).catch((e) => setMsg({ tone: "error", text: errorMessage(e) }))}>Sign in with browser</button>
          {status?.connected && <button className="btn ghost" onClick={async () => { await api.edupageLogout(true); refresh(); }}>Sign out</button>}
        </div>
      </div>
    </div>
  );
}

function GithubSection({ s, set }: { s: Settings; set: (f: (s: Settings) => Settings) => void }) {
  const [user, setUser] = useState<GithubUser | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [token, setToken] = useState("");
  const [device, setDevice] = useState<DeviceCode | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const timer = useRef<number | undefined>(undefined);

  const refresh = () => api.githubStatus().then((r) => { setUser(r.user); setMessage(r.message); }).catch(() => {});
  useEffect(() => { refresh(); return () => window.clearTimeout(timer.current); }, []);

  const poll = (dc: DeviceCode, interval: number) => {
    timer.current = window.setTimeout(async () => {
      try {
        const r = await api.githubDevicePoll(dc.device_code);
        if (r.poll.state === "authorized") { setDevice(null); setUser(r.user); }
        else if (r.poll.state === "pending") poll(dc, r.poll.interval);
        else { setDevice(null); setErr(r.poll.state === "denied" ? "Authorization was denied" : "The code expired; start again"); }
      } catch (e) { setDevice(null); setErr(errorMessage(e)); }
    }, interval * 1000);
  };

  return (
    <div className="settings-section" id="github">
      <h2>GitHub</h2>
      <p>Used to create one repository per assignment. The token is stored in Windows Credential Manager.</p>
      {user ? <Banner tone="ok">Connected as {user.login}{user.name ? ` (${user.name})` : ""}</Banner> : message && <Banner tone="warn">{message}</Banner>}
      {err && <Banner tone="error">{err}</Banner>}
      <div className="form">
        {!user && (
          <>
            <Field label="Personal access token" hint={<>Fine-grained token with <b>Administration</b> and <b>Contents</b> read/write on all repositories, or a classic token with the <code>repo</code> scope.</>}>
              <input type="password" value={token} onChange={(e) => setToken(e.target.value)} placeholder="github_pat_…" />
            </Field>
            <div className="btn-row">
              <button className="btn primary" disabled={!token} onClick={async () => { setErr(null); try { setUser(await api.githubConnectToken(token)); setToken(""); } catch (e) { setErr(errorMessage(e)); } }}>Connect with token</button>
              <button className="btn" disabled={!s.github.oauth_client_id} onClick={async () => {
                setErr(null);
                try { const dc = await api.githubDeviceStart(); setDevice(dc); api.openExternal(dc.verification_uri); poll(dc, dc.interval); } catch (e) { setErr(errorMessage(e)); }
              }}>Sign in with GitHub</button>
            </div>
            {device && <Banner>Enter code <b style={{ fontFamily: "var(--mono)", fontSize: 15 }}>{device.user_code}</b> at {device.verification_uri}. Waiting for authorization…</Banner>}
          </>
        )}
        {user && <div><button className="btn ghost" onClick={async () => { await api.githubDisconnect(); setUser(null); }}>Disconnect</button></div>}
        <Field label="OAuth App client id (optional)" hint="Enables “Sign in with GitHub” via the device flow. Create an OAuth App with device flow enabled.">
          <input type="text" value={s.github.oauth_client_id} onChange={(e) => set((x) => ({ ...x, github: { ...x.github, oauth_client_id: e.target.value } }))} />
        </Field>
        <label className="check">
          <input type="checkbox" checked={s.github.auto_publish} onChange={(e) => set((x) => ({ ...x, github: { ...x.github, auto_publish: e.target.checked } }))} />
          Push to GitHub automatically after every successful build
        </label>
        <Field label="Default visibility">
          <select value={s.github.default_private ? "private" : "public"} onChange={(e) => set((x) => ({ ...x, github: { ...x.github, default_private: e.target.value === "private" } }))}>
            <option value="private">Private</option>
            <option value="public">Public</option>
          </select>
        </Field>
        <Field label="Repository name template" hint="Tokens: {subject} {title} {year} {date} {id}. Diacritics are removed.">
          <input type="text" value={s.github.repo_name_template} onChange={(e) => set((x) => ({ ...x, github: { ...x.github, repo_name_template: e.target.value } }))} />
        </Field>
      </div>
    </div>
  );
}

function AiSection({ s, set }: { s: Settings; set: (f: (s: Settings) => Settings) => void }) {
  const [hasKey, setHasKey] = useState<Record<string, boolean>>({});
  const [key, setKey] = useState("");
  const [usage, setUsage] = useState<AiUsage[]>([]);
  const [msg, setMsg] = useState<string | null>(null);
  useEffect(() => { api.secretStatus().then((r) => setHasKey(r.ai_keys)); api.aiUsage().then(setUsage).catch(() => {}); }, []);
  const ai = s.ai;
  const upd = (p: Partial<Settings["ai"]>) => set((x) => ({ ...x, ai: { ...x.ai, ...p } }));

  return (
    <div className="settings-section" id="ai">
      <h2>AI</h2>
      <p>The AI analyzes assignments and writes the project. Assignment text and materials are sent to the selected provider.</p>
      <div className="form">
        <Field label="Provider">
          <select value={ai.provider} onChange={(e) => upd({ provider: e.target.value, model: e.target.value === "anthropic" ? "claude-opus-5-5" : "" })}>
            <option value="anthropic">Anthropic (Claude)</option>
            <option value="openai_compatible">OpenAI-compatible endpoint (other vendors, local models)</option>
          </select>
        </Field>
        {ai.provider === "openai_compatible" && (
          <Field label="Base URL" hint="e.g. http://localhost:11434/v1 for Ollama">
            <input type="text" value={ai.base_url} onChange={(e) => upd({ base_url: e.target.value })} />
          </Field>
        )}
        <Field label="Model">
          {ai.provider === "anthropic" ? (
            <select value={ai.model} onChange={(e) => upd({ model: e.target.value })}>
              {[...new Set([ai.model, ...MODELS])].map((m) => <option key={m}>{m}</option>)}
            </select>
          ) : (
            <input type="text" value={ai.model} onChange={(e) => upd({ model: e.target.value })} />
          )}
        </Field>
        {ai.provider === "anthropic" && (
          <Field label="Effort" hint="Higher effort writes better code but uses more tokens.">
            <select value={ai.effort} onChange={(e) => upd({ effort: e.target.value })}>
              {["low", "medium", "high", "xhigh", "max"].map((x) => <option key={x}>{x}</option>)}
            </select>
          </Field>
        )}
        <Field label={`API key ${hasKey[ai.provider] ? "(stored)" : "(not set)"}`} hint="Stored in Windows Credential Manager. Leave empty and save to remove.">
          <div className="btn-row">
            <input type="password" value={key} onChange={(e) => setKey(e.target.value)} style={{ flex: 1 }} placeholder={hasKey[ai.provider] ? "••••••••" : "sk-ant-…"} />
            <button className="btn" onClick={async () => { await api.setAiKey(ai.provider, key); setKey(""); setMsg("Key saved"); setHasKey(await api.secretStatus().then((r) => r.ai_keys)); }}>Save key</button>
          </div>
        </Field>
        {msg && <span className="muted">{msg}</span>}
        <Field label="Fix attempts" hint="How many times build/test failures are sent back to the AI.">
          <input type="number" min={0} max={10} value={ai.max_fix_iterations} onChange={(e) => upd({ max_fix_iterations: Number(e.target.value) })} />
        </Field>
        <Field label="Token usage (last 30 days)">
          {usage.length ? (
            <table>
              <thead><tr><th>Model</th><th>Requests</th><th>Input</th><th>Output</th><th>Cache read</th></tr></thead>
              <tbody>{usage.map((u) => <tr key={u.provider + u.model}><td>{u.model}</td><td>{u.requests}</td><td>{formatTokens(u.input_tokens)}</td><td>{formatTokens(u.output_tokens)}</td><td>{formatTokens(u.cache_read_tokens)}</td></tr>)}</tbody>
            </table>
          ) : <span className="muted">No usage yet.</span>}
        </Field>
      </div>
    </div>
  );
}

function BuildSection({ s, set }: { s: Settings; set: (f: (s: Settings) => Settings) => void }) {
  const [tools, setTools] = useState<Toolchain[]>([]);
  useEffect(() => { api.detectToolchains().then(setTools).catch(() => {}); }, []);
  const b = s.build;
  const upd = (p: Partial<Settings["build"]>) => set((x) => ({ ...x, build: { ...x.build, ...p } }));
  const toggle = (l: string) => upd({ enabled_languages: b.enabled_languages.includes(l) ? b.enabled_languages.filter((x) => x !== l) : [...b.enabled_languages, l] });
  return (
    <div className="settings-section" id="build">
      <h2>Build</h2>
      <p>Generated commands run without a shell, from an allowlist of build tools, with a clean environment and a timeout. Generated code still runs with your user's permissions, so review projects you do not trust.</p>
      <div className="form">
        <Field label="Project directory">
          <div className="btn-row">
            <input type="text" value={b.projects_dir} onChange={(e) => upd({ projects_dir: e.target.value })} style={{ flex: 1 }} />
            <button className="btn" onClick={async () => { const d = await open({ directory: true }); if (typeof d === "string") upd({ projects_dir: d }); }}>Browse…</button>
          </div>
        </Field>
        <Field label="Command timeout (seconds)">
          <input type="number" min={10} max={3600} value={b.command_timeout_secs} onChange={(e) => upd({ command_timeout_secs: Number(e.target.value) })} />
        </Field>
        <label className="check"><input type="checkbox" checked={b.allow_dependency_install} onChange={(e) => upd({ allow_dependency_install: e.target.checked })} /> Allow installing dependencies (npm install, pip install, dotnet add package, …)</label>
        <Field label="Allowed languages">
          <div className="chips">{LANGS.map(([id, label]) => <span key={id} className={`chip ${b.enabled_languages.includes(id) ? "on" : ""}`} onClick={() => toggle(id)}>{label}</span>)}</div>
        </Field>
        <Field label="Extra .gitignore lines">
          <textarea value={b.extra_gitignore} onChange={(e) => upd({ extra_gitignore: e.target.value })} placeholder="*.sqlite" />
        </Field>
        <Field label="Installed toolchains">
          <div className="chips">
            {tools.map((t) => <span key={t.name} className={`chip ${t.path ? "on" : ""}`} title={t.note ?? t.path ?? "not installed"}>{t.name}</span>)}
          </div>
        </Field>
      </div>
    </div>
  );
}

export default function SettingsPage() {
  const [s, setS] = useState<Settings | null>(null);
  const [dirty, setDirty] = useState(false);
  const [msg, setMsg] = useState<{ tone: "ok" | "error"; text: string } | null>(null);
  const [info, setInfo] = useState<{ version: string; log_dir: string } | null>(null);

  const load = () => api.getSettings().then((x) => { setS(x); setDirty(false); });
  useEffect(() => { load(); api.appInfo().then(setInfo).catch(() => {}); }, []);
  const set = (f: (s: Settings) => Settings) => { setS((x) => (x ? f(x) : x)); setDirty(true); };

  const save = async () => {
    if (!s) return;
    try {
      const saved = await api.saveSettings(s);
      setS(saved);
      setDirty(false);
      emit("theme-changed", saved.ui.theme);
      setMsg({ tone: "ok", text: "Settings saved" });
    } catch (e) {
      setMsg({ tone: "error", text: errorMessage(e) });
    }
  };

  if (!s) return <div className="page" />;
  return (
    <div className="page">
      <PageHeader title="Settings" actions={<button className="btn primary" disabled={!dirty} onClick={save}>Save changes</button>} />
      {msg && <Banner tone={msg.tone}>{msg.text}</Banner>}
      <div className="settings-layout">
        <nav className="settings-nav">
          {["edupage", "github", "ai", "build", "appearance"].map((x) => <a key={x} className="nav-item" href={`#/settings`} onClick={(e) => { e.preventDefault(); document.getElementById(x)?.scrollIntoView({ behavior: "smooth" }); }}>{x === "ai" ? "AI" : x === "edupage" ? "EduPage" : x === "github" ? "GitHub" : x[0].toUpperCase() + x.slice(1)}</a>)}
        </nav>
        <div>
          <EdupageSection s={s} onSaved={load} />
          <div className="settings-section">
            <div className="form">
              <Field label="Synchronization frequency">
                <select value={s.edupage.sync_interval_minutes} onChange={(e) => set((x) => ({ ...x, edupage: { ...x.edupage, sync_interval_minutes: Number(e.target.value) } }))}>
                  {[[0, "Manual only"], [15, "Every 15 minutes"], [30, "Every 30 minutes"], [60, "Every hour"], [180, "Every 3 hours"]].map(([v, l]) => <option key={v} value={v}>{l}</option>)}
                </select>
              </Field>
            </div>
          </div>
          <GithubSection s={s} set={set} />
          <AiSection s={s} set={set} />
          <BuildSection s={s} set={set} />
          <div className="settings-section" id="appearance">
            <h2>Appearance</h2>
            <div className="form">
              <Field label="Theme">
                <select value={s.ui.theme} onChange={(e) => set((x) => ({ ...x, ui: { theme: e.target.value as Settings["ui"]["theme"] } }))}>
                  <option value="system">System</option><option value="light">Light</option><option value="dark">Dark</option>
                </select>
              </Field>
              {info && <span className="muted">EduTask {info.version} · Logs: {info.log_dir}</span>}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
