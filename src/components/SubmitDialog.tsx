import { useEffect, useState } from "react";
import { api, errorMessage, type SubmissionPreview } from "../lib/api";
import { formatBytes } from "../lib/format";
import { Banner } from "./ui";

/**
 * Final confirmation before anything is sent to EduPage.
 * The backend refuses submission without the single-use token issued here.
 */
export default function SubmitDialog({ id, onClose, onDone }: { id: string; onClose: () => void; onDone: (msg: string) => void }) {
  const [preview, setPreview] = useState<SubmissionPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [method, setMethod] = useState<string>("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [manualStep, setManualStep] = useState(false);
  const [confirmed, setConfirmed] = useState(false);

  useEffect(() => {
    api.prepareSubmission(id)
      .then((p) => {
        setPreview(p);
        setMessage(p.message);
        setMethod(p.methods.find((m) => m.available)?.id ?? "");
      })
      .catch((e) => setError(errorMessage(e)));
  }, [id]);

  const selected = preview?.methods.find((m) => m.id === method);

  const submit = async () => {
    if (!preview || !selected) return;
    setBusy(true);
    setError(null);
    try {
      if (selected.id === "manual_handin") {
        await api.recordManualSubmission(id, preview.confirm_token, "Handed in via EduPage by the student");
        onDone("Recorded as handed in (self-reported, not verified by EduPage).");
      } else {
        const r = await api.submitAssignment(id, preview.confirm_token, selected.id, message);
        onDone(r.message);
      }
    } catch (e) {
      // The token is single-use; reload a fresh preview so the user can retry.
      setError(errorMessage(e));
      setConfirmed(false);
      api.prepareSubmission(id).then(setPreview).catch(() => {});
    } finally {
      setBusy(false);
    }
  };

  const startManual = async () => {
    setManualStep(true);
    if (preview?.repo_url) await navigator.clipboard?.writeText(preview.repo_url).catch(() => {});
    await api.openWorkspace(id, "submission").catch(() => {});
    await api.openEdupage().catch(() => {});
  };

  return (
    <div className="modal-back" onMouseDown={(e) => e.target === e.currentTarget && !busy && onClose()}>
      <div className="modal" role="dialog" aria-modal="true">
        <div className="modal-head"><h2>Review submission</h2></div>
        <div className="modal-body">
          {error && <Banner tone="error">{error}</Banner>}
          {!preview && !error && <div className="muted">Preparing files…</div>}
          {preview && (
            <>
              <dl className="kv">
                <dt>Assignment</dt><dd>{preview.title}</dd>
                <dt>Teacher</dt><dd>{preview.teacher ?? "—"}</dd>
                <dt>Repository</dt><dd>{preview.repo_url ? <a onClick={() => api.openExternal(preview.repo_url!)}>{preview.repo_url}</a> : <span className="muted">not published</span>}</dd>
                {preview.expectation && (<><dt>Teacher expects</dt><dd>{preview.expectation}</dd></>)}
                <dt>Files</dt>
                <dd>{preview.files.map((f) => <div key={f.path}>{f.name} <span className="muted">({formatBytes(f.size)})</span></div>)}</dd>
              </dl>
              {preview.warnings.map((w) => <Banner key={w} tone="warn">{w}</Banner>)}
              <div className="field">
                <label>Submission method</label>
                {preview.methods.map((m) => (
                  <label key={m.id} className={`method ${method === m.id ? "selected" : ""} ${m.available ? "" : "disabled"}`}>
                    <input type="radio" name="method" disabled={!m.available} checked={method === m.id} onChange={() => { setMethod(m.id); setConfirmed(false); }} />
                    <div>
                      <b>{m.label}</b>
                      <p>{m.description}</p>
                      {!m.available && <p className="tone-danger">{m.unavailable_reason}</p>}
                    </div>
                  </label>
                ))}
              </div>
              {selected?.id === "edupage_message" && (
                <div className="field">
                  <label>Message to teacher</label>
                  <textarea value={message} onChange={(e) => setMessage(e.target.value)} />
                  <div className="hint">File links are appended automatically after upload.</div>
                </div>
              )}
              {selected?.id === "manual_handin" && (
                <div className="banner">
                  1. <a onClick={startManual}>Open EduPage and the submission folder</a> (the repository link is copied to the clipboard).<br />
                  2. Hand in the files in EduPage's homework view.<br />
                  3. Come back and confirm below. EduTask will record this as <b>self-reported</b>.
                </div>
              )}
              <label className="check">
                <input type="checkbox" checked={confirmed} onChange={(e) => setConfirmed(e.target.checked)} disabled={selected?.id === "manual_handin" && !manualStep} />
                {selected?.id === "manual_handin" ? "I have handed this assignment in on EduPage." : "I reviewed the files and message and want to send them to my teacher."}
              </label>
            </>
          )}
        </div>
        <div className="modal-foot">
          <button className="btn" onClick={onClose} disabled={busy}>Cancel</button>
          <button className="btn primary" disabled={!preview || !selected || !confirmed || busy} onClick={submit}>
            {busy ? "Submitting…" : selected?.id === "manual_handin" ? "Mark as handed in" : "Submit to EduPage"}
          </button>
        </div>
      </div>
    </div>
  );
}
