import { useEffect, useState } from "react";
import { libraryRequest, type PackageKey } from "../api-library";

/** The host grants access to saved revisions only; agents cannot change this permission. */
export function DraftAgentAccess({ draftKey }: { draftKey: PackageKey }) {
  const [enabled, setEnabled] = useState<boolean>();
  const [retry, setRetry] = useState(0);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  useEffect(() => {
    let active = true;
    setBusy(true);
    void libraryRequest<{ enabled: boolean }>({ action: "draftAccess", key: draftKey })
      .then(r => { if (active) { setEnabled(r.enabled); setError(""); } })
      .catch(e => { if (active) setError((e as Error).message); })
      .finally(() => { if (active) setBusy(false); });
    return () => { active = false; };
  }, [draftKey.service, draftKey.apiVersion, draftKey.scope, retry]);
  const change = async (next: boolean) => {
    if (busy) return;
    setBusy(true); setError("");
    try {
      const r = await libraryRequest<{ enabled: boolean }>({ action: "draftAccess", key: draftKey, enabled: next });
      setEnabled(r.enabled);
    } catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  };
  return <details className="settings-note">
    <summary>Agent access · {enabled === undefined ? "status unavailable" : enabled ? "shared" : "private"}</summary>
    <label><input type="checkbox" checked={enabled ?? false} disabled={busy || enabled === undefined} onChange={e => void change(e.target.checked)} /> Allow agents to read and edit this draft</label>
    <p>Shares saved revisions and source evidence with connected local agents until revoked or the backend restarts. Save your edits first. Agent saves create revisions; refresh the library to open them. Unsaved editor text stays here.</p>
    {error && <p role="status" className="mono-bad">Agent access: {error}</p>}
    {error && enabled === undefined && <button type="button" className="screen-chip cell-action" disabled={busy} onClick={() => setRetry(n => n + 1)}>retry access status</button>}
  </details>;
}
