import { LogView } from "./LogView";
import { logShape } from "./log-identity";
import { typedJsonSummary } from "./JsonTree";
import { ProcessOutput, isProcessOutput } from "./ProcessOutput";
import "./builtins.css";
import type { Engine } from "../../engine";
import { InstanceView, isViewInstance } from "../../value-views/InstanceView";
import { valueViewModules } from "../../value-views/registry";
import type { InstanceDisplay } from "../../value-views/InstancePlacement";
/**
 * One held value, prepared once, presented for this block's width and mode, drawn by kind.
 *
 * Preparation is cached by result handle and workspace generation (`PreparedCache`), so a resize or
 * a view change re-runs only `present()`. What did not fit stays inside this block's box, as its
 * last line: the counts belong to the value that left them out, not to the cell (a cell of several
 * results would otherwise pool them). Only the value's offers go up to the cell, so the chips and
 * keys name the views this value admits.
 *
 * Asking to see something must show it, without the rest of the block changing under the hand:
 * a value opened by hand (`▸ rows`, `▸` on a cut line) is drawn whole in a viewport of its own
 * while the block keeps its preview form; clicking the counts line presents the block whole, in
 * its expanded form. Both last until the cell's view changes.
 */
import { useEffect, useLayoutEffect, useMemo, useReducer, useRef, useState, useSyncExternalStore } from "react";
import type { StoredValue } from "../../protocol";
import { PreparedCache } from "../../presentation/prepare";
import { present, tailOf } from "../../presentation/present";
import type { Registry } from "../../presentation/registry";
import { registryStore } from "../../presentation/registry-store";
import { tableViewStore } from "../../presentation/table-views";
import { expanded_lines, pageSize, window_lines, type Facts, type Mode } from "../../presentation/types";
import { useBlockReport } from "../Cell";
import { MonoLine } from "../MonoLine";
import { useColumns } from "./measure";
import { Presented, segments } from "./Presentation";
import { DeletePlanReview, isDeletePlan } from "../DeletePlanReview";
import { DatasetHostContext, datasetWithdrawals, WITHDRAWN_FACTS, WITHDRAWN_TYPE_LABEL } from "./dataset-source";

/** One preparation per handle and generation, shared by every place a value is shown. */
export const preparedValues = new PreparedCache();

/** The presentation registry in force: core entries and data-home overrides. */
export function useRegistry(): Registry {
  return useSyncExternalStore(registryStore.subscribe, registryStore.get, registryStore.get);
}

function locale(): { locale: string; timeZone: string } {
  try {
    const options = new Intl.DateTimeFormat().resolvedOptions();
    return { locale: options.locale, timeZone: options.timeZone };
  } catch {
    return { locale: "en-GB", timeZone: "UTC" };
  }
}

export interface ValueBlockProps {
  readonly collapsed?: boolean;
  readonly instanceDisplay?:InstanceDisplay;
  readonly engine?: Engine;
  readonly value: StoredValue;
  /** `${generation}:${handle}` — the preparation's identity. */
  readonly cacheKey: string;
  /** Source identity without the data revision. Separate mounted surfaces remain independent. */
  readonly bindingKey?: string;
  readonly facts?: Facts;
  readonly mode: Mode;
  /** This result's independent preview line budget. Ignored when expanded or in the window. */
  readonly lines?: number;
  /** Whether counts go to a cell's tail (in a cell) or are drawn under the value (in the window). */
  readonly inCell?: boolean;
  /**
   * The stored result this value was read from, for the session it was read in. Datasets inside
   * the value page through it; without it they show their descriptor only.
   */
  readonly stored?: { readonly handle: string; readonly generation: string };
  /** The result's workspace name, which management commands use to refer to it. */
  readonly name?: string;
}

const NO_FACTS: Facts = {};

export function ValueBlock(props:ValueBlockProps) {
  const data=props.value.data;
  const id=typeof data==="object"&&data!==null&&"id" in data?String(data.id):"view";
  const registry=useRegistry();
  /** A list whose presentation entry declares a log mapping reads as a log, like Docker's events. */
  const log=logShape(props.value,registry);
  return log ? <LogView value={props.value} mode={props.mode} identity={props.bindingKey} collapsed={props.collapsed} {...(log.mapping?{mapping:log.mapping}:{})}/> : isViewInstance(props.value) ? <div className="value-instance-block">{props.collapsed && <p className="mono-dim">View · ${id}</p>}<div hidden={props.collapsed}><InstanceView value={props.value} engine={props.engine} mode={props.mode}
    display={props.inCell===false?undefined:props.instanceDisplay??{label:`$${id}`}}/></div></div> : <DataValueBlock {...props}/>;
}
function DataValueBlock(props: ValueBlockProps) {
  const { engine, stored, collapsed = false, mode } = props;
  useSyncExternalStore(datasetWithdrawals.subscribe, datasetWithdrawals.snapshot, datasetWithdrawals.snapshot);
  const handle = stored?.handle, generation = stored?.generation;
  const source = useMemo(() => engine && handle !== undefined && generation ? { engine, handle, generation } : undefined, [engine, handle, generation]);
  const name = props.name;
  const host = useMemo(() => ({ ...(source ? { source } : {}), ...(name ? { name } : {}), mode, collapsed }), [source, name, mode, collapsed]);
  const frame = useRef<HTMLDivElement>(null);
  const height = useRef(0);
  const withdrawn = source !== undefined && datasetWithdrawals.has(source);
  useLayoutEffect(() => { if (!withdrawn && frame.current) height.current = frame.current.getBoundingClientRect().height; });
  useEffect(() => { if (withdrawn) preparedValues.forget(props.cacheKey, props.value); }, [withdrawn, props.cacheKey, props.value]);
  // Withdrawn access clears every detail the value showed; the block keeps its place and height.
  return <div ref={frame} className="value-block-frame">
    {withdrawn ? <WithdrawnBlock height={height.current} />
      : <DatasetHostContext.Provider value={host}>{isDeletePlan(props.value)
        ? <div className="value-block"><DeletePlanReview value={props.value} {...(name ? { name } : {})} collapsed={collapsed} /></div>
        : <PresentedValueBlock {...props} />}</DatasetHostContext.Provider>}
  </div>;
}

/** What stays of a value whose Dataset access was withdrawn: a generic notice, nothing read from it. */
function WithdrawnBlock({ height }: { readonly height: number }) {
  return <div className="value-block dataset-withdrawn" role="status" style={height > 0 ? { minHeight: `${height}px` } : undefined}>
    <MonoLine segments={[{ text: WITHDRAWN_TYPE_LABEL, role: "mono-dim" }, { text: " · ", role: "mono-faint" }, ...WITHDRAWN_FACTS]} className="value-line" />
  </div>;
}

function PresentedValueBlock({ value, cacheKey, bindingKey, collapsed = false, facts = NO_FACTS, mode, lines = 6, inCell = true }: ValueBlockProps) {
  const registry = useRegistry();
  const modules = useSyncExternalStore(valueViewModules.subscribe, valueViewModules.get, valueViewModules.get);
  const box = useRef<HTMLDivElement>(null);
  const columns = useColumns(box);
  const [, landed] = useReducer((n: number) => n + 1, 0);
  const [rows, setRows] = useState(pageSize());
  const [pages, setPages] = useState<ReadonlyMap<string, number>>(new Map());
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const [closed, setClosed] = useState<ReadonlySet<string>>(new Set());
  const [grown, setGrown] = useState(false);
  const [shown, setShown] = useState<ReadonlyMap<string, number>>(new Map());
  const [sorts,setSorts]=useState<ReadonlyMap<string,{column:string;descending:boolean}>>(new Map());
  const [filters, setFilters] = useState<ReadonlyMap<string, string>>(new Map());
  const tables = useSyncExternalStore(tableViewStore.subscribe, tableViewStore.get, tableViewStore.get);
  // Inspection state survives tier changes. The explicit reset returns a grown preview to its budget.
  useEffect(() => { setGrown(false); }, [mode]);
  const effective: Mode = mode === "preview" && grown ? "expanded" : mode;
  // A block that shows more than its preview by hand keeps that inside its own scroll box.
  const grownByHand = effective !== mode || (mode === "preview" && (open.size > 0 || shown.size > 0));
  const prepared = preparedValues.read(cacheKey, value, landed);
  const presentation = useMemo(() => present({
    prepared, facts, registry,
    context: {
      mode: effective, columns, rows, pages, open, closed, shown, filters, tables, sorts, density: "normal", ...locale(),
      lines: effective === "preview" ? lines : effective === "expanded" ? expanded_lines() : window_lines(),
    },
  }), [prepared, facts, registry, modules, effective, columns, rows, pages, open, closed, shown, filters, tables, sorts, lines]);
  // Past the preview the pager under the value counts the root's rows; the counts line says the rest.
  const counts = useMemo(() => segments(tailOf(presentation, effective !== "preview", true)), [presentation, effective]);
  const report = useBlockReport();
  const reported = presentation.offers.join();
  useEffect(() => {
    if (inCell) report({ counts: [], offers: presentation.offers });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reported, inCell]);
  useEffect(() => () => { if (inCell) report(undefined); }, [inCell, report]);
  const toggle = (path: string) => {
    const opened = open.has(path) || (!closed.has(path) && presentation.root !== undefined && isOpenByDefault(path, presentation.root));
    setOpen((was) => { const next = new Set(was); if (opened) next.delete(path); else next.add(path); return next; });
    setClosed((was) => { const next = new Set(was); if (opened) next.add(path); else next.delete(path); return next; });
  };
  // A table pages itself under itself; this pager is for a root list of items.
  const remaining = presentation.root.kind === "table" ? 0 : presentation.root.more?.rows ?? presentation.root.more?.items ?? 0;
  if (isProcessOutput(value)) return <ProcessOutput value={value} preparedData={prepared.data} mode={mode} collapsed={collapsed} whole={facts.whole !== false}/>;
  const summary=<div className="value-summary">
    {presentation.root.kind==="table" ? `${facts.whole===false ? "≥ " : ""}${presentation.root.total} rows · ${presentation.root.arrangement.columns.length} columns · { ${presentation.root.arrangement.columns.slice(0,3).join(", ")}${presentation.root.arrangement.columns.length>3 ? ", …" : ""} }`
      : presentation.root.kind==="view" ? <MonoLine segments={segments(presentation.root.summary)}/> : typedJsonSummary(prepared.data, prepared.type)}
  </div>;
  return <div ref={box} className={`value-block${grownByHand ? " value-block-grown" : ""}`} data-mode={effective}>
    {collapsed && summary}
    <div className="value-block-body" hidden={collapsed}>
    {grownByHand && <button className="cell-action" onClick={() => {setGrown(false);setRows(pageSize());setOpen(new Set());setClosed(new Set());setShown(new Map());setPages(new Map());}}>back to preview</button>}
    <Presented key={bindingKey ?? cacheKey} node={presentation.root} onSort={(path,column)=>setSorts(was=>new Map(was).set(path,{column,descending:was.get(path)?.column===column && !was.get(path)?.descending}))} onToggle={toggle} onPage={(path, page) => setPages(was => new Map(was).set(path, Math.max(0, page)))}
      onShowRows={(path, count) => setShown(was => new Map(was).set(path, count))}
      onFilter={(path, text) => {
        setFilters(was => { const next = new Map(was); if (text) next.set(path, text); else next.delete(path); return next; });
        // A new filter starts from its first rows.
        setShown(was => { if (!was.has(path)) return was; const next = new Map(was); next.delete(path); return next; });
      }} />
    {effective !== "preview" && remaining > 0 && (
      <button type="button" className="value-more" onClick={() => setRows((was) => was + pageSize())}>
        <MonoLine segments={[{ text: `+${remaining} ${presentation.root.more?.rows ? "rows" : "items"} · `, role: "mono-faint" }, { text: `show ${Math.min(pageSize(), remaining)} more`, role: "mono-ref" }]} />
      </button>
    )}
    {counts.length > 0 && (effective === "preview"
      ? <button type="button" className="value-more" aria-label={`Show what did not fit: ${counts.map((it) => it.text).join("")}`} aria-description="show this value whole" onClick={() => setGrown(true)}>
        <MonoLine segments={counts} className="value-counts" />
      </button>
      : <MonoLine segments={counts} className="value-counts" />)}
    </div>
  </div>;
}

/** Whether a path is drawn open without having been toggled: its node carries a folding control. */
function isOpenByDefault(path: string, root: import("../../presentation/types").PresentationNode): boolean {
  const walk = (node: import("../../presentation/types").PresentationNode): boolean => {
    if (node.path === path) return node.disclosure === "folds";
    if (node.kind === "table") return node.details?.some(row => row.some(detail => detail !== undefined && walk(detail))) ?? false;
    if (node.kind === "fields") return node.rows.some((row) => walk(row.node));
    if (node.kind === "nested" || node.kind === "stream") return node.body !== undefined && walk(node.body);
    if (node.kind === "view") return node.children.some(walk);
    if (node.kind === "process") return walk(node.stdout) || walk(node.stderr);
    return false;
  };
  return walk(root);
}
