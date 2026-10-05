/**
 * A cell is one command and what came of it, and it is the unit this client is built around.
 *
 * The engine's model is a graph — a command makes a node, nodes depend on nodes, staleness travels
 * along the edges. That is true and it is not what a person is doing. A person types one thing, reads
 * what came back, and types the next; the reading order is the order they were typed, and a canvas laid
 * out by dependency loses that reading order.
 *
 * So: cells are the surface, and the graph is a view you can open when the question is "what depends on
 * what" rather than "what did I just do".
 *
 * Two identifiers, and the distinction is load-bearing:
 *
 * - `id` names the cell. It never changes, and everything the person arranged — pinned, resized,
 *   pulled into a tab — hangs off it.
 * - `lastRun` names one **attempt** at running it, and changes every time Run is pressed. It is what
 *   the engine deduplicates on: re-running a cell is the normal gesture, so a key that named the cell
 *   would make the second run look like a duplicate and swallow it.
 */

import type { Diagnostic } from "./protocol";
import { EngineRefusal } from "./engine-diagnostics";

export type CellState =
  /** Sent, and the engine said what it made. */
  | "running"
  /** Sent, and the engine never answered. Nobody knows whether it arrived. */
  | "unanswered"
  /** The engine answered. */
  | "answered";

export interface Cell {
  readonly id: string;
  readonly text: string;
  readonly receipts?: readonly { readonly summary: string }[];
  readonly document?: { readonly source: string };
  readonly lastRun: string;
  readonly originAttempt?: string;
  readonly workRoot?: string;
  readonly revisionOf?: string;
  readonly revisionText?: string;
  readonly attempts?: readonly string[];
  readonly acknowledgeEffects?: boolean;
  readonly repeatFrom?: string;
  readonly state: CellState;
  /**
   * The engine's reason for refusing the current attempt before it started. Set only when the engine
   * said so authoritatively; a lost or refused reply without that statement stays `unanswered`.
   */
  readonly submissionRefusal?: string;
  readonly restored?: boolean;

  /** What the engine said this command made. Empty is a real answer: `:import` makes no nodes. */
  readonly nodes: readonly string[];
  readonly diagnostics: readonly Diagnostic[];

  /**
   * How the person arranged it. Client-side and kept in local storage, because the engine can neither
   * use it nor check it, and a second client would want its own.
   */
  readonly pinned: boolean;
  /**
   * Default tier for results without an explicit arrangement. Command folding is independent.
   */
  readonly view: CellView;
  /** Each result owns its tier and explicitly requested height (text rows). */
  readonly results?: Readonly<Record<string, ResultArrangement>>;
  /** Pixels, when the person has dragged it. Unset means "as tall as it needs".*/
  readonly height?: number;
}

/** The three result tiers, in the order the size control cycles. */
export type CellView = "preview" | "expanded" | "collapsed";

export interface ResultArrangement {
  readonly view: CellView;
  readonly rows?: number;
}

export function resultArrangement(cell: { view: CellView; results?: Readonly<Record<string, ResultArrangement>> }, node?: string): ResultArrangement {
  return node && cell.results?.[node] || { view: cell.view };
}

/** Geometry never changes a command or replays an effect. Missing targets do nothing. */
export function arrangeResult(cell: Cell, node: string | undefined, patch: { view?: CellView; rows?: number | null }): Cell {
  const target = node ?? cell.nodes.at(-1);
  if (!target || !cell.nodes.includes(target)) return cell;
  const before = resultArrangement(cell, target);
  const rows = patch.rows === null ? undefined : patch.rows === undefined ? before.rows : Math.min(40, Math.max(6, Math.round(patch.rows)));
  if (rows !== undefined && !Number.isFinite(rows)) return cell;
  return { ...cell, results: { ...cell.results, [target]: { view: arrangedView({ view: patch.view ?? before.view }), rows } } };
}

export const NEXT_VIEW: Readonly<Record<CellView, CellView>> = { preview: "expanded", expanded: "collapsed", collapsed: "preview" };

/** Current three-state arrangement view; malformed inputs use preview. */
export function arrangedView(arrangement: { readonly view?: unknown } | undefined): CellView {
  const view = arrangement?.view;
  if (view === "preview" || view === "expanded" || view === "collapsed") return view;
  return "preview";
}

/** Browsers without `crypto.randomUUID` are old enough that this is only about not throwing. */
function identifier(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

export function newCell(text: string): Cell {
  const id = identifier();
  return {
    id,
    text,
    lastRun: id,
    state: "running",
    nodes: [],
    diagnostics: [],
    pinned: false,
    view: "preview",
  };
}

/**
 * The same cell, run again — a new attempt, so the engine treats it as a new question.
 *
 * Everything the person arranged is kept. Re-running a chart should not throw away the size they
 * dragged it to.
 */
export function running(cell: Cell): Cell {
  return { ...cell, lastRun: identifier(), state: "running", submissionRefusal: undefined, nodes: [], diagnostics: [], restored: false };
}

export function repeating(cell: Cell, acknowledgeEffects: boolean, from?: string): Cell {
  const next = running(cell);
  return { ...next, revisionOf: undefined, revisionText: undefined, nodes: cell.nodes, originAttempt: cell.originAttempt ?? cell.lastRun,
    attempts: [...(cell.attempts ?? [cell.lastRun]), next.lastRun], acknowledgeEffects, repeatFrom: from };
}

/** A revision preserves the displayed successful definition until admission answers. */
export function revising(cell: Cell, text: string): Cell {
  const next = running(cell);
  return { ...next, text: cell.text, nodes: cell.nodes, workRoot: cell.workRoot ?? cell.originAttempt ?? cell.lastRun,
    originAttempt: cell.originAttempt ?? cell.lastRun, revisionOf: cell.originAttempt ?? cell.lastRun,
    revisionText: text, repeatFrom: undefined,
    attempts: [...(cell.attempts ?? [cell.lastRun]), next.lastRun] };
}

/** A full reconnect snapshot repairs retirement events missed while disconnected. */
export function reconcileCells(cells: readonly Cell[], admitted?: readonly string[]): readonly Cell[] {
  if (admitted === undefined) return cells;
  const known = new Set(admitted);
  return cells.filter(cell => cell.state !== "answered" || refusedBeforeAdmission(cell) || [cell.workRoot, cell.originAttempt, cell.lastRun, ...(cell.attempts ?? [])]
    .some(attempt => attempt !== undefined && known.has(attempt)));
}

/** Retirement removes the whole admitted work group; unrelated local drafts survive. */
export function retireCells(cells: readonly Cell[], retired: readonly string[]): readonly Cell[] {
  const gone = new Set(retired);
  return cells.filter(cell => ![cell.workRoot, cell.originAttempt, cell.lastRun, ...(cell.attempts ?? [])]
    .some(attempt => attempt !== undefined && gone.has(attempt)));
}

/** Reconnect replays attempts in admission order. Older frames cannot replace a newer definition. */
export function planned(cells: readonly Cell[], event: Extract<import("./protocol").Event, { event: "planned" }>): readonly Cell[] {
  const root = event.workOf ?? event.repeatOf ?? event.revisionOf ?? event.cell;
  const found = cells.find(cell => cell.lastRun === event.cell || (cell.workRoot ?? cell.originAttempt ?? cell.lastRun) === root);
  const accepted = event.revisionOf ? event.revisionAccepted === true : !event.failure;
  const definition = event.repeatOf ?? (accepted ? event.cell : undefined);
  if (!found) return [...cells, { ...newCell(event.text), id: root, workRoot: root, lastRun: event.cell,
    originAttempt: definition, revisionOf: event.revisionOf ?? undefined,
    revisionText: event.revisionOf ? event.text : undefined, acknowledgeEffects: event.acknowledgeEffects,
    attempts: [event.cell], nodes: event.nodes, state: "answered", receipts: event.receipts ?? undefined, restored: event.restored, diagnostics: event.diagnostics ?? [], document: event.document ?? undefined }];
  if (found.lastRun !== event.cell && (found.attempts?.includes(event.cell) || (!event.repeatOf && !event.revisionOf))) return cells;
  return cells.map(cell => cell !== found ? cell : { ...cell, workRoot: root, lastRun: event.cell,
    text: accepted ? event.text : cell.text,
    document: event.document ?? cell.document,
    receipts: event.receipts ?? undefined,
    diagnostics: event.diagnostics ?? cell.diagnostics,
    originAttempt: definition ?? cell.originAttempt,
    revisionOf: event.revisionOf ?? undefined,
    revisionText: event.revisionOf ? event.text : undefined,
    submissionRefusal: undefined,
    acknowledgeEffects: event.acknowledgeEffects ?? cell.acknowledgeEffects,
    repeatFrom: event.repeatFrom ?? undefined,
    attempts: cell.attempts?.includes(event.cell) ? cell.attempts : [...(cell.attempts ?? [cell.lastRun]), event.cell].filter((id, index, all) => all.indexOf(id) === index),
    nodes: accepted ? event.nodes : cell.nodes, state: "answered", restored: event.restored });
}

/**
 * The same attempt again, because this is asking what happened to it rather than asking for another.
 *
 * Keeping the identifier is what tells the engine this is the same question, so it replies with what
 * the first attempt produced instead of running it twice.
 */
export function askingAgain(cell: Cell): Cell {
  return { ...cell, state: "running", submissionRefusal: undefined };
}

/** A reply on SSE can beat an HTTP failure; an older request can also outlive a new run. */
export function unanswered(cell: Cell, attempt: string): Cell {
  return cell.lastRun === attempt && cell.state === "running"
    ? { ...cell, state: "unanswered" } : cell;
}

const UNSTATED_REFUSAL = "The engine refused this submission before it started and gave no reason.";

/**
 * What a failed submission request means for its cell, for every submission path.
 *
 * Only the engine's explicit `not-started` outcome turns the attempt into a durable refusal; the
 * nodes and arrangement already shown are kept, so a refused repeat or revision still shows the
 * previous results. Any other failure — a lost reply, a timeout, a refusal without that statement —
 * leaves the outcome unknown. A later attempt or an SSE answer that arrived first always wins.
 */
export function submissionFailed(cell: Cell, attempt: string, error: unknown): Cell {
  if (cell.lastRun !== attempt || cell.state !== "running") return cell;
  if (!(error instanceof EngineRefusal) || error.submissionOutcome !== "not-started") return unanswered(cell, attempt);
  return { ...cell, state: "answered", submissionRefusal: error.detail.trim() || UNSTATED_REFUSAL };
}

/** A refused first submission: no definition was ever admitted, so retrying means submitting it anew. */
export function refusedBeforeAdmission(cell: Cell): boolean {
  return cell.state === "answered" && cell.submissionRefusal !== undefined
    && cell.originAttempt === undefined && cell.workRoot === undefined;
}

/** What the person arranged, which survives a reload; everything else comes from the engine. */
interface Arrangement {
  readonly pinned: boolean;
  readonly view?: CellView;
  readonly height?: number;
}

const KEY = "wes.cells";

export function loadArrangements(): Record<string, Arrangement> {
  try {
    const stored = window.localStorage.getItem(KEY);
    return stored === null ? {} : (JSON.parse(stored) as Record<string, Arrangement>);
  } catch {
    return {};
  }
}

export function saveArrangements(cells: readonly Cell[]): void {
  try {
    const arranged: Record<string, Arrangement> = {};
    for (const cell of cells) {
      if (cell.pinned || cell.view !== "preview" || cell.height !== undefined) {
        arranged[cell.id] = { pinned: cell.pinned, view: cell.view, height: cell.height };
      }
    }
    window.localStorage.setItem(KEY, JSON.stringify(arranged));
  } catch {
    // nothing worth interrupting anyone over
  }
}

/**
 * What a cell made, in the words you would use to refer to it.
 *
 * <p>A node's name is how a later command reaches its result, and that is worth showing — but only when
 * showing it says something. {@code :read $bars > mum} already ends in the name, so a
 * chip reading {@code $mum} two words later is the same fact twice on one line.
 *
 * <p>It is kept for everything else, because the other cases are the ones where there is nowhere else to
 * read it: a command written without {@code > name}, whose result is still reachable by node id; a cell
 * rebuilt from a session or sent by another client, whose text you never typed; and a command that makes
 * more than one node.
 */
export function referencesOf(
  text: string,
  nodes: readonly string[],
  nameOf: (node: string) => string,
): readonly string[] {
  return nodes
    .map((node) => {
      const name = nameOf(node);
      if (name === "") {
        return node;
      }
      return binds(text, name) ? undefined : name;
    })
    .filter((reference): reference is string => reference !== undefined)
    .map((reference) => `$${reference}`);
}

/** Whether the command itself already ends by binding this name, as in {@code … > mum}. */
function binds(text: string, name: string): boolean {
  return new RegExp(`(^|\\s)>\\s*${escaped(name)}(\\s|$)`).test(text);
}

function escaped(name: string): string {
  return name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
