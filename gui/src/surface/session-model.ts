import { locationLines } from "../failure-text";
import { constructed, inputsCaptured, incompleteResult, observationStale, staleMessage, stoppedStream, updatePendingStatus } from "../workspace";
/**
 * The session, projected from what the engine said.
 *
 * The client's model is a workspace of nodes and a list of cells; the surface is a scrollback, a
 * prompt and two lines of context. This turns one into the other and nothing else: it decides what
 * each cell *is*, what its verdict reads, which facts it carries as marks, and which form its value
 * takes. Kept pure and away from the components, because the deciding is the part that can be wrong
 * and the drawing is not.
 *
 * Facts come from engine events. Run durations use matching start and completion log records;
 * fields without supporting evidence are omitted rather than guessed.
 */
import type { Cell as ClientCell } from "../cells";
import type { Workspace, WorkspaceNode } from "../workspace";
import type { CellState, Mark, RepeatGuard } from "./Cell";
import { documentLabel, documentOperation } from "./document-command";
import { commandSegments } from "./command-line";
import type { CellView, ResultArrangement } from "../cells";
import { summarize } from "../presentation/summary";
import { formatExecutionDuration } from "../presentation/format";
import type { Language } from "./language";
import type { Segment } from "./MonoLine";
import type { FormValue } from "./forms/form";
import { lifetimeActive, openLifetimeWord } from "./record-progress";

/** A node's state as the gutter says it: one glyph per node, the same vocabulary everywhere. */
export type Glyph = "ready" | "failed" | "skipped" | "running" | "stopped" | "cancelled" | "stale" | "pending" | "recipe";

/** Identity only — a state glyph and `$name` or the node id. */
export interface Identity {
  readonly id: string;
  readonly label: string;
  readonly glyph: Glyph;
}

/** One line of the command band and the nodes that line makes. */
export interface SourceRow {
  readonly segments: readonly Segment[];
  readonly nodes: readonly Identity[];
}

/** What a block needs to know about its node, without the value. */
export interface NodeView extends Identity {
  /** Row of the command band that makes this node, for source location metadata. */
  readonly row: number;
  /** The node's own type label, for its block header. */
  readonly type?: string;
  /** RUNNING → READY from the log records of its current run; absent when either is missing. */
  readonly durationMs?: number;
  /** The failure as a record: its code when the engine gave one, its message, its source spans. */
  readonly failure?: Failure;
}

/** One field of the verdict. `keep` fields (state, shape) never drop when the line is narrow. */
export interface VerdictField {
  readonly segments: readonly Segment[];
  readonly keep: boolean;
  /** A successful result's type/facts are drawn in its own header. */
  readonly zone?: "data";
  /** Stable execution columns. Unslotted fields are complete explanatory text. */
  readonly slot?: "state" | "duration" | "retention";
}

export interface SessionCell {
  readonly streamOutput?: boolean;
  /** The engine says one of its nodes' own call streams: cancelling it targets that source run. */
  readonly streamSource?: boolean;
  readonly id: string;
  readonly attempt?: string;
  readonly state: CellState;
  /** Arrangement stays independent of live, failed and stale status. */
  readonly pinned: boolean;
  /** The command as submitted, one line per row, with the nodes each line makes. */
  readonly rows: readonly SourceRow[];
  /** When it ran, `21:23`. */
  readonly time?: string;
  readonly timestamp?: string;
  /** Characters of the whole source, for source metadata. */
  readonly chars: number;
  /** Exact submitted command or document body, independent of its command label. */
  readonly source?: string;
  readonly documentNotice?: string;
  readonly previousAttemptResults?: boolean;
  readonly notices?: readonly { readonly severity: "info" | "warning"; readonly message: string }[];
  /** One block per node, in the order the cell made them. */
  readonly nodes: readonly NodeView[];
  /** Verdict fields: `state · shape · facts · time · retention`. */
  readonly verdict: readonly VerdictField[];
  readonly marks: readonly Mark[];
  /** What the cell's last result is, for the run-state rules that precede value shape. */
  readonly value?: FormValue;
  /** More than one stage, so cancelling stops the pipeline. */
  readonly pipeline: boolean;
  /** Semantic run activity for cancel versus repeat/branch; see `runActiveOf`. */
  readonly runActive: boolean;
  /** Why a repeat must be confirmed, when it must. */
  readonly guard?: RepeatGuard;
  /** The view control: preview, expanded or collapsed. */
  readonly view: CellView;
  readonly results?: Readonly<Record<string, ResultArrangement>>;
}

export interface SessionModel {
  /** `wes / sales-api · connected` */
  readonly top: readonly Segment[];
  readonly cells: readonly SessionCell[];
  /** `~ 2 results stale   /stale`, when something just went stale. */
  readonly note?: readonly Segment[];
  /** `env: DEV · rev 14 · grant 12 min · → acme-eu` */
  readonly context: readonly Segment[];
  /** `9 nodes · 3 kept · 2 stale` */
  readonly counts: readonly Segment[];
  readonly keys: readonly Segment[];
}

export interface SessionContext {
  readonly workspace: string;
  readonly environment?: string;
  readonly connection: "connecting" | "connected" | "reconnecting";
  readonly revision?: number;
  /** Minutes left on the grant this session is using, when there is one. */
  readonly grantMinutes?: number;
  /** The target commands are redirected to, when it is not the environment's own. */
  readonly target?: string;
  /** The environment a command runs against by default, so a cell only marks what differed. */
  readonly defaultEnvironment?: string;
}

export interface SessionInput {
  readonly language?: Language;
  readonly workspace: Workspace;
  readonly cells: readonly ClientCell[];
  readonly context: SessionContext;
  /** The cell the caret is in, when it is in one rather than in the prompt. */
  readonly focused?: string;
  /** Results fetched back from the engine, by handle. A form draws from these. */
  readonly held?: ReadonlyMap<string, import("../protocol").StoredValue>;
  /** Whether the scrollback is staying with the newest cell, which the footer says out loud. */
  readonly following?: boolean;
}

/** The nodes one cell made, in the order the cell made them. */
export function nodesOf(workspace: Workspace, cell: ClientCell): WorkspaceNode[] {
  return cell.nodes
    .map((id) => workspace.nodes.find((node) => node.id === id))
    .filter((node): node is WorkspaceNode => node !== undefined);
}

/**
 * What a cell is.
 *
 * In the order that matters: what is happening to it now, then what went wrong, then what has gone
 * out of date, then how the person arranged it. Focus is last because a cell that is stale and
 * focused is still stale — it shows what it is, and the caret's own wash says where the caret is.
 */
export function stateOf(
  cell: ClientCell,
  nodes: readonly WorkspaceNode[],
  focused: boolean,
  refusal?: string,
): CellState {
  if (cell.state === "running" || nodes.some((node) => node.state === "running")) return "live";
  /*
   * Cancelled wears the same chrome as failed and says a different word.
   *
   * There are six cell states and cancelling does not add a seventh: what a person needs from the
   * chrome is that this cell has no result, which is what `failed` shows. Whether that was a
   * failure or their own `x` is the verdict's business, and the verdict says it.
   */
  if (
    cell.state === "unanswered" ||
    refusal !== undefined ||
    nodes.some((node) => node.state === "failed" || (node.state === "cancelled" && !stoppedStream(node)))
  ) {
    return "failed";
  }
  if (nodes.some((node) => node.state === "stale" || stoppedStream(node))) return "stale";
  if (cell.pinned) return "pinned";
  return focused ? "focus" : "default";
}

/**
 * Why the engine would not run this attempt, when it would not.
 *
 * A command the engine refuses makes no node at all, so a cell that only looks at its nodes has
 * nothing to look at and reads as an ordinary empty result — which is how `:list sh` came to say
 * `ok · not kept` about a command that never ran. The refusal is on the attempt: the `planned`
 * event carries it, and the diagnostics say the same thing with a code. A submission the engine
 * refused before it started never reaches the event stream; its reason is kept on the cell.
 */
export function refusalOf(workspace: Workspace, cell: ClientCell): string | undefined {
  const failure = workspace.attemptFailures?.[cell.lastRun] ?? cell.submissionRefusal;
  const errors = cell.diagnostics.filter((diagnostic) => diagnostic.severity === "error");
  const message = failure || (errors.length ? `${errors[0]!.code}: ${errors[0]!.message}` : undefined);
  const hints = [...new Set(errors.flatMap(error => error.hints))];
  return message === undefined ? undefined : [message, ...hints.map(hint => `Hint: ${hint}`)].join("\n");
}

/** The shape field: the last node's type for one node, `N nodes` for several, the refusal when none ran. */
function shapeOf(state: CellState, nodes: readonly WorkspaceNode[], refusal?: string): string {
  const stopped = nodes.find((it) => it.state === "cancelled");
  if (stopped && nodes.length === 1) return stoppedStream(stopped) ? "stream stopped · last value" : stopped.cancellation?.reason ?? "";
  if (nodes.length > 1) return `${nodes.length} results`;
  const node = nodes[0];
  if (state === "failed") {
    if (refusal) return refusal;
    // The partial result is said beside the failure: the run did not complete, and nothing says it did.
    const partial = incompleteResult(node) ? "partial result · incomplete" : undefined;
    if (!node?.failure) return partial ?? "";
    return [headlineOf(failureOf(node.failure, node.failureRecord)),partial,errorRoute(node)].filter(Boolean).join(" · ");
  }
  if (node && ["pending","skipped"].includes(node.state) && node.waiting?.length)return node.waiting.map(wait=>wait.message).join(" · ");
  if (node?.state === "stale") return staleMessage(node)!;
  const behind = node && updatePendingStatus(node);
  if (behind) return behind;
  if (state === "live" && node?.interactive && node.conversationActive) return "interactive";
  const type = node?.type ?? "";
  return type === "Unknown" ? "" : type;
}

function errorRoute(node:WorkspaceNode):string|undefined {return node.state==="failed" && node.errorNames?.length ? `error → ${node.errorNames.map(name=>`$${name}`).join(", ")}`:undefined;}

/** `2 ok · 1 failed · 1 skipped` for several nodes; the value's own summary for one. */
function factsOf(nodes: readonly WorkspaceNode[], value: FormValue | undefined): Segment[][] {
  if (nodes.length > 1) {
    const count = (states: readonly string[]) => nodes.filter((node) => states.includes(node.state)).length;
    const facts: [number, string, Segment["role"]][] = [
      [nodes.filter((node) => node.state === "ready" && !lifetimeActive(node)).length, "ok", "mono-ok"],
      ...(["recording", "analyzing", "active"] as const).map((word): [number, string, Segment["role"]] =>
        [nodes.filter((node) => node.state === "ready" && lifetimeActive(node) && openLifetimeWord(node) === word).length, word, "mono-meta"]),
      [count(["running"]), "running", "mono-meta"],
      [count(["failed"]), "failed", "mono-bad"], [nodes.filter((node) => node.state === "cancelled" && stoppedStream(node)).length, "stopped", "mono-warn"],
      [nodes.filter((node) => node.state === "cancelled" && !stoppedStream(node)).length, "cancelled", "mono-warn"],
      [count(["skipped"]), "skipped", "mono-dim"], [count(["stale"]), "stale", "mono-warn"],
    ];
    const said = facts.filter(([n, word]) => n > 0 && !(word === "ok" && n === nodes.length)).map(([n, word, role]) => [{ text: `${n} ${word}`, role }]);
    if (nodes.some((node) => node.state === "cancelled" && stoppedStream(node))) said.push([{ text: "stream stopped · last value", role: "mono-warn" }]);
    const partial = nodes.filter((node) => incompleteResult(node)).length;
    if (partial) said.push([{ text: `${partial} incomplete · partial result${partial === 1 ? "" : "s"}`, role: "mono-warn" }]);
    if (nodes.some((node) => node.updatePending)) said.push([{ text: "newer input waiting", role: "mono-meta" }]);
    return said;
  }
  if (!value || value.state !== "ready") return [];
  const ROLE = { ok: "mono-ok", warn: "mono-warn", dim: "mono-dim" } as const;
  return summarize(value.type, value.data).map((run) => [{ text: run.text, role: ROLE[run.tone as keyof typeof ROLE] ?? "mono-dim" }]);
}

/**
 * How long it took, or how long it has been running.
 *
 * A running cell counts from the engine-recorded start. A finished one says what its log records say:
 * the earliest RUNNING to the latest end of its nodes' current runs. When a record is missing, the
 * field is absent rather than guessed.
 */
function timeOf(state: CellState, nodes: readonly WorkspaceNode[], durations: ReadonlyMap<string, Span>, now: Date): string {
  if (state === "live") {
    const started = nodes.map((node) => node.startedAt).find((at) => at !== undefined);
    if (started === undefined) return "";
    return formatExecutionDuration(Math.max(0, now.getTime() - new Date(started).getTime())).replace(/\.\d s$/, " s");
  }
  const spans = nodes.filter((node) => node.state !== "skipped").map((node) => durations.get(node.id));
  if (spans.length === 0 || spans.some((span) => span === undefined)) return "";
  const start = Math.min(...spans.map((span) => span!.start));
  const end = Math.max(...spans.map((span) => span!.end));
  return formatExecutionDuration(end - start);
}

/** When a node's current run started and ended, from the log's RUNNING and terminal records. */
export interface Span {
  readonly start: number;
  readonly end: number;
}

const TERMINAL = new Set(["READY", "FAILED", "CANCELLED"]);

/** Per node, the span of its latest run that has both records. */
export function durationsOf(workspace: Workspace): Map<string, Span> {
  const currentRuns = new Map(workspace.nodes.map(node => [node.id, node.run]));
  const active = new Set(workspace.nodes.filter(node => node.state === "running").map(node => node.id));
  const streamSources = new Set(workspace.nodes.filter(node => stoppedStream(node)?.source === node.id).map(node => node.id));
  const running = new Map<string, { run: string; at: number }>();
  const spans = new Map<string, Span>();
  for (const entry of workspace.history) {
    if (entry.event !== "log") continue;
    const { node, run, at, state } = entry.record;
    const time = Date.parse(at);
    if (!Number.isFinite(time)) continue;
    const current = currentRuns.get(node);
    if (current === undefined || current !== run) continue;
    if (state === "RUNNING" && !running.has(node)) {
      running.set(node, { run, at: time });
      spans.delete(node);
    } else if (TERMINAL.has(state) && !(state === "READY" && (streamSources.has(node) || active.has(node)))) {
      const started = running.get(node);
      if (started && started.run === run && !spans.has(node)) spans.set(node, { start: started.at, end: time });
    }
  }
  return spans;
}

/**
 * The last field of the verdict.
 *
 * Retention appears here rather than in a badge. When the cell is
 * something — pinned, stale, live — that is what belongs in the sentence instead, because it is the
 * more particular fact and the reader is looking at the same place for both.
 */
function retentionOf(state: CellState, cell: ClientCell, nodes: readonly WorkspaceNode[]): Segment {
  // A command that never ran keeps nothing, and "not kept" would read as a result that was let go.
  if (state === "failed" && nodes.length === 0) return { text: "nothing ran", role: "mono-dim" };
  if (state === "live") return { text: "● live", role: "mono-meta" };
  if (state === "pinned" || cell.pinned) return { text: "pinned", role: "mono-ref" };
  // Among several nodes the facts already count the stale ones; retention then says what is kept.
  if (state === "stale" && nodes.length === 1 && nodes[0]!.state === "stale") return { text: "1 stale", role: "mono-warn" };
  const results = nodes.filter(node => node.kept || node.state === "ready" || node.handle !== undefined || node.evidence !== undefined);
  const kept = results.filter((node) => node.kept).length;
  return { text: kept && kept < results.length ? `${kept} of ${results.length} kept` : kept ? "kept" : "not kept", role: "mono-dim" };
}

const VERDICT_STATE: Record<CellState, Segment> = {
  live: { text: "running", role: "mono-meta-strong" },
  failed: { text: "failed", role: "mono-bad-strong" },
  stale: { text: "stale", role: "mono-warn" },
  pinned: { text: "ok", role: "mono-ok" },
  focus: { text: "ok", role: "mono-ok" },
  default: { text: "ok", role: "mono-ok" },
};

/**
 * state · shape · facts · time · retention, always in that order.
 *
 * Missing fields are omitted. Execution state, duration and retention use stable slots;
 * type and value facts move to the result header when marked as data.
 */
export function verdictOf(
  state: CellState,
  cell: ClientCell,
  nodes: readonly WorkspaceNode[],
  now: Date,
  refusal?: string,
  value?: FormValue,
  durations: ReadonlyMap<string, Span> = new Map(),
  rejected = false,
): VerdictField[] {
  if (rejected) return [
    { segments: [{ text: "not run", role: "mono-warn" }], keep: true, slot: "state" },
    { segments: [{ text: refusal ?? "The attempt was rejected.", role: "mono-ink" }], keep: true },
    ...(nodes.length ? [{ segments: [{ text: "previous results shown", role: "mono-dim" as const }], keep: true }] : []),
  ];
  const cancelled = nodes.length === 1 && nodes[0]!.state === "cancelled";
  const lone=nodes.length===1 ? nodes[0] : undefined;
  const semantic:Segment = nodes.some(node=>node.doubt) || cell.state==="unanswered" ? {text:"outcome unknown",role:"mono-warn"}
    : cancelled ? {text:stoppedStream(lone) ? "stopped" : "cancelled",role:"mono-warn"}
    : lone?.state==="pending" ? {text:"waiting",role:"mono-dim"}
    : lone?.state==="skipped" ? {text:"skipped",role:"mono-faint"}
    : cell.submissionRefusal !== undefined && !nodes.length ? {text:"refused",role:"mono-bad-strong"}
    : refusal && !nodes.length ? {text:"not run",role:"mono-warn"}
    // A usable prefix of a run the engine still calls open has not finished: say what it is doing.
    : lone?.state==="ready" && lifetimeActive(lone) && VERDICT_STATE[state].text==="ok" ? { text: openLifetimeWord(lone), role: "mono-meta" } : VERDICT_STATE[state];
  const fields: VerdictField[] = [{ segments: [semantic], keep: true, slot: "state" }];
  const shape = shapeOf(state, nodes, refusal);
  const dataHeader = nodes.length === 1 && !refusal && ["default", "focus", "pinned"].includes(state);
  // A known type belongs to the result disclosure during execution too. Waiting, stale,
  // failure and newer-input messages still describe the run and must remain in its band.
  const typeInHeader = !refusal && lone !== undefined && shape === lone.type;
  if (shape !== "") fields.push({ segments: [{ text: shape, role: cancelled && stoppedStream(nodes[0]) ? "mono-warn" : "mono-ink" }], keep: true, ...(typeInHeader ? { zone: "data" as const } : {}) });
  for (const fact of factsOf(nodes, value)) fields.push({ segments: fact, keep: false, ...(dataHeader ? { zone: "data" as const } : {}) });
  const time = timeOf(state, nodes, durations, now);
  if (time !== "") fields.push({ segments: [{ text: time, role: "mono-dim" }], keep: false, slot: "duration" });
  // A lost reply proves nothing about what ran, so a node-free unanswered cell claims no retention.
  const unknown = cell.state === "unanswered" && !nodes.length;
  if (!unknown && (nodes.length || refusal || cell.pinned || cell.state !== "answered")) fields.push({ segments: [retentionOf(state, cell, nodes)], keep: false, slot: "retention" });
  const pin = nodes.length === 1 ? pinBindingOf(nodes[0]!) : undefined;
  if (pin) fields.push({ segments: [pin], keep: pin.role === "mono-warn" });
  return fields;
}

const PIN_COMMAND = /^:view\s+pin\b/;

/**
 * A Pin's view binding, beside (never instead of) whether its result was kept. Before publication
 * only the command says a Pin is waiting; no binding is claimed until the engine reports one.
 */
export function pinBindingOf(node: WorkspaceNode): Segment | undefined {
  const binding = node.publication?.pinBinding;
  if (binding?.state === "bound") return { text: "view input pinned", role: "mono-ok" };
  if (binding?.state === "refused") return { text: `view not pinned: ${binding.problem}`, role: "mono-warn" };
  if (binding?.state === "pending") return { text: "pinning view…", role: "mono-meta" };
  // Only work in flight: a Pin that has not started keeps nothing, and its waiting reason says why.
  if (PIN_COMMAND.test(node.command) && (node.state === "running" || node.state === "ready" && node.publication?.state === "pending")) {
    return { text: "keeping input for Pin…", role: "mono-meta" };
  }
  return undefined;
}

/** The verdict's fields with ` · ` between them. */
export function joined(fields: readonly VerdictField[]): Segment[] {
  return fields.flatMap((field, at) => (at === 0 ? [...field.segments] : [{ text: " · ", role: "mono-faint" as const }, ...field.segments]));
}

/**
 * The facts a cell carries, and only the ones that could have differed for it.
 *
 * An environment or a target is marked when it is not the session's own; a revision only when the
 * node saw a different one; traced when the command was traced; effects when a repeat was confirmed
 * although it performs an external action again — that one is always shown, per attempt.
 */
export function marksOf(cell: ClientCell, nodes: readonly WorkspaceNode[], context: SessionContext): Mark[] {
  const marks: Mark[] = [];
  const environment = nodes.map((node) => node.environment).find((it) => it !== undefined);
  if (environment && environment.environment !== context.defaultEnvironment) {
    marks.push({ kind: "environment", text: environment.environment, title: environment.revision });
  }
  if (environment && environment.target && environment.target !== context.target) {
    marks.push({ kind: "target", text: environment.target });
  }
  if (nodes.some((node) => node.traced)) marks.push({ kind: "traced", text: "traced" });
  if (cell.acknowledgeEffects) marks.push({ kind: "effects", text: "effects acknowledged" });
  return marks;
}

/**
 * Why this repeat must be asked about first, or nothing when it need not be.
 *
 * The engine reports a node it cannot repeat freely; the graph says what hangs off this one. Both
 * are the engine's facts, so the question is asked wherever the surface is drawn.
 */
export function guardOf(
  workspace: Workspace,
  cell: ClientCell,
  nodes: readonly WorkspaceNode[],
  context: SessionContext,
): RepeatGuard | undefined {
  const effectful = nodes.some((node) => node.repeatable !== true);
  // A completed construction, or an analysis that captured its inputs, is not invalidated by a
  // repeat; listing it would imply re-creation or recomputation.
  const dependents = workspace.nodes
    .filter((node) => !constructed(node) && !inputsCaptured(node) && node.dependsOn.some((on) => cell.nodes.includes(on)))
    .map((node) => (node.name ? `$${node.name}` : node.id));
  if (!effectful && dependents.length === 0) return undefined;
  const environment = nodes.map((node) => node.environment).find((it) => it !== undefined);
  return {
    what: cell.text,
    against: effectful ? environment?.environment ?? context.environment : undefined,
    dependents,
    // The same evidence as the `outcome unknown` verdict, so every repeat question says it.
    ...(nodes.some(node => node.doubt) || cell.state === "unanswered" ? { unknownOutcome: true } : {}),
  };
}

/**
 * The value the cell's form is chosen from, when the cell made one.
 *
 * The engine keeps results; the client holds handles. `held` is what has been fetched back so far,
 * so a cell whose result has not arrived yet still picks a form from what the node says it is.
 */
export function valueOf(
  nodes: readonly WorkspaceNode[],
  held: ReadonlyMap<string, import("../protocol").StoredValue> = new Map(),
): FormValue | undefined {
  const node = nodes[nodes.length - 1];
  if (!node) return undefined;
  const stored = node.handle ? held.get(node.handle) : undefined;
  return {
    type: stored?.type ?? { kind: "unknown" },
    data: stored?.data,
    state: node.state === "running" ? "running" : node.state === "failed" ? "failed" : node.state === "stale" ? "stale" : "ready",
    /*
     * A conversation is asking when the engine says one is active; what it asked is the last line
     * it wrote, which is where a prompt like `Password:` is. The rest of the transcript is the
     * stream, not the question — showing all of it as the question was the whole of it.
     */
    asking:
      node.interactive && node.conversationActive && node.state === "running" ? lastLine(node.wrote) : undefined,
    wrote: node.wrote,
    http: node.provenance["http.status"] !== undefined,
  };
}

/** The last line with anything on it, which is where a command's question is. */
function lastLine(wrote: string | undefined): string | undefined {
  const lines = (wrote ?? "").split("\n").filter((line) => line.trim() !== "");
  return lines[lines.length - 1];
}

/** The identity glyph of one node. */
export function glyphOf(node: WorkspaceNode, value?: FormValue): Glyph {
  if (node.state === "cancelled") return stoppedStream(node) ? "stopped" : "cancelled";
  if (node.state === "ready" && value?.type?.kind === "iter") return "recipe";
  if (node.state === "pending") return "pending";
  return node.state;
}

export function identityOf(node: WorkspaceNode, value?: FormValue): Identity {
  return { id: node.id, label: node.name ? `$${node.name}` : node.id, glyph: glyphOf(node, value) };
}

export interface Failure {
  readonly issues?: import("../protocol").ErrorRecord["issues"];
  readonly code?: string;
  readonly message: string;
  readonly span?: string;
}

/**
 * The one line that stands for a failure wherever it is summarised: its code when it has one, its
 * message otherwise. Whoever draws the failure in full says the rest and never repeats this.
 */
export function headlineOf(failure: Failure): string {
  return failure.code ?? failure.message;
}

/** Structured locations are authoritative; arbitrary provider messages are never parsed as spans. */
export function failureOf(reason: string, record?: import("../protocol").ErrorRecord, cell?: string): Failure {
  const matched = /^([A-Z][A-Z0-9_]*):\s*([\s\S]*)$/.exec(reason.trim());
  const code = record?.code ?? matched?.[1];
  const message = record?.message ?? (matched?.[2] ?? reason).trim();
  const span = record?.locations?.length ? locationLines(record.locations, cell).join("\n") : undefined;
  return { ...(code ? { code } : {}), message, ...(span ? { span } : {}), ...(record?.issues.length ? { issues: record.issues } : {}) };
}

/** Command text with its gaps removed, for matching a node's stage to the line that made it. */
function squeeze(text: string): string {
  return text.replace(/^\s*\|\s*/, "").replace(/\s+/g, " ").trim();
}

/**
 * The command band: the text split into lines, coloured as one command so a multi-line `:calc`
 * keeps its colours, and each node placed on the line that makes it.
 */
export function rowsOf(text: string, nodes: readonly WorkspaceNode[], values: ReadonlyMap<string, FormValue>, language?: Language): { rows: SourceRow[]; rowOf: Map<string, number> } {
  const segments = commandSegments(text, language);
  const lines: Segment[][] = [[]];
  for (const segment of segments) {
    const parts = segment.text.split("\n");
    parts.forEach((part, at) => {
      if (at > 0) lines.push([]);
      if (part !== "") lines[lines.length - 1]!.push(segment.role ? { text: part, role: segment.role } : { text: part });
    });
  }
  const raw = text.split("\n").map(squeeze);
  const rowOf = new Map<string, number>();
  let at = 0;
  for (const node of nodes) {
    const stage = squeeze(node.command);
    const found = stage === "" ? -1 : raw.findIndex((line, index) => index >= at && (line.includes(stage) || (line !== "" && stage.includes(line))));
    if (found >= 0) at = found;
    rowOf.set(node.id, Math.min(at, lines.length - 1));
  }
  const rows = lines.map((line, index) => ({
    segments: line,
    nodes: nodes.filter((node) => rowOf.get(node.id) === index).map((node) => identityOf(node, values.get(node.id))),
  }));
  return { rows, rowOf };
}

function clock(at: string | undefined): string | undefined {
  if (at === undefined) return undefined;
  const date = new Date(at);
  if (Number.isNaN(date.getTime())) return undefined;
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

export function readCell(input: SessionInput, cell: ClientCell, now: Date, durations: ReadonlyMap<string, Span> = durationsOf(input.workspace)): SessionCell {
  const nodes = nodesOf(input.workspace, cell);
  const refusal = refusalOf(input.workspace, cell);
  // A refused repeat or revision keeps showing the previous attempt's results under its refusal.
  const rejected = input.workspace.attemptFailures?.[cell.lastRun] !== undefined
    || (cell.submissionRefusal !== undefined && nodes.length > 0);
  const state = stateOf(cell, rejected ? [] : nodes, input.focused === cell.id, refusal);
  const document = cell.document ? documentOperation(cell.text) : undefined;
  const documentNotice = document?.context === "env" && cell.state === "answered"
    ? [...cell.diagnostics.filter(d => d.severity !== "error").map(d => d.message),
      ...(refusal ? [] : [cell.restored
        ? "Historical plan preview. Reopen the document and plan again before applying."
        : `In the original window session:\nApply with :env apply $${document.planName}\nDiscard with :env discard $${document.planName}\nIf this window was reopened, edit and plan again. Nothing applied by planning.`])].join("\n")
    : undefined;
  const values = new Map(nodes.map((node) => [node.id, valueOf([node], input.held)!]));
  const definitions = nodes.filter(node => node.currentDefinition !== undefined);
  const shown = cell.document ? documentLabel(cell.text) : nodes.length === 1 && definitions.length ? `${definitions[0]!.currentDefinition}${definitions[0]!.name ? ` > ${definitions[0]!.name}` : ""}` : cell.text;
  const definitionNotices = definitions.map(node => ({ severity: "info" as const, message: nodes.length === 1
    ? "Current definition shown; the submitted source is preserved in source details."
    : `Submitted source is superseded for ${node.name ? `$${node.name}` : node.id}. Current definition: ${node.currentDefinition}` }));
  const { rows, rowOf } = rowsOf(shown, nodes, values, input.language);
  const value = valueOf(nodes, input.held);
  return {
    id: cell.id,
    streamOutput: nodes.some(node => node.streamOutput),
    // Only the engine's own statement names a source; dependencies are never used to guess one.
    streamSource: nodes.some(node => node.streamSource === true),
    attempt: cell.lastRun,
    state,
    pinned: cell.pinned,
    rows,
    ...(clock(nodes[0]?.startedAt) ? { time: clock(nodes[0]?.startedAt)!, timestamp: nodes[0]?.startedAt } : {}),
    chars: shown.length,
    source: cell.document?.source ?? cell.text,
    documentNotice,
    previousAttemptResults: rejected && nodes.length > 0,
    notices: [...definitionNotices, ...(cell.receipts ?? []).map(receipt => ({ severity: "info" as const, message: receipt.summary })), ...cell.diagnostics.filter(d => d.severity === "info" || d.severity === "warning")
      .map(d => ({ severity: d.severity as "info" | "warning", message: d.message }))],
    nodes: nodes.map((node) => {
      const span = durations.get(node.id);
      return {
        ...identityOf(node, values.get(node.id)),
        row: rowOf.get(node.id) ?? 0,
        ...(node.type && node.type!=="Unknown" ? {type:node.type}:{}),
        ...(span ? { durationMs: span.end - span.start } : {}),
        ...(node.state === "failed" && node.failure ? { failure: failureOf(node.failure, node.failureRecord, cell.id) } : {}),
      };
    }),
    verdict: verdictOf(state, cell, nodes, now, refusal, value, durations, rejected),
    marks: marksOf(cell, nodes, input.context),
    ...(value ? { value } : {}),
    pipeline: nodes.length > 1,
    runActive: runActiveOf(cell, nodes),
    guard: guardOf(input.workspace, cell, nodes, input.context),
    view: cell.view,
    results: cell.results,
  };
}

/**
 * Whether this cell's work is still active, from its actual current nodes: a submission not yet
 * answered, a node running or waiting to run, or a run the engine still calls open (a recording or
 * a followed scan whose ready value is an acknowledged prefix). Active work can be cancelled through
 * the ordinary node cancellation and is not repeated or branched until it has finished. Display
 * words, receipts and type names play no part.
 */
export function runActiveOf(cell: ClientCell, nodes: readonly WorkspaceNode[]): boolean {
  return cell.state === "running"
    || nodes.some(node => node.state === "running" || node.state === "pending" || lifetimeActive(node));
}

/** `wes / sales-api · connected` */
export function topLine(context: SessionContext): Segment[] {
  const dot: Segment = { text: "  ·  ", role: "mono-faint" };
  const connection: Segment =
    context.connection === "connected"
      ? { text: "connected", role: "mono-ok" }
      : { text: context.connection, role: "mono-warn" };
  const line: Segment[] = [
    { text: "wes", role: "mono-ink-strong" },
    { text: "  /  ", role: "mono-faint" },
    { text: context.workspace, role: "mono-ink" },
  ];
  line.push(dot, connection);
  return line;
}

/** `env: DEV · rev 14 · grant 12 min · → acme-eu` — only what a command would run against. */
export function contextLine(context: SessionContext): Segment[] {
  const dot: Segment = { text: "  ·  ", role: "mono-faint" };
  const line: Segment[] = [];
  if (context.environment) line.push({ text: "env: ", role: "mono-dim" }, { text: context.environment, role: "mono-meta" });
  if (context.revision !== undefined) {
    if (line.length > 0) line.push(dot);
    line.push({ text: `rev ${context.revision}`, role: "mono-faint" });
  }
  if (context.grantMinutes !== undefined) {
    if (line.length > 0) line.push(dot);
    line.push({ text: `grant ${context.grantMinutes} min`, role: "mono-warn" });
  }
  if (context.target) {
    if (line.length > 0) line.push(dot);
    line.push({ text: `→ ${context.target}`, role: "mono-dim" });
  }
  return line;
}

/**
 * `9 nodes · 3 kept · 2 stale · ⇣ following` — and no field for a count of nothing.
 *
 * Following is the exception to "no field for nothing": it is a setting rather than a count, and a
 * session that is *not* following has to say so. Otherwise `/follow off` is a silent mode — the
 * next ⏎ simply fails to take the reader anywhere, and nothing on the screen says why.
 */
export function countsLine(workspace: Workspace, following = true): Segment[] {
  const dot: Segment = { text: "  ·  ", role: "mono-faint" };
  const kept = workspace.nodes.filter((node) => node.kept).length;
  const stale = workspace.nodes.filter((node) => node.state === "stale").length;
  const line: Segment[] = [{ text: `${workspace.nodes.length} nodes`, role: "mono-ink" }];
  if (kept > 0) line.push(dot, { text: `${kept} kept`, role: "mono-dim" });
  if (stale > 0) line.push(dot, { text: `${stale} stale`, role: "mono-warn" });
  line.push(dot, { text: following ? "⇣ following" : "⇣ not following", role: "mono-dim" });
  return line;
}

/**
 * `~ 2 results stale   /stale`
 *
 * Staleness is two things at once: a property each node carries, and something that just happened,
 * which nothing else announces. This is the announcement, and it is only there when there is one.
 */
export function noteLine(workspace: Workspace): Segment[] | undefined {
  // Stream invalidation is a normal observation transition, already labelled on the
  // cell/graph. Announcing every sample here resizes the entire workspace each time.
  const nodes = workspace.nodes.filter(node => node.state === "stale"
    && !observationStale(node) && workspace.wentStale.includes(node.id));
  const stale = nodes.length;
  if (stale === 0) return undefined;
  const line: Segment[] = [
    { text: `~ ${stale} result${stale === 1 ? "" : "s"} stale`, role: "mono-warn" },
  ];
  line.push({ text: "   ", role: "mono-faint" }, { text: "/stale", role: "mono-dim" });
  return line;
}

/** The keys the session offers, as the surface's footer lists them. */
export function keysLine(): Segment[] {
  const dot: Segment = { text: "  ·  ", role: "mono-faint" };
  return [
    { text: "↑↓", role: "mono-ref" }, { text: " history", role: "mono-dim" }, dot,
    { text: "⇥", role: "mono-ref" }, { text: " complete", role: "mono-dim" }, dot,
    // Shift+Enter inserts a newline, matching the prompt key handler.
    { text: "⇧⏎", role: "mono-ref" }, { text: " newline", role: "mono-dim" }, dot,
    { text: "/graph /env /settings", role: "mono-meta" }, dot,
    { text: "?", role: "mono-ref" }, { text: " keys", role: "mono-dim" },
  ];
}

export function readSession(input: SessionInput, now = new Date()): SessionModel {
  const durations = durationsOf(input.workspace);
  return {
    top: topLine(input.context),
    cells: input.cells.map((cell) => readCell(input, cell, now, durations)),
    note: noteLine(input.workspace),
    context: contextLine(input.context),
    counts: countsLine(input.workspace, input.following ?? true),
    keys: keysLine(),
  };
}
