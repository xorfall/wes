import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { applicationLog, formatLogs, type ApplicationLog } from "../application-log";
import "./application-logs.css";

export function ApplicationLogs({ log = applicationLog }: { log?: ApplicationLog }) {
  const records = useSyncExternalStore(log.subscribe, log.snapshot, log.snapshot);
  const latest = [...records].reverse().find(record => record.level !== "info");
  const [notice, setNotice] = useState<typeof latest>();
  useEffect(() => {
    const remaining = latest ? 8000 - (Date.now() - Date.parse(latest.time)) : 0;
    setNotice(remaining > 0 ? latest : undefined);
    if (remaining <= 0) return;
    const timer = setTimeout(() => setNotice(undefined), remaining);
    return () => clearTimeout(timer);
  }, [latest]);
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState("");
  const dialog = useRef<HTMLDialogElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (open) dialog.current?.showModal();
    else if (dialog.current?.open) dialog.current.close();
  }, [open]);
  const close = () => { setOpen(false); setCopied(""); trigger.current?.focus(); };
  return <span className="application-logs" onKeyDown={event => {
    event.stopPropagation();
    if (open && event.key === "Escape") { event.preventDefault(); close(); }
  }}>
    {notice && <span className="logs-notice" data-level={notice.level} role="status">{(notice.notice ?? [notice.message, notice.detail].filter(Boolean).join(" ")).slice(0, 500)}</span>}
    <button ref={trigger} type="button" className="logs-trigger" onClick={() => setOpen(true)} aria-haspopup="dialog">Logs{records.length ? ` (${records.length})` : ""}</button>
    <dialog ref={dialog} aria-label="Application logs" onCancel={event => { event.preventDefault(); event.stopPropagation(); close(); }} onClose={close}>
      <header><strong>Application logs</strong><button type="button" onClick={close} autoFocus>Close · esc</button></header>
      <p className="logs-help">This window · latest {log.limit} records · cleared when the window reloads. Shell input and output are not recorded. Persistent diagnostic export: /settings data.</p>
      <div className="logs-actions">
        <button type="button" disabled={!records.length} onClick={async () => {
          try { await navigator.clipboard.writeText(formatLogs(records)); setCopied("Copied."); }
          catch { setCopied("Copy failed. Select and copy the records below."); }
        }}>Copy logs</button>
        <button type="button" disabled={!records.length} onClick={() => { log.clear(); setCopied(""); }}>Clear</button>
        <span role="status">{copied}</span>
      </div>
      <div className="logs-records" tabIndex={0}>
        {!records.length && <p>No application diagnostics in this window.</p>}
        {[...records].reverse().map(record => <article key={record.id} data-level={record.level}>
          <div className="logs-meta"><time dateTime={record.time}>{record.time}</time> · {record.level} · <code>{record.code}</code>{record.count > 1 && ` · ×${record.count}`}</div>
          <div>{record.source} → {record.operation}</div>
          <div className="logs-context">workspace: {record.workspace ?? "unknown"} · pane: {record.pane ?? "—"} · terminal: {record.terminal ?? "not created"}</div>
          {record.generation && <div className="logs-context">session: {record.generation}</div>}
          {(record.cell || record.node) && <div className="logs-context">cell: {record.cell ?? "—"} · node: {record.node ?? "—"}</div>}
          <p>{record.message}</p>
          {record.detail && <p>Cause: {record.detail}</p>}
          {record.next && <p>Next: {record.next}</p>}
        </article>)}
      </div>
    </dialog>
  </span>;
}
