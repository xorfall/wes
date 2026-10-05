import { useEffect, useState } from "react";
import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";
import { DataView } from "../surface/render/DataView";
import "./http-inspection.css";

type Fields = Record<string, unknown>;
const object = (v: unknown): v is Fields => typeof v === "object" && v !== null && !Array.isArray(v);
export function httpTrace(data: unknown): Fields | undefined {
  if (!object(data)) return undefined;
  const trace = object(data.trace) ? data.trace : data;
  return trace.schema === 1 && trace.profile === "http" && Array.isArray(trace.events) ? trace : undefined;
}
export function HttpInspection({ data }: { readonly data: unknown }) {
  const trace = httpTrace(data);
  if (!trace) return <p>Not an HTTP inspection.</p>;
  return <section className="http-inspection" aria-label="HTTP inspection">
    <dl><div><dt>Attempt</dt><dd>{String(trace.run)}</dd></div><div><dt>State</dt><dd>{String(trace.state)}</dd></div><div><dt>Storage</dt><dd>{String(trace.persistence)}</dd></div></dl>
    <p className="dim">Client observations · elapsed time from this attempt · bounded, redacted previews</p>
    <ol>{(trace.events as unknown[]).filter(object).map((event, index) => <li key={index}>
      <header><strong>{String(event.kind)}</strong><span>+{String(event.elapsedMs)} ms</span></header>
      <DataView data={event.details} lines={200} />
    </li>)}</ol>
    {Number(trace.dropped) > 0 && <p role="status">{String(trace.dropped)} observations omitted at the trace limit.</p>}
  </section>;
}
export function shouldPollTrace(state: string, trace: unknown): boolean {
  const snapshot = httpTrace(trace);
  return state === "pending" || state === "running" || snapshot?.state === "running";
}
export function LiveHttpInspection({ engine, node, run, state }: { readonly engine: Engine; readonly node: string; readonly run?: string; readonly state: string }) {
  const [open, setOpen] = useState(true);
  const [value, setValue] = useState<StoredValue>();
  const [error, setError] = useState<string>();
  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    setValue(undefined); setError(undefined);
    const poll = async () => {
      let again = false;
      try {
        const result = await engine.trace(node, controller.signal);
        if (!controller.signal.aborted) { setValue(result); setError(undefined); again = shouldPollTrace(state, result?.data); }
      }
      catch (error) { if (!controller.signal.aborted) setError(error instanceof Error ? error.message : "Inspection unavailable."); }
      if (!controller.signal.aborted && again) timer = setTimeout(() => void poll(), 500);
    };
    void poll();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [engine, node, run, state, open]);
  return <details className="live-http-inspection" open={open} onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>HTTP inspection</summary>
    {error ? <p role="status">{error}</p> : value ? <HttpInspection data={value.data} /> : <p>Waiting for trace evidence. Untraced or evicted attempts have no retained trace.</p>}
  </details>;
}
