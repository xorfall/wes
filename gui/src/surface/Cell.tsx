import { StreamBoundary, StreamControls, type StreamDisplayProps } from "./LiveView";
/** The Ledger cell. Execution evidence and returned data have independent zones. */
import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode, type MouseEvent } from "react";
import { ResultSize } from "./ResultSize";
import { ResultType } from "./ResultType";
import type { TypeShape } from "../protocol";
import type { CellView } from "../cells";
import type { OfferName } from "../presentation/types";
import { ViewHost } from "./ViewHost";
import { ErrorVerdict } from "./ErrorVerdict";
import { RUN_SLOTS, RunSlotValue, runBandOf } from "./RunVerdict";
import { DeleteWork, type DeleteWorkHandle } from "./DeleteWork";
import { MonoLine, lineText, type Segment } from "./MonoLine";
import type { PeekWhat } from "./peek";
import { primaryGlyph } from "../platform-keys";
import type { Glyph, Identity, SourceRow, VerdictField } from "./session-model";
import "./surface.css";
import "./cell.css";


export type Theme = "controls" | "keys";
export type CellState = "default" | "focus" | "stale" | "failed" | "pinned" | "live";

/**
 * Cell facts are specific to this command rather than the current global context.
 *
 * `effects` is the exception to "could have differed": a repeat that was confirmed although it
 * performs an external action again carries it per attempt, always.
 */
export type MarkKind = "environment" | "revision" | "grant" | "target" | "traced" | "effects";
export interface Mark {
  readonly kind: MarkKind;
  readonly text: string;
  readonly title?: string;
}

/**
 * What a cell can be asked to do. A missing callback is an action this cell does not have, and
 * neither its chip nor its key appears.
 */
export interface CellActions {
  readonly history?: (node?: string) => void;
  /** `⌫ delete` / `Shift+D`: review the authoritative deletion scope; confirmation is asked inside. */
  readonly deleteWork?: import("./DeleteWork").DeleteWorkActions;
  /** `r` — run the captured definition again. Asks first when `confirmRepeat` is given. */
  readonly repeat?: (acknowledgeEffects: boolean) => void;
  /** `b` — submit this source as a separate cell. */
  readonly branch?: () => void;
  /** `p` — keep this cell in view while the scrollback moves. */
  readonly pin?: () => void;
  /** `o` — open the whole result. */
  readonly open?: (node?: string) => void;
  /** `v` — open the result as JSON. */
  readonly json?: (node?: string) => void;
  /** `d` — open the details: the request, the plan, the grants. */
  readonly details?: (node?: string) => void;
  /** `e` — grow the command into the editor. */
  readonly edit?: () => void;
  /** `w` — open this result's own window at the view it offers (`⇄ http`, `⇄ chart`). */
  readonly view?: (name: string, node?: string) => void;
  /** ⌘click on the command band: the source in a read-only window. */
  readonly openSource?: () => void;
  /** ⌘click on the verdict, a block or the source: that piece in a plain window; `node` names the block's own result when the cell made several. */
  readonly peek?: (what: PeekWhat, node?: string) => void;
  /** `space` and the view control — preview → expanded → collapsed. */
  readonly cycle?: (node?: string) => void;
  readonly setView?: (view:CellView, node?: string)=>void;
  readonly setHeight?: (rows: number | undefined, node?: string) => void;
  /**
   * `x` — stop a running command, or the whole pipeline when there is more than one stage. A returned
   * promise is the engine's acknowledgement of the request: the cell says `cancel requested` until
   * the node's own state changes, and a refusal clears it. Neither is a claim the work has ended.
   * The acknowledgement belongs to the attempt it was sent for; a later attempt never shows it.
   */
  readonly cancel?: () => void | Promise<unknown>;
  /** `f` — stay with the newest line of a live cell. */
  readonly follow?: () => void;
  /** `c` — take the command away as text. */
  readonly copy?: () => void;
  readonly copyPath?: (node?: string) => void;
  /** `j` and `k` — the cell after and the cell before. */
  readonly next?: () => void;
  readonly previous?: () => void;
}

/** Why a repeat must be confirmed before it runs, and what it would disturb. */
export interface RepeatGuard {
  readonly what: string;
  readonly against?: string;
  readonly dependents: readonly string[];
  /** The previous attempt's outcome is unknown; every repeat question says so. */
  readonly unknownOutcome?: boolean;
}

/** One result, or execution evidence in the run zone. */
export interface CellBlock {
  readonly stream?: Omit<StreamDisplayProps,"children">;
  /** Finite-result observation status shares its existing header/verdict row. */
  readonly status?: ReactNode;
  readonly key: string;
  /** The node it shows; absent for a cell-level notice. */
  readonly identity?: Identity;
  /** Source row of that node, so blocks follow the fold. */
  readonly row?: number;
  /** `ProcessOutput · 94 ms` — the node's own facts; drawn only when a cell has several blocks. */
  readonly header?: readonly Segment[];
  readonly type?: TypeShape;
  /** The contract metadata captured with the value, if any; shown in the type popup. */
  readonly meta?: import("../value-meta").ValueMeta;
  readonly typeLabel?: string;
  readonly duration?: string;
  readonly view?: CellView;
  readonly height?: number;
  /** Open until the person closes it; closed blocks show their header with `▸`. */
  readonly open: boolean;
  readonly content: ReactNode;
  readonly zone?: "run" | "data";
  readonly hasValue?: boolean;
}

/** A block reports available views; omission counts stay with the data. */
export interface BlockReport {
  readonly counts: readonly Segment[];
  readonly offers: readonly OfferName[];
}

type Report = (key: string, report: BlockReport | undefined) => void;
const ReportContext = createContext<Report>(() => undefined);
/** The key of the block a component is drawn in, so available views apply to the selected result. */
const BlockKeyContext = createContext<string>("");

/** Blocks call this with their counts and offers; the cell's tail draws them. */
export function useBlockReport(): (report: BlockReport | undefined) => void {
  const report = useContext(ReportContext);
  const key = useContext(BlockKeyContext);
  return useCallback((value: BlockReport | undefined) => report(key, value), [report, key]);
}

export interface CellProps {
  readonly streamOutput?: boolean;
  /** A node of this cell is the stream source itself, not a consumer of another cell's stream. */
  readonly streamSource?: boolean;
  readonly outputIdentity?: string;
  readonly theme: Theme;
  /** Keep action footers visible, or use the cell action menu and shortcuts. */
  readonly tailKeys?: boolean;
  readonly state: CellState;
  readonly pinned?: boolean;
  /** Submitted command rows and their result identities. */
  readonly rows: readonly SourceRow[];
  readonly time?: string;
  readonly timestamp?: string;
  /** Characters of the submitted command. */
  readonly chars?: number;
  readonly verdict: readonly VerdictField[];
  /** Distinguishes identical diagnostics from separate execution attempts. */
  readonly attempt?: string;
  readonly blocks?: readonly CellBlock[];
  /** Tail lines the cell adds itself: `Bytes read as utf-8` is reported by blocks; this is for the rest. */
  readonly tail?: readonly Segment[];
  readonly marks?: readonly Mark[];
  readonly actions?: CellActions;
  readonly confirmRepeat?: RepeatGuard;
  /** More than one stage, so cancelling stops the pipeline and the verdict tail says so. */
  readonly pipeline?: boolean;
  readonly view?: CellView;
  readonly onFocus?: () => void;
  /** Names the cell for the people reading the scrollback with a screen reader. */
  readonly label?: string;
}

interface Chip {
  readonly action: keyof CellActions;
  readonly label: string;
  readonly key?: string;
  readonly verb: string;
  /** What the action does and does not affect, for its tooltip and accessible description. */
  readonly description?: string;
}

const DELETE_CHIP: Chip = { action: "deleteWork", label: "⌫ delete", key: "Shift+D", verb: "delete" };
const DELETE_KEY = "D";
/** Every key a cell answers to, whatever it offers. */
const KEYS: Record<string, keyof CellActions> = {
  j: "next", k: "previous", r: "repeat", b: "branch", p: "pin", e: "edit", o: "open", v: "json",
  h: "history", d: "details", w: "view", x: "cancel", c: "copy", y: "copyPath", f: "follow", " ": "cycle",
};
/** Offers that open a window view, in the order the chip prefers them. */
const LOCAL_OFFERS = new Set(["source", "copy", "follow"]);
/** Rows the command band shows before it folds. */
export const FOLD_ROWS = 4;

const GLYPH: Record<Glyph, Segment> = {
  ready: { text: "✓", role: "mono-ok" }, failed: { text: "✗", role: "mono-bad" }, skipped: { text: "○", role: "mono-faint" },
  running: { text: "●", role: "mono-meta" }, stopped: { text: "■", role: "mono-warn" }, cancelled: { text: "✗", role: "mono-warn" },
  stale: { text: "~", role: "mono-warn" }, pending: { text: "·", role: "mono-faint" }, recipe: { text: "◇", role: "mono-meta" },
};

/** Verdict states that are not an ordinary success or failure of the latest attempt. */
const EXCEPTIONAL = ["cancelled","stopped","waiting","skipped","outcome unknown","not run"];
/** Run actions whose availability depends on whether work is running or waiting. */
const LIFECYCLE: ReadonlySet<keyof CellActions> = new Set(["repeat","branch","cancel","follow"]);

/**
 * Whether a cell's work is running or waiting, and what running it again would be called.
 *
 * One decision for the footer, the action menu, the keys and the repeat confirmation. It is read
 * from the identity glyphs and the verdict the cell already shows, because a waiting node need not
 * make the cell `live`. Running or waiting work is never repeated or branched; it can be cancelled.
 */
export interface RunAvailability {
  readonly said: string;
  readonly running: boolean;
  readonly working: boolean;
  readonly failure: boolean;
  readonly repeatVerb: string;
}
export function runAvailability(state: CellState, verdict: readonly VerdictField[], identities: readonly Identity[]): RunAvailability {
  const said = lineText(runBandOf(verdict).slots.state);
  const stopped = identities.some(node => node.glyph === "stopped");
  const running = identities.some(node => node.glyph === "running") || (state === "live" && !stopped);
  const waiting = identities.some(node => node.glyph === "pending") || said === "waiting";
  const failure = state === "failed" && !EXCEPTIONAL.includes(said);
  const previous = verdict.some(field => lineText(field.segments) === "previous results shown");
  const repeatVerb = said === "not run" ? previous ? "retry" : "run"
    : said === "outcome unknown" ? "repeat…" : said === "stopped" ? "restart…" : failure ? "retry" : state === "stale" ? "refresh" : "repeat";
  return { said, running, working: running || waiting, failure, repeatVerb };
}
/** Whether a run-lifecycle action is meaningful now; other actions are decided by their callback. */
function lifecycleAllows(action: keyof CellActions, run: RunAvailability): boolean {
  if (!LIFECYCLE.has(action)) return true;
  if (action === "cancel") return run.working;
  if (action === "follow") return run.running;
  return !run.working;
}

/** The text naming this command’s result identities: `✓ $name ✓ id7`. */
export function identityText(nodes: readonly Identity[]): string {
  return nodes.map((node) => `${GLYPH[node.glyph].text} ${node.label}`).join(" ");
}

/** Ledger: command band, run evidence, data and two bottom action groups. */
export function Cell({ streamOutput = false, streamSource = false, pipeline = false, outputIdentity, theme, tailKeys = true, state, pinned = false, rows, time, timestamp, verdict, attempt,
  blocks = [], tail = [], marks = [], actions = {}, confirmRepeat, view = "preview", onFocus, label }: CellProps) {
  const [asking, setAsking] = useState(false);
  const [cancelling, setCancelling] = useState<{ readonly token: number; readonly scope: string; readonly phase: "sending" | "requested" }>();
  // Each request's token; replacing the attempt or generation, ending the work or unmounting retires it.
  const cancelToken = useRef(0);
  const [menu, setMenu] = useState(false);
  const closeMenu = () => { setMenu(false); section.current?.focus?.(); };
  const [unfolded, setUnfolded] = useState(false);
  const [commandOverflow, setCommandOverflow] = useState(false);
  const command = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const element = command.current;
    if (!element) return;
    const measure = () => {
      const leading = parseFloat(getComputedStyle(element).lineHeight);
      setCommandOverflow(element.scrollHeight > 6 * (Number.isFinite(leading) ? leading : 21) + 1);
    };
    measure();
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(measure);
    observer?.observe(element);
    return () => observer?.disconnect();
  }, [rows, unfolded]);
  const [target, setTarget] = useState<string>();
  const [typePopup, setTypePopup] = useState<string>();
  const [reports, setReports] = useState<ReadonlyMap<string, BlockReport>>(new Map());
  const section = useRef<HTMLElement>(null);
  const deletion = useRef<DeleteWorkHandle>(null);
  const data = blocks.filter(block => block.zone !== "run");
  const selected = data.find(block => block.key === target) ?? data.at(-1);
  const selectedNode = selected?.identity?.id;
  const several = data.length > 1;
  const identities = rows.flatMap(row => row.nodes).filter((node, at, all) => all.findIndex(it => it.id === node.id) === at);
  const hasValue = selected !== undefined && selected.hasValue !== false;
  const report = useCallback<Report>((key, value) => setReports(was => {
    const old = was.get(key);
    if (value === undefined ? old === undefined : old && lineText(old.counts) === lineText(value.counts) && old.offers.join() === value.offers.join()) return was;
    const next = new Map(was); if (value) next.set(key, value); else next.delete(key); return next;
  }), []);
  const offers = [...(reports.get(selected?.key ?? "")?.offers ?? [])];
  const viewName = offers.find(name => !LOCAL_OFFERS.has(name));
  const availability = runAvailability(state, verdict, identities);
  // The request's acknowledgement is not the outcome: the node's state says when it is cancelled.
  // Without an attempt identity there is nothing to bind an acknowledgement to, so none is shown.
  const cancelScope = attempt === undefined ? undefined : `${outputIdentity ?? ""}\u0000${attempt}`;
  useEffect(() => { cancelToken.current++; setCancelling(undefined); }, [cancelScope, availability.working]);
  useEffect(() => () => { cancelToken.current++; }, []);
  // Filtered during render, so even the first render of a replaced attempt shows no earlier request.
  const cancelPhase = availability.working && cancelling?.scope === cancelScope ? cancelling?.phase : undefined;
  const requestCancel = () => {
    if (cancelPhase) return;
    const token = ++cancelToken.current;
    let sent: unknown;
    try { sent = actions.cancel?.(); } catch { setCancelling(undefined); return; }
    if (!(sent instanceof Promise) || cancelScope === undefined) return;
    const scope = cancelScope;
    const current = (was: typeof cancelling) => was?.token === token && was.scope === scope && cancelToken.current === token;
    setCancelling({ token, scope, phase: "sending" });
    sent.then(() => setCancelling(was => current(was) ? { token, scope, phase: "requested" } : was),
      () => setCancelling(was => current(was) ? undefined : was));
  };
  const repeatable = Boolean(actions.repeat) && lifecycleAllows("repeat", availability);
  // A question asked before the work started running or waiting is not asked again afterwards.
  useEffect(() => { if (!repeatable) setAsking(false); }, [repeatable]);
  const repeat = (confirmed: boolean) => {
    if (!repeatable) { setAsking(false); return; }
    if (confirmRepeat && !confirmed) {setAsking(true);section.current?.focus();return;}
    setAsking(false); actions.repeat!(Boolean(confirmRepeat));
  };
  const run = (action: keyof CellActions) => {
    if (action === "repeat") return repeat(false);
    if (action === "deleteWork") return deletion.current?.review();
    if (action === "cancel") return requestCancel();
    if (action === "view") return void (viewName && actions.view?.(viewName, selectedNode));
    if (["open", "json", "details", "history", "copyPath", "cycle"].includes(action)) return (actions[action] as ((node?: string) => void) | undefined)?.(selectedNode);
    (actions[action] as (() => void) | undefined)?.();
  };
  /** Previous/next cycles through every result, unreadable ones included: their history and details stay targetable. */
  const stepTarget = (by: number) => {
    if (!data.length) return;
    const at = data.indexOf(selected!);
    setTarget(data[(at + by + data.length) % data.length]!.key);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    const element = event.target as HTMLElement | undefined;
    if (!event.defaultPrevented && (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "m" && !element?.closest?.("input,textarea,[contenteditable=true]")) {
      return act(event, () => setMenu(was => !was));
    }
    if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey || element?.closest?.("input,textarea,[contenteditable=true],button,a,select,[role=tab]") || event.target && event.target!==event.currentTarget) return;
    if (asking && confirmRepeat && repeatable) {
      if (event.key === "Escape") act(event, () => setAsking(false));
      if (event.key === "Enter") act(event, () => repeat(true));
      return;
    }
    if (event.key === "[" || event.key === "]") return act(event, () => stepTarget(event.key === "[" ? -1 : 1));
    if (event.key === DELETE_KEY) {
      if (event.shiftKey && !event.repeat && actions.deleteWork) act(event, () => run("deleteWork"));
      return;
    }
    const action = KEYS[event.key];
    if (["open","json","cycle","view","copyPath"].includes(action ?? "") && !hasValue) return;
    if (action === "view" && !viewName) return;
    if (action && actions[action] && lifecycleAllows(action, availability)) act(event, () => run(action));
  };
  const runVerdict = data.some(block => block.type || block.typeLabel) ? verdict.filter(field => field.zone !== "data") : verdict;
  const band = runBandOf(runVerdict);
  const { said, failure: isFailure } = availability;
  const unknownOutcome = said === "outcome unknown";
  // `…` promises a question; an unguarded repeat of an unknown outcome runs at once and says so.
  const repeatVerb = unknownOutcome && !confirmRepeat ? "repeat" : availability.repeatVerb;
  const runState=EXCEPTIONAL.includes(said) ? said.replaceAll(" ","-") : state;
  // Stream evidence comes from the engine's node metadata, not from a running node beside an old value.
  const streaming = streamOutput && identities.some(node=>node.glyph==="running");
  const stages = `all ${identities.length} stages of this cell`;
  const pinChip: Chip = { action:"pin",label:pinned ? "p unpin" : "p pin",key:"p",verb:pinned ? "unpin" : "pin" };
  const editChip: Chip = { action:"edit",label:"e edit",key:"e",verb:"edit" };
  // An unknown outcome is reviewed before anything is repeated; nothing repeats on its own.
  const reviewChip: Chip | undefined = unknownOutcome && identities.length ? { action:"details",label:"d review outcome",key:"d",verb:"review outcome",
    description:"Open what the engine recorded about this attempt. Runs nothing." } : undefined;
  const runChips: Chip[] = (availability.working ? [
    pipeline ? { action:"cancel",label:streaming ? "x stop pipeline" : "x cancel pipeline",key:"x",verb:streaming ? "stop pipeline" : "cancel pipeline",
        description:`Ask the engine to cancel ${stages}${streaming && streamSource ? ", including its stream source" : ""}.${streaming ? " Holding or closing the display does not stop them." : ""}` }
      : streaming && streamSource ? { action:"cancel",label:"x stop source",key:"x",verb:"stop source",description:"Cancel this source run. Holding or closing the display does not stop it." }
      : streaming ? { action:"cancel",label:"x stop",key:"x",verb:"stop",description:"Cancel this cell's run. A stream source in another cell is not cancelled. Holding or closing the display does not stop it." }
      : { action:"cancel",label:"x cancel",key:"x",verb:"cancel",description:"Ask the engine to cancel this run." },
    { action:"follow",label:"f follow",key:"f",verb:"follow",description:"Keep the scrollback at the newest output. The run is not affected." },
    editChip, pinChip,
  ] satisfies Chip[] : [
    ...(reviewChip ? [reviewChip] : []),
    { action:"repeat",label:`r ${repeatVerb}`,key:"r",verb:repeatVerb,
      ...(unknownOutcome ? { description:"Run the captured definition again as a new attempt. The previous outcome stays unknown." } : {}) },
    editChip,
    { action:"branch",label:"b branch",key:"b",verb:"branch" },
    pinChip,
    // Every host's `copy` writes the cell's submitted source, so the chip names that, not the error.
    ...(isFailure ? [{action:"copy" as const,label:"c copy source",key:"c",verb:"copy source",description:"Copy the submitted source of this run. The error stays shown in the cell."}] : []),
  ] satisfies Chip[]).filter(item => lifecycleAllows(item.action, availability));
  if (!reviewChip) runChips.push({action:"details",label:"d details",key:"d",verb:"details"});
  runChips.push({action:"history",label:"h history",key:"h",verb:"history"},{action:"openSource",label:"source",verb:"source"},DELETE_CHIP);
  const dataChips: Chip[] = [
    {action:"cycle",label:"space size",key:"Space",verb:"size"},
    {action:"open",label:"o inspect",key:"o",verb:"inspect"},
    {action:"json",label:"v json",key:"v",verb:"json"},
    {action:"copyPath",label:"y copy path",key:"y",verb:"copy path"},
    ...(viewName ? [{action:"view" as const,label:`w ${viewName}`,key:"w",verb:viewName}] : []),
  ];
  const chip = (item: Chip, disabled = false) => <button key={item.action} type="button" className="cell-action"
    aria-label={item.label} aria-keyshortcuts={item.key} title={item.description} aria-description={item.description} disabled={disabled} onClick={() => { closeMenu(); run(item.action); }}>
    {theme === "keys" ? <><kbd>{item.key === "Space" ? "space" : item.key}</kbd> {item.verb}</> : item.verb}
  </button>;
  const actionGroups = () => <>
    <div role="group" aria-label="Run actions"><span className="cell-action-group-name">Run</span>{runChips.filter(item => actions[item.action]).map(item => chip(item, item.action === "cancel" && cancelPhase !== undefined))}</div>
    {(hasValue || several) && <div role="group" aria-label="Data actions"><span className="cell-action-group-name">Data</span>{several && <span className="cell-target"><button aria-label="Previous result" onClick={() => stepTarget(-1)}>‹</button><span className="cell-target-name" title={selected?.identity?.label}>{selected?.identity?.label ?? "result"}</span><button aria-label="Next result" onClick={() => stepTarget(1)}>›</button></span>}{hasValue && dataChips.filter(item => actions[item.action]).map(item => chip(item))}</div>}
  </>;
  const shownRows = rows.length > FOLD_ROWS && !unfolded ? [0,1,-1,rows.length-1] : rows.map((_, at) => at);
  const peekSource = (event: MouseEvent<HTMLElement>) => {
    if (!event.metaKey || event.button !== 0) return;
    event.preventDefault(); event.stopPropagation(); if (actions.peek) actions.peek("source"); else actions.openSource?.();
  };
  const counts = [...reports.values()].flatMap(value => value.counts);
  return <section className={`cell cell-${state}${data.some(block => block.key === typePopup) ? " cell-type-open" : ""}`} data-theme={theme} data-state={state} data-view={view}
    data-cell={label} tabIndex={0} aria-label={label} ref={section} onFocus={onFocus} onKeyDown={onKeyDown}>
    <div className="cell-frame">
      <header className="cell-band" aria-label="Command source">
        <div className="cell-command">
          <div className={`cell-command-clip${unfolded ? " cell-command-unfolded" : ""}`}><div ref={command}>
          {shownRows.map(at => at < 0 ? <div key="fold" className="cell-fold-marker mono-faint">… {rows.length-3} lines folded</div>
            : <div key={at} className="cell-source" onClick={peekSource}>
              <span className="cell-prompt" aria-hidden="true">{at === 0 ? "❯" : rows.length > 1 ? at+1 : ""}</span>
              <MonoLine segments={rows[at]!.segments} className="cell-source-line" sourceWrap />
            </div>)}
          </div></div>
          {(rows.length > FOLD_ROWS || commandOverflow) && <button className="cell-fold" aria-expanded={unfolded} onClick={() => setUnfolded(was => !was)}>{unfolded ? "▾ fold command" : "▸ show full command"}</button>}
        </div>
      </header>
      <section className={`cell-run cell-run-${runState}`} data-zone="run" aria-label="Run">
        <div className="cell-verdict-row" role="group" aria-label="Result status and type" onClick={event => {if(event.metaKey)actions.peek?.(state === "failed" ? "error" : "type");}}>
          {RUN_SLOTS.map(slot => <RunSlotValue key={slot} slot={slot} segments={band.slots[slot]} />)}
          <div className="cell-run-marks">
            {cancelPhase && <span className="cell-cancel-status mono-warn" role="status" title="The engine has not yet recorded the run as cancelled.">{cancelPhase === "sending" ? "requesting cancel…" : "cancel requested"}</span>}
            {marks.map(mark => <span key={mark.kind} className={`cell-mark badge-${mark.kind}`} title={mark.title ?? mark.text}>{mark.text}</span>)}
          </div>
          {time ? <time className="cell-time" dateTime={timestamp} title={timestamp ?? time}>{time}</time> : <span className="cell-time" aria-hidden="true" />}
          {band.notes.length > 0 && (state === "failed"
            ? <div className="cell-run-notes"><ErrorVerdict key={attempt} segments={band.notes} onPeek={actions.peek ? () => actions.peek?.("error") : undefined} /></div>
            : <MonoLine className="cell-run-notes" segments={band.notes} />)}
        </div>
        {blocks.filter(block => block.zone === "run").map(block => <div key={block.key}>{block.content}</div>)}
      </section>
      <ReportContext.Provider value={report}>
        {data.map(block => {
          const size = block.view ?? view;
          const node = block.identity?.id;
          return <StreamBoundary key={block.key} stream={block.stream}><div className={`cell-result${selected?.key === block.key ? " cell-result-selected" : ""}`} data-node={node} data-view={size} data-glyph={block.identity?.glyph} role="group" aria-label={`Result ${block.identity?.label ?? block.key}, ${block.identity?.glyph ?? "value"}`}
            onFocus={() => setTarget(block.key)} onClick={event => {setTarget(block.key); if(event.metaKey && !(event.target as HTMLElement)?.closest?.("button,a,input,.result-type-popover,.result-resize"))actions.peek?.(block.identity?.glyph === "failed" ? "error" : "value",node);}}>
            <header className="result-header" aria-label={`Result ${block.identity?.label ?? block.key}`}>
              {block.identity && <span className="result-name" title={block.identity.label} aria-label={`${block.identity.label}, ${block.identity.glyph}`}><MonoLine segments={[GLYPH[block.identity.glyph],{text:` ${block.identity.label}`,role:"mono-ref",variableName:block.identity.label.startsWith("$") ? block.identity.label : undefined}]} /></span>}
              <div className="result-typeline"><ResultType shape={block.hasValue === false ? undefined : block.type} meta={block.hasValue === false ? undefined : block.meta} label={block.typeLabel} identity={block.identity?.label ?? block.key} omitFieldCount={/\bfields\b/.test(lineText(block.header ?? []))} onOpenChange={open => setTypePopup(was => open ? block.key : was === block.key ? undefined : was)}/>
              {block.hasValue !== false && <ResultFacts header={block.header} duration={block.duration}/>}</div>
              <div className="result-controls">
                {block.stream && <StreamControls/>}
                {actions.setView && block.hasValue !== false && <ResultSize value={size} identity={block.identity?.label ?? block.key} disabled={false} onChange={next => {setTarget(block.key);actions.setView?.(next, node);}} />}
                {actions.open && block.hasValue !== false && <button type="button" className="cell-action result-inspect" aria-label={`Inspect ${block.identity?.label ?? block.key} in the pane`} data-tooltip="Inspect (o)" title="Inspect in the pane (o)" onClick={() => {setTarget(block.key);actions.open?.(node);}}><svg aria-hidden="true" viewBox="0 0 18 18" fill="none" stroke="currentColor" strokeWidth="1.5"><rect x="2" y="3" width="14" height="12" rx="1.6"/><path d="M10.5 3v12m2-8h1m-1 3h1"/></svg></button>}
              </div>
            </header>
            <section className={`cell-data${block.hasValue === false ? " cell-data-empty" : ""}`} data-zone="data" aria-label={`Data${block.identity ? ` of ${block.identity.label}` : ""}`}>
              {block.status !== undefined && <div className="cell-data-status">{block.status}</div>}
              <BlockKeyContext.Provider value={block.key}><ViewHost resizeLabel={block.identity?.label ?? block.key} stream={Boolean(block.stream)} identity={`${outputIdentity}:${block.key}`} mode={size} rows={block.height} onResize={actions.setHeight && block.hasValue !== false ? rows => actions.setHeight?.(rows, node) : undefined}>{block.content}</ViewHost></BlockKeyContext.Provider>
            </section>
          </div></StreamBoundary>;
        })}
      </ReportContext.Provider>
      {(counts.length > 0 || tail.length > 0) && <div className="cell-counts"><MonoLine segments={[...counts,...tail]} /></div>}
      {asking && confirmRepeat && repeatable ? <div className="cell-confirm"><RepeatQuestion guard={confirmRepeat} verb={repeatVerb}/><button className="cell-action" onClick={() => repeat(true)}>{`confirm ${repeatVerb.replace("…","")}`}</button><button className="cell-action" onClick={() => setAsking(false)}>cancel</button></div>
        : tailKeys && <footer className="cell-actions">{actionGroups()}</footer>}
      {menu && <CellActionMenu onClose={closeMenu}>{actionGroups()}</CellActionMenu>}
      {actions.deleteWork && <DeleteWork key={attempt ?? label} ref={deletion} actions={actions.deleteWork} onDismiss={() => section.current?.focus?.()} />}
    </div>
  </section>;
}

/** `3 rows · 94 ms`: a separator only between facts that are present. */
function ResultFacts({ header, duration }: { readonly header?: readonly Segment[]; readonly duration?: string }) {
  const primary = lineText(header ?? []) !== "";
  if (!primary && !duration) return null;
  return <div className="result-facts" title={[lineText(header ?? []),duration].filter(Boolean).join(" · ")}>
    {primary && <MonoLine className="result-primary-fact" segments={header!}/>}
    {duration && <span className="result-duration">{primary ? " · " : ""}{duration}</span>}
  </div>;
}

function CellActionMenu({ children, onClose }: { children:ReactNode; onClose:()=>void }) {
  const dialog=useRef<HTMLDialogElement>(null);
  useLayoutEffect(()=>{dialog.current?.showModal?.();dialog.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();},[]);
  return <dialog ref={dialog} className="cell-action-dialog" aria-label="Cell actions" onCancel={event=>{event.preventDefault();onClose();}} onKeyDown={event=>{
    if(event.key==="Escape" || (event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==="m"){event.preventDefault();event.stopPropagation();onClose();}
  }}>
    <header><strong>Cell actions</strong><kbd>{primaryGlyph()}M</kbd><button className="cell-action" aria-label="Close cell actions" onClick={onClose}>Close</button></header>
    <div className="cell-action-menu">{children}</div>
  </dialog>;
}

/** Up to three dependents are named; after that the names stop being the point and the count is. */
export function describeDependents(dependents: readonly string[]): string {
  if (dependents.length === 0) return "";
  const noun = dependents.length === 1 ? "dependent" : "dependents";
  if (dependents.length <= 3) return `${dependents.length} ${noun}: ${dependents.join(" ")}`;
  return `${dependents.length} ${noun}`;
}

/** The question a guarded repeat asks, in place of the tail. */
export function RepeatQuestion({ guard, verb = "repeat" }: { readonly guard: RepeatGuard; readonly verb?: string }) {
  const dependents = describeDependents(guard.dependents);
  const named = verb.replace("…", "");
  const sentence: Segment[] = [
    { text: named === "run" ? `▶ run ${guard.what}` : `↻ ${named} runs ${guard.what} again`, role: "mono-warn" },
    ...(guard.against ? [{ text: ` against ${guard.against}`, role: "mono-warn" } as Segment] : []),
    ...(dependents ? ([{ text: " · ", role: "mono-faint" }, { text: dependents, role: "mono-warn" }] as Segment[]) : []),
    ...(guard.unknownOutcome ? ([{ text: " · ", role: "mono-faint" }, { text: "the previous attempt's outcome is unknown", role: "mono-warn" }] as Segment[]) : []),
  ];
  const keys: Segment[] = [
    { text: "   ", role: "mono-faint" }, { text: "⏎", role: "mono-ref" }, { text: " confirm", role: "mono-dim" },
    { text: " · ", role: "mono-faint" }, { text: "esc", role: "mono-ref" }, { text: " cancel", role: "mono-dim" },
  ];
  return (
    <div className="cell-question">
      <MonoLine segments={sentence} className="cell-question-said" />
      <MonoLine segments={keys} className="cell-question-keys" />
    </div>
  );
}

/** A key the cell answers to is a key the page does not also get to act on. */
function act(event: KeyboardEvent<HTMLElement>, what: () => void) {
  event.preventDefault();
  event.stopPropagation();
  what();
}
