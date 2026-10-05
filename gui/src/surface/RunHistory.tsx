import { stringifyExactJson } from "../exact-json";
import { useEffect, useRef, useState } from "react";
import type { Engine } from "../engine";
import type { StoredValue, WorkHistory, WorkRun, WorkRunDetail } from "../protocol";
import type { Workspace } from "../workspace";
import { ResultReadError } from "../result-reader";
import { ValueView } from "../views/Result";
import type { Identity } from "./session-model";
import "./run-history.css";

export interface RunHistoryContext {
  readonly engine: Engine;
  readonly workspace: Workspace;
  readonly generation: string | undefined;
}
export interface RunHistoryTarget extends RunHistoryContext {
  readonly cell: string;
  readonly nodes: readonly Identity[];
  readonly initialNode?:string;
}
const message = (error: unknown) => error instanceof Error ? error.message : String(error);
export const runState = (state: string) => state.toLowerCase() === "ready" ? "ok" : state.toLowerCase();
export const runKey = (run: WorkRun) => JSON.stringify([run.node, run.run, run.handle]);
export function orderedRuns(runs: readonly WorkRun[]) {
  return [...runs].sort((a, b) => b.at.localeCompare(a.at) || a.run.localeCompare(b.run));
}
function recorded(at: string) {
  const date = new Date(at);
  return Number.isNaN(date.getTime()) ? at : date.toLocaleString();
}

/** Reads evidence only. Selection names a fixed run; live workspace values never supply its content. */
export function RunHistory({ engine, workspace, generation, cell, nodes, initialNode, onClose }: RunHistoryTarget & { onClose: () => void }) {
  const [history, setHistory] = useState<WorkHistory>();
  const [failure, setFailure] = useState<string>();
  const [loading, setLoading] = useState(true);
  const [refresh, setRefresh] = useState(0);
  const [scope, setScope] = useState<string | undefined>(initialNode && nodes.some(node=>node.id===initialNode) ? initialNode : nodes.length === 1 ? nodes[0]!.id : undefined);
  const [family, setFamily] = useState(nodes.length === 0);
  const [selected, setSelected] = useState<WorkRun>();
  const [showDetail, setShowDetail] = useState(false);
  const [limit, setLimit] = useState(40);
  const [newRuns, setNewRuns] = useState(false);
  const previousRuns = useRef<Set<string>>();
  const panel = useRef<HTMLElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  const relevant = new Set([...nodes.map(node => node.id), ...(history?.runs.map(run => run.node) ?? [])]);
  const live = JSON.stringify(workspace.nodes.filter(node => relevant.has(node.id))
    .map(node => [node.id, node.state, node.publication?.run, node.handle]));
  const lastRecord = workspace.history.filter(event => event.event === "log" && relevant.has(event.record.node)).at(-1)?.record.id;
  useEffect(() => { close.current?.focus(); }, []);
  useEffect(() => {
    let active = true;
    setLoading(true); setFailure(undefined);
    void engine.workHistory(cell).then(value => {
      if (!active) return;
      const ids = new Set(value.runs.map(run => run.run));
      if (previousRuns.current && [...ids].some(id => !previousRuns.current!.has(id))) setNewRuns(true);
      previousRuns.current = ids;
      setHistory(value);
    }, error => { if (active) setFailure(message(error)); }).finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [engine, generation, cell, live, lastRecord, refresh]);
  const allNodes = [...new Set([...nodes.map(node => node.id), ...(history?.attempts.flatMap(attempt => attempt.nodes) ?? [])])];
  const name = (id: string) => nodes.find(node => node.id === id)?.label ?? workspace.nodes.find(node => node.id === id)?.name ?? id;
  const visible = orderedRuns(history?.runs ?? []).filter(run => family || run.node === scope);
  const current = (run: WorkRun) => workspace.nodes.find(node => node.id === run.node)?.publication?.run === run.run;
  const choose = (run: WorkRun) => { setSelected(run); setShowDetail(true); setNewRuns(false); };
  const changeScope = (id: string | undefined, relatives: boolean) => {
    setScope(id); setFamily(relatives); setSelected(undefined); setShowDetail(false); setLimit(40);
  };
  const back = () => {
    setShowDetail(false);
    panel.current?.querySelector<HTMLButtonElement>('[data-run-selected="true"]')?.focus();
  };
  return <section ref={panel} className={`run-history${showDetail ? " history-show-detail" : ""}`} aria-label="Run history"
    onKeyDown={event => {
      event.stopPropagation();
      if (event.key === "Escape") { event.preventDefault(); if (showDetail) back(); else onClose(); }
    }}>
    <header className="history-header">
      <strong>History</strong>
      <span>{family ? "Related work" : scope ? name(scope) : "Choose a node"}</span>
      <button className="cell-action" onClick={() => setRefresh(n => n + 1)} disabled={loading}>refresh history</button>
      <button ref={close} className="cell-action" onClick={onClose} aria-label="Close history">close</button>
    </header>
    <div className="history-scope" role="group" aria-label="History scope">
      {nodes.map(node => <button key={node.id} className={`cell-action${!family && scope === node.id ? " chip-chosen" : ""}`}
        aria-pressed={!family && scope === node.id} data-variable-name={node.label} onClick={() => changeScope(node.id, false)}>{node.label}</button>)}
      <button className={`cell-action${family ? " chip-chosen" : ""}`} aria-pressed={family} onClick={() => changeScope(undefined, true)}>related work{history ? ` · ${allNodes.length} nodes` : ""}</button>
    </div>
    {newRuns && <p className="history-notice" role="status">New runs recorded. Your selected run has not changed.</p>}
    {history && history.unconfirmedWrites !== "0" && <p className="history-notice" role="status">Some records could not be confirmed. This history may be incomplete.</p>}
    {failure && <p className="history-error" role="alert">Could not read history: {failure}</p>}
    {loading && <p role="status">Reading history…</p>}
    <div className="history-columns">
      <div className="history-list" aria-label="Recorded runs">
        {!family && !scope ? <p>Choose the node whose runs you want to inspect.</p> : <>
          {history && !visible.length && <p>No runs recorded{family ? " for this work" : " for this node"}.</p>}
          {visible.slice(0, limit).map(run => <button key={runKey(run)} type="button"
            className={`history-run${selected && runKey(selected) === runKey(run) ? " history-run-chosen" : ""}`}
            data-run-selected={selected && runKey(selected) === runKey(run) ? "true" : "false"}
            aria-pressed={selected && runKey(selected) === runKey(run) || false} onClick={() => choose(run)}
            onKeyDown={event => {
              if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
              event.preventDefault();
              const buttons = Array.from(panel.current?.querySelectorAll<HTMLButtonElement>(".history-run") ?? []);
              const index = buttons.indexOf(event.currentTarget);
              const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : Math.max(0, Math.min(buttons.length - 1, index + (event.key === "ArrowDown" ? 1 : -1)));
              buttons[next]?.focus();
            }}>
            {family && <span className="history-run-node">{name(run.node)} <small>{run.node}</small></span>}
            <time dateTime={run.at} aria-description={`Recorded ${run.at}`}>{recorded(run.at)}</time>
            <span className={`history-state history-state-${runState(run.state)}`}>{runState(run.state)}</span>
            <span aria-description={run.run}>{run.run.slice(0, 8)}</span>
            <small>{current(run) ? "current · " : ""}{run.protected ? "protected" : run.handle ? "open result" : "no result recorded"}{run.trace ? " · trace" : ""}</small>
          </button>)}
          {visible.length > limit && <button className="cell-action" onClick={() => setLimit(n => n + 40)}>show more · {visible.length - limit} remaining</button>}
          {family && history?.attempts.filter(attempt => !attempt.nodes.length).map(attempt => <details key={attempt.id} className="history-attempt">
            <summary>Submission without runs · {attempt.id}</summary>
            <pre aria-label="Recorded source">{attempt.source}</pre>
            {attempt.failure && <p className="history-error">{attempt.failure}</p>}
            {attempt.diagnostics.map((diagnostic, i) => <p key={i}>{diagnostic}</p>)}
          </details>)}
        </>}
      </div>
      <div className="history-detail">
        {selected ? <RunDetail key={`${generation}:${runKey(selected)}`} engine={engine} cell={cell} run={selected}
          current={current(selected)} nodeName={name(selected.node)} attempts={history?.attempts ?? []} onBack={back}
          onProtected={detail => { setHistory(old => old && { ...old, runs: old.runs.map(run => run.run === detail.run.run ? detail.run : run) }); }} />
          : <p>Select a run to inspect its recorded result.</p>}
      </div>
    </div>
  </section>;
}

function RunDetail({ engine, cell, run, current, nodeName, attempts, onBack, onProtected }: {
  engine: Engine; cell: string; run: WorkRun; current: boolean; nodeName: string; attempts: WorkHistory["attempts"];
  onBack: () => void; onProtected: (detail: WorkRunDetail) => void;
}) {
  const [detail, setDetail] = useState<WorkRunDetail>();
  const [error, setError] = useState<string>();
  const [revision, setRevision] = useState(0);
  const [protectError, setProtectError] = useState<string>();
  const [, redraw] = useState(0);
  const [reading, setReading] = useState(true);
  const title = useRef<HTMLHeadingElement>(null);
  const active = useRef(true);
  useEffect(() => { active.current = true; title.current?.focus(); return () => { active.current = false; }; }, []);
  useEffect(() => engine.onRunProtection(() => redraw(n => n + 1)), [engine]);
  useEffect(() => {
    let valid = true;
    setError(undefined); setReading(true);
    void engine.workRun(cell, run.run).then(value => {
      if (!valid) return;
      if (value.run.node !== run.node || value.run.run !== run.run || value.run.handle !== run.handle) {
        setError("The recorded result reference changed. Refresh history and select the run again."); setDetail(undefined); return;
      }
      setDetail(value);
    }, failure => { if (valid) setError(message(failure)); }).finally(() => { if (valid) setReading(false); });
    return () => { valid = false; };
  }, [engine, cell, run.run, run.node, run.handle, revision]);
  const protect = async () => {
    setProtectError(undefined);
    try {
      const value = await engine.protectRun(cell, run.run);
      if (active.current) { setDetail(value); onProtected(value); }
    } catch (failure) { if (active.current) setProtectError(message(failure)); }
  };
  const protection = engine.runProtection(run.run);
  const definition = attempts.find(attempt => attempt.nodes.includes(run.node) && !attempt.repeatOf);
  return <>
    <div className="history-detail-heading">
      <button className="cell-action" onClick={onBack}>← runs</button>
      <h3 tabIndex={-1} ref={title}>Run {run.run.slice(0, 8)}</h3>
      <span className={`history-state history-state-${runState(run.state)}`}>{runState(run.state)}</span>
    </div>
    <p className="history-metadata">{nodeName} · <span aria-description="Node id">{run.node}</span><br />Recorded {recorded(run.at)}</p>
    <p className="history-notice">{current ? "Current run" : "Past run"} · read-only inspection</p>
    {reading && <p role="status">Reading run details…</p>}
    {error && <p className="history-error" role="alert">Could not read this run: {error}</p>}
    {error && <button className="cell-action" onClick={() => setRevision(n => n + 1)} disabled={reading}>read again</button>}
    {detail && <>
      {detail.error && <div className="history-error" role="status"><strong>{detail.error.code}</strong><pre>{detail.error.message}</pre>
        {detail.error.issues.map((issue, i) => <p key={i}>{issue.path} · {issue.code} · {issue.message}</p>)}</div>}
      <HistoricalResult engine={engine} run={detail.run} canProtect={detail.canProtect} protection={protection} onProtect={() => void protect()} />
      {protectError && <p className="history-error" role="alert">{protectError}</p>}
      {protection === "uncertain" && <div className="history-notice" role="status">Protection is unconfirmed. The result may already be protected.
        <button className="cell-action" disabled={reading} onClick={() => setRevision(n => n + 1)}>check protection</button>
      </div>}
      <details className="history-disclosure"><summary>Recorded definition</summary>
        <pre aria-label="Recorded source">{detail.definition.text}</pre>
        {definition && <p className="history-metadata">Definition {definition.id}{definition.revisionOf ? ` · revision of ${definition.revisionOf}` : ""}</p>}
        <p>{detail.contextNote}</p>
        <details><summary>Captured references and metadata</summary><pre>{stringifyExactJson(detail.definition, 2)}</pre></details>
      </details>
      <details className="history-disclosure"><summary>Trace · {detail.trace ? "recorded" : "not recorded"}</summary>
        {detail.trace ? <ValueView value={detail.trace} cacheKey={`history-trace:${run.node}:${run.run}`} /> : <p>{detail.traceNote}</p>}
      </details>
      <details className="history-disclosure"><summary>Run identity</summary><pre>{run.run}</pre><p>Node {run.node}</p></details>
    </>}
  </>;
}

function HistoricalResult({ engine, run, canProtect, protection, onProtect }: {
  engine: Engine; run: WorkRun; canProtect: boolean; protection: "pending" | "uncertain" | undefined; onProtect: () => void;
}) {
  const [value, setValue] = useState<StoredValue>();
  const [error, setError] = useState<unknown>();
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let valid = true;
    setValue(undefined); setError(undefined);
    if (run.handle) void engine.fetch(run.handle).then(result => { if (valid) setValue(result); }, failure => { if (valid) setError(failure); });
    return () => { valid = false; };
  }, [engine, run.handle, revision]);
  const unavailable = error instanceof ResultReadError && ["VALUE_UNAVAILABLE", "VALUE_PRIVATE_UNAVAILABLE"].includes(error.problem?.code ?? "");
  return <div className="history-result">
    <div className="history-result-heading"><strong>Result</strong>
      {run.protected ? <span>Protected</span> : protection === "pending" ? <span role="status">Protecting…</span>
        : value && canProtect && !protection ? <button className="cell-action" onClick={onProtect} aria-description="Protect this run’s result from automatic cleanup">protect result</button> : null}
    </div>
    {!run.handle ? <p>No result was recorded for this run.</p> : error ? <>
      <p className="history-error" role="alert">{unavailable ? "Result unavailable" : "Could not read result"}: {message(error)}</p>
      <button className="cell-action" onClick={() => setRevision(n => n + 1)}>read again</button>
    </> : value ? <ValueView value={value} cacheKey={`history:${run.node}:${run.run}:${run.handle}`} /> : <p role="status">Reading this run’s result…</p>}
  </div>;
}
