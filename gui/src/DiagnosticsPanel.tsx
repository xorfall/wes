/**
 * Local diagnostics, as rows of `/settings data`: the collection as option cards like any other
 * choice, then the state and the actions as facts and chips. One process setting shared by all
 * windows; no polling, no user data, no remote upload.
 */
import { useState, useSyncExternalStore } from "react";
import { controlDiagnostics, diagnosticsStatus, refreshDiagnostics, subscribeDiagnostics, type DiagnosticsAction } from "./local-telemetry";
import { MonoLine } from "./surface/MonoLine";
import { fact } from "./surface/settings-model";

const COLLECTION = [
  { name: "off", what: "nothing collected" },
  { name: "basic", what: "bounded logs and metrics, on this device" },
] as const;

export function DiagnosticsPanel() {
  const status = useSyncExternalStore(subscribeDiagnostics, diagnosticsStatus);
  const [busy, setBusy] = useState(false), [message, setMessage] = useState("");
  const [metricsShown, setMetricsShown] = useState(false);
  const act = async (action?: DiagnosticsAction) => {
    setBusy(true); setMessage("");
    try { if (action) await controlDiagnostics(action); else await refreshDiagnostics(); }
    catch (error) { setMessage(error instanceof Error ? error.message : "Diagnostics are unavailable."); }
    finally { setBusy(false); }
  };
  const chip = (label: string, onDo: () => void, disabled = false, on = false, hint?: string) =>
    <button type="button" className={`screen-chip ${on ? "chip-chosen" : "cell-action"}`} aria-pressed={on || undefined} disabled={disabled} aria-description={hint} onClick={onDo}>{label}</button>;
  const capturing = status?.mode === "diagnostic";
  return <>
    <div className="settings-row">
      <div className="settings-row-head">
        <span className="screen-label settings-label">Local diagnostics</span>
        <MonoLine segments={[{ text: status ? (status.forced ? "fixed by WES_TELEMETRY" : `collection ${status.saved_mode}`) : "not read yet", role: "mono-meta" }]} className="settings-command" />
      </div>
      <div className="settings-block">
        <div className="settings-options" role="radiogroup" aria-label="Telemetry collection">
          {COLLECTION.map((option) => {
            const here = status?.saved_mode === option.name;
            return <button key={option.name} type="button" className={`settings-option ${here ? "option-chosen" : "option"}`} role="radio" aria-checked={here}
              disabled={!status || busy || status.forced}
              onClick={() => { if (!here) void act({ action: "set", mode: option.name }); }}>
              <MonoLine segments={[{ text: option.name, role: here ? "mono-ink-strong" : "mono-ink" }]} className="settings-option-name" />
              <MonoLine segments={[{ text: option.what, role: "mono-faint" }]} className="settings-option-what" />
            </button>;
          })}
        </div>
        <p className="settings-note">Nothing is sent to an external service. Command history and explicit provider traces have controls of their own.</p>
      </div>
    </div>
    <div className="settings-row">
      <div className="settings-row-head">
        <span className="screen-label settings-label">Capture</span>
      </div>
      <div className="settings-block">
        {status && <div className="settings-facts">
          <div role="status"><MonoLine segments={fact("now", capturing ? `diagnostic · up to ${Math.ceil(status.remaining_ms / 1000)} s left at last refresh, then ${status.saved_mode}` : status.mode)} className="settings-fact" /></div>
          <MonoLine segments={fact("records", `${status.dropped} dropped · ${status.suppressed} rate-limited · ${status.write_errors} write errors${status.writer_stopping ? " · a previous write is finishing" : ""}`)} className="settings-fact" />
          {status.previous_unclean && <MonoLine segments={[{ text: "the previous recorded process did not mark a clean stop (a force quit or power loss does this too)", role: "mono-warn" }]} className="settings-fact" />}
        </div>}
        <div className="settings-chips">
          {chip(capturing ? "Stop capture" : "Capture for 5 minutes", () => { void act({ action: capturing ? "stop" : "start" }); }, !status || busy || status.forced || status.writer_stopping, capturing, "collect more operation records for five minutes, then return to the saved mode")}
          {chip("Save diagnostics file", () => { void act({ action: "export" }); }, busy, false, "a bounded local JSON snapshot")}
          {chip("Clear saved logs", () => { void act({ action: "clear" }); }, !status || busy || status.mode !== "off" || status.writer_stopping, false, "only while collection is off")}
          {chip("Refresh diagnostics", () => { void act(); }, busy)}
          {status && status.metrics.operations.length > 0 && chip(metricsShown ? "▾ metrics" : "▸ metrics", () => setMetricsShown((was) => !was), false, metricsShown, "operations completed since collection started")}
        </div>
        {status && metricsShown && <div className="settings-facts">
          {status.metrics.operations.map(metric => <MonoLine key={metric.operation} className="settings-fact"
            segments={fact(metric.operation, `${metric.outcomes.reduce((n, [, count]) => n + count, 0)} completed · ${(metric.sum_us / 1000).toFixed(1)} ms in all`)} />)}
          <MonoLine segments={[{ text: "approximate concurrent snapshots; outcome counts and duration buckets are in the saved file", role: "mono-faint" }]} className="settings-fact" />
        </div>}
        {message && <p className="mono-line settings-status mono-bad" role="alert">{message}</p>}
      </div>
    </div>
  </>;
}
