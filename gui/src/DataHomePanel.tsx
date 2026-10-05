/**
 * The data folder, as a row of `/settings data`: its label and the folder it is now on the left,
 * the field and the one action on the right, what a switch means as a note under them. Drawn in
 * the same language as every other settings row, so the section reads as one screen.
 */
import { useEffect, useState } from "react";
import { flushDesktopPreferences } from "./desktop-preferences";

type Location = { path: string; identity: string; warning?: string | null };
export async function openDataHome(path: string): Promise<void> {
  await flushDesktopPreferences();
  const response = await fetch("/data-home", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ path }),
  });
  if (!response.ok) {
    const body = await response.json().catch(() => undefined) as { message?: string } | undefined;
    throw new Error(body?.message ?? "The data folder could not be opened. Your current folder remains open.");
  }
  // The native host changes every window only after the previous engine has finished closing.
  // No timer, guessed port, automatic retry or client-owned engine lifetime.
}

/** One global data folder, independent of whichever pane displays this control. */
export function DataHomePanel() {
  const desktop = typeof window !== "undefined" && window.__WES_DESKTOP__ === true;
  const [location, setLocation] = useState<Location>();
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [trouble, setTrouble] = useState(false);
  useEffect(() => {
    if (!desktop) return;
    const abort = new AbortController();
    void fetch("/data-home", { signal: abort.signal, cache: "no-store" }).then(async response => {
      if (!response.ok) throw new Error("The current data folder could not be read.");
      const value = await response.json() as Location;
      if (!abort.signal.aborted) { setLocation(value); setPath(value.path); setMessage(value.warning ?? ""); setTrouble(false); }
    }).catch(error => { if (!abort.signal.aborted) { setMessage(String(error instanceof Error ? error.message : error)); setTrouble(true); } });
    return () => abort.abort();
  }, [desktop]);
  if (!desktop) return null;
  const unchanged = !location || !path.trim() || path.trim() === location.path;
  return <form className="settings-row" aria-label="Data folder" onSubmit={event => {
    event.preventDefault();
    if (busy || unchanged) return;
    setBusy(true); setMessage("Opening data folder…"); setTrouble(false);
    void openDataHome(path.trim()).then(() => {
      // Same canonical path (for example ~/...) needs no native navigation.
      setBusy(false); setMessage("Data folder opened.");
    }).catch(error => { setBusy(false); setMessage(error instanceof Error ? error.message : String(error)); setTrouble(true); });
  }}>
    <div className="settings-row-head">
      <span className="screen-label settings-label">Data folder</span>
      {location && <code className="mono-line settings-command mono-literal">{location.path}</code>}
    </div>
    <div className="settings-block">
      <div className="settings-field-row">
        <input className="settings-field" aria-label="Data folder path" value={path} onChange={event => setPath(event.target.value)}
          placeholder="~/.wes" disabled={busy} spellCheck={false} />
        <button type="submit" className={`screen-chip ${unchanged || busy ? "cell-action" : "chip-chosen"}`} disabled={busy || unchanged}>Open data folder</button>
      </div>
      <p className="settings-note">Workspaces, results, the API library and desktop preferences live in this folder.</p>
      <p className="settings-note">Opening another folder switches every pane, closes result windows and drops unsent drafts. Finish running work and close terminal sessions first. The folder is remembered next time.</p>
      {message && <p className={`mono-line settings-status ${trouble ? "mono-bad" : "mono-dim"}`} role="status">{message}</p>}
    </div>
  </form>;
}
