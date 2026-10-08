import { compareNumeric, numericText, isNumeric, isExactNumber, drawingNumber, drawingTextNumber } from "../exact-json";
import type { Rule } from "./registry";
import { declarationPath, declaredTone, fieldMeta, scalarSpelling, throughOptions, ELEMENT, field as fieldSegment } from "../value-meta";
import { ViewInputError } from "../value-views/contract";
import { valueViewModules } from "../value-views/registry";
/**
 * `present(prepared, facts, context, registry) → Presentation`: tier 2 of the output pipeline.
 *
 * Pure: the same inputs give the same tree, with no DOM, no React and no engine call. Width and the
 * line budget are inputs, so the tree holds exactly what fits and every node says in `more` what did
 * not. Only the rows and fields inside the budget are visited; a list of a million rows costs the
 * rows that are drawn. The window pages lists by `pageSize()`, never `Infinity`.
 *
 * Selection order, most particular first, first match wins:
 *   1. the client's own record types (`wes.Help`);
 *   2. a matching type presentation entry, or automatic value-view modules when no entry
 *      selects another kind (including direct calc drawings);
 *   3. structural rules — list of records → table, list of numbers → items, record
 *      → fields, long or multi-line text → text, Bytes → text or size; Option unwrapped first;
 *   4. `line` for whatever is left.
 */
import type { TypeShape } from "../protocol";
import { describeType } from "../protocol";
import { clusters, ellipsizeEnd, pad, width } from "./columns";
import { compactDecimal, formatWire, formatWith, grouped, isIdentifier, prettyType, shortIdentifier } from "./format";
import { DecodedBytes, type Prepared } from "./prepare";
import { typeLine, typeShapeOf, typeStructure } from "./type-shape";
import type { Entry, Format, Registry } from "./registry";
import { tableKey } from "./table-views";
import {
  EXPANDED_DEPTH, expanded_lines, pageSize, PREVIEW_DEPTH,
  type Column, type Context, type Facts, type FieldRow, type More, type OfferName, type PresentationNode, type Run, type Summary, type TableArrangement,
} from "./types";

export interface Presentation {
  readonly root: PresentationNode;
  /** Lines the tree spends when drawn. */
  readonly lines: number;
  /** What the verdict may say about the value: `exit 1`, `HTTP 404`, `213`. */
  readonly summary: Summary;
  /** Tail notices: a rejected registry entry for this type, a decoded reading. */
  readonly notices: readonly string[];
  /** Offers of the whole value, deduplicated, in the order they were found. */
  readonly offers: readonly OfferName[];
}

export interface PresentInput {
  readonly prepared: Prepared;
  readonly facts?: Facts;
  readonly context: Context;
  readonly registry: Registry;
  readonly skipViews?: ReadonlySet<string>;
}

/** Widest a table column is drawn before its cells ellipsize. */
/** Widest a table cell is drawn in a cell's preview or expanded block; the window allows more. */
const MAX_CELL = 40;
const MAX_CELL_WINDOW = 200;
/** Rows one `show more` adds to a table in a cell; the window adds a page. */
export const CELL_STEP = 20;
/** Widest the name column of a record is padded to. */
const MAX_NAME = 20;
/** Columns between table columns and between a name and its value. */
export const GAP = 2;
/** Columns the renderer's `▸ `/`▾ ` control takes at the start of the value column. */
export const OPENER = 2;
/** Columns an open nested value's body sits in from its own name's column: one nesting level. */
export const INDENT = 2;

/**
 * What a mode means, decided once per `present()` and read by every kind: the only place the
 * three modes are told apart. A kind never asks `context.mode`; it asks the policy for the one
 * thing it needs, so preview, expanded and window differ by this table alone.
 */
interface Policy {
  /** A record's scalars share lines before its nested values are drawn, so a list still gets room. */
  readonly packScalars: boolean;
  /** Lines the packed scalars may take. */
  readonly packedLines: number;
  /** A wide single-line scalar becomes a wrapped block; otherwise a cut line with an opener. */
  readonly blockWideText: boolean;
  /** Multi-line text wraps at the columns instead of ending each line in `…`. */
  readonly wrapText: boolean;
  /** Long hex identifiers are shortened. */
  readonly shortIdentifiers: boolean;
  /** Scalars of a list share one line instead of one line each. */
  readonly packItems: boolean;
  /** Rows a list may draw: the page asked for, or what the budget leaves. */
  readonly rows: (budgetLeft: number, page: number) => number;
  /** HTTP headers drawn under the status line. */
  readonly headers: "none" | "budget";
  /** Nesting depth opened without being asked. */
  readonly depth: number;
  /** Lines a value opened by hand spends on itself, or 0 when it shares the block's budget. */
  readonly ownLines: number;
  /** Columns a table cell may take before it is cut; the table itself is never cut, it scrolls. */
  readonly cell: number;
  /** Rows one `show more` under a table adds: a screenful in a cell, a page in the window. */
  readonly step: number;
}

const POLICIES: Record<Context["mode"], Policy> = {
  preview: { packScalars: true, packedLines: 2, blockWideText: false, wrapText: false, shortIdentifiers: true, packItems: true, rows: (left) => left - 1, headers: "none", depth: PREVIEW_DEPTH, ownLines: expanded_lines(), cell: MAX_CELL, step: CELL_STEP },
  expanded: { packScalars: false, packedLines: 0, blockWideText: true, wrapText: true, shortIdentifiers: false, packItems: false, rows: (left, page) => Math.min(page, left - 1), headers: "budget", depth: EXPANDED_DEPTH, ownLines: 0, cell: MAX_CELL, step: CELL_STEP },
  window: { packScalars: false, packedLines: 0, blockWideText: true, wrapText: true, shortIdentifiers: false, packItems: false, rows: (left, page) => Math.min(page, left - 1), headers: "budget", depth: Number.POSITIVE_INFINITY, ownLines: 0, cell: MAX_CELL_WINDOW, get step(){ return pageSize(); } },
};

interface Walk {
  readonly viewModules?: Prepared["viewModules"];
  readonly skipViews: ReadonlySet<string>;
  readonly context: Context;
  readonly policy: Policy;
  readonly registry: Registry;
  readonly facts: Facts;
  readonly budget: { left: number };
  readonly offers: OfferName[];
  readonly notices: string[];
  /** The declaration path of a data pointer, when the value carries contract metadata. */
  readonly declared: (pointer: string) => string | undefined;
  readonly meta?: import("../value-meta").ValueMeta;
}

const childPath = (path: string, key: string | number) => `${path}/${String(key).replace(/~/g, "~0").replace(/\//g, "~1")}`;

const run = (text: string, tone: Run["tone"]): Run => ({ text, tone });

function isObject(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value) && !isExactNumber(value) && !(value instanceof DecodedBytes);
}

function fieldType(type: TypeShape, name: string): TypeShape {
  return type.kind === "record" ? type.fields?.find((field) => field.name === name)?.type ?? { kind: "unknown" } : { kind: "unknown" };
}

function elementOf(type: TypeShape): TypeShape {
  return type.kind === "list" || type.kind === "iter" || type.kind === "option" ? type.element : { kind: "unknown" };
}

/** Option unwrapped: `some(x)` is `x`, `none` is `none`. Undefined means "not an option value". */
function unwrap(type: TypeShape, data: unknown): { type: TypeShape; data: unknown; none?: boolean } {
  if (type.kind === "option" && isObject(data) && (data.kind === "some" || data.kind === "none")) {
    return data.kind === "none" ? { type: type.element, data: undefined, none: true } : unwrap(type.element, data.value);
  }
  // An unknown-typed option value from a loose descriptor reads the same way.
  if (type.kind === "unknown" && isObject(data) && Object.keys(data).length <= 2 && (data.kind === "none" || (data.kind === "some" && "value" in data))) {
    return data.kind === "none" ? { type, data: undefined, none: true } : unwrap(type, data.value);
  }
  return { type, data };
}

function spend(walk: Walk, lines = 1): boolean {
  if (walk.budget.left < lines) return false;
  walk.budget.left -= lines;
  return true;
}

function offer(walk: Walk, name: OfferName) {
  if (!walk.offers.includes(name)) walk.offers.push(name);
}

/** A string on one line: tabs as a faint `⇥`, a trailing newline as a faint `⏎`, the rest as `+N lines`. */
function textRuns(text: string, tone: Run["tone"], walk: Walk): Run[] {
  if (text === "") return [run("empty", "faint")];
  const newline = text.endsWith("\n");
  const lines = (newline ? text.slice(0, -1) : text).split("\n");
  const first = lines[0] ?? "";
  const runs: Run[] = [];
  const parts = first.split("\t");
  parts.forEach((part, at) => {
    if (at > 0) runs.push(run("⇥", "faint"));
    if (part !== "") runs.push(run(identifierText(part, walk), tone));
  });
  if (lines.length > 1) runs.push(run(`  +${lines.length - 1} lines`, "faint"));
  else if (newline) runs.push(run(" ⏎", "faint"));
  return runs;
}

function identifierText(text: string, walk: Walk): string {
  return walk.policy.shortIdentifiers && isIdentifier(text) ? shortIdentifier(text) : text;
}

/**
 * A scalar's runs in the tone its contract declares for that exact value, when the value carries
 * metadata for its declaration path. Structural tones (none, null, missing, decoding) never change.
 */
function scalarRuns(type: TypeShape, data: unknown, walk: Walk, format?: Format, declaration?: string): Run[] {
  const runs = plainScalarRuns(type, data, walk, format);
  if (declaration === undefined || !walk.meta) return runs;
  const tone = declaredTone(fieldMeta(walk.meta, declaration), unwrap(type, data).data);
  return tone ? runs.map(it => it.tone === "literal" ? { ...it, tone } : it) : runs;
}

/** Runs a declaration may recolour: literal values and earlier declared tones, never structural ones. */
const RETONABLE = new Set<Run["tone"]>(["literal", "ok", "warn", "bad", "dim", "meta", "ink"]);
const matches = (rule: Rule, row: unknown, element: TypeShape) => {
  const spelling = isObject(row) ? scalarSpelling(unwrap(fieldType(element, rule.field), row[rule.field]).data) : undefined;
  return spelling !== undefined && rule.values.includes(spelling);
};
/**
 * A table cell under its entry: the field's tone cases over the type's tone (`inherit` keeps it,
 * `ink` clears it), then matching cell rules in order, each later rule winning per property.
 */
function declaredCell(entry: Entry, row: unknown, name: string, element: TypeShape, runs: Run[]): { runs: Run[]; style?: "badge" } {
  let tone: Run["tone"] | undefined;
  let style = entry.styles?.[name];
  const cases = entry.tones?.[name];
  if (cases && isObject(row)) {
    const spelling = scalarSpelling(unwrap(fieldType(element, name), row[name]).data);
    if (spelling !== undefined && Object.hasOwn(cases.cases, spelling)) tone = cases.cases[spelling];
    else if (spelling !== undefined && cases.otherwise !== "inherit") tone = cases.otherwise;
  }
  for (const rule of entry.rules ?? []) {
    if (!("cell" in rule.target) || rule.target.cell !== name || !matches(rule, row, element)) continue;
    if (rule.tone) tone = rule.tone;
    if (rule.style) style = rule.style;
  }
  const toned = tone ? runs.map((it) => RETONABLE.has(it.tone) ? { ...it, tone: tone! } : it) : runs;
  return style === "badge" ? { runs: toned, style } : { runs: toned };
}
/** The background tint the last matching row rule declares. */
function rowTint(entry: Entry, row: unknown, element: TypeShape): Run["tone"] | undefined {
  let tint: Run["tone"] | undefined;
  for (const rule of entry.rules ?? []) if ("row" in rule.target && rule.tone && matches(rule, row, element)) tint = rule.tone;
  return tint;
}

/** Where a nested tree starts in the declaration, when the value carries metadata. */
function declaredAt(walk: Walk, path: string, suffix = ""): { declared?: import("./types").Declared } {
  const at = walk.meta ? walk.declared(path) : undefined;
  return walk.meta && at !== undefined ? { declared: { meta: walk.meta, at: at + suffix } } : {};
}

/** One scalar, or a stand-in for something bigger, as runs on one line. */
function plainScalarRuns(type: TypeShape, data: unknown, walk: Walk, format?: Format): Run[] {
  const open = unwrap(type, data);
  if (open.none) return [run("none", "faint")];
  const value = open.data;
  if (value === null) return [run("null", "faint")];
  if (value === undefined) return [run("missing", "faint")];
  if (value instanceof DecodedBytes) {
    if (value.pending) return [run("decoding…", "faint")];
    if (value.text !== undefined) return textRuns(value.text, "literal", walk);
    return [run(`${grouped(value.size)} B`, "literal"), run(` · ${value.problem ?? "not text"}`, "faint")];
  }
  if (format) {
    const formatted = formatWith(format, value, walk.context.locale, walk.context.timeZone);
    if (formatted !== undefined) return [run(formatted, "literal")];
  }
  if (open.type.kind === "primitive") {
    const formatted = formatWire(open.type.name, value, walk.context.locale, walk.context.timeZone);
    if (formatted !== undefined) return [run(formatted, "literal")];
  }
  if (isExactNumber(value)) return [run(open.type.kind === "primitive" && open.type.name === "DECIMAL" ? compactDecimal(value.text) : value.text,"literal")];
  if (typeof value === "string") return textRuns(value, "literal", walk);
  const described = typeShapeOf(value);
  if (described) return [run(typeLine(described), "ink")];
  if (Array.isArray(value)) return [run(value.length === 0 ? "no items" : `${describeType(open.type)} · ${grouped(value.length)}`, "faint")];
  if (isObject(value)) return [run(braces(Object.keys(value)), "faint")];
  return [run(String(value), "literal")];
}

/** `{name, rack}` while it is short, `8 fields` when it is not. */
function braces(names: readonly string[]): string {
  if (names.length === 0) return "{}";
  const written = `{${names.join(", ")}}`;
  return written.length <= 32 ? written : `${names.length} fields`;
}

function isScalarLike(type: TypeShape, data: unknown): boolean {
  const open = unwrap(type, data);
  if (open.none || open.data === null || open.data === undefined) return true;
  if (typeShapeOf(open.data)) return true;
  if (open.data instanceof DecodedBytes) return open.data.text === undefined || !open.data.text.replace(/\n$/, "").includes("\n");
  if (typeof open.data === "string") return !open.data.replace(/\n$/, "").includes("\n");
  return typeof open.data !== "object";
}

function numeric(value: unknown): number | undefined {
  const numeric=drawingNumber(value);
  if (numeric!==undefined) return numeric;
  if (typeof value === "string") return drawingTextNumber(value);
  return undefined;
}

// ------------------------------------------------------------------------------------------ nodes

function lineNode(path: string, runs: readonly Run[], walk: Walk, more?: More): PresentationNode {
  const fitted = fitRuns(runs, walk.context.columns);
  return { kind: "line", path, runs: fitted, ...(more ? { more } : {}) };
}

/** Runs cut to `columns` display columns, ending in `…`. Tier 2 decides what fits. */
function fitRuns(runs: readonly Run[], columns: number): Run[] {
  const out: Run[] = [];
  let used = 0;
  for (const part of runs) {
    const w = width(part.text, columns - used);
    if (used + w <= columns) {
      out.push(part);
      used += w;
      continue;
    }
    const room = columns - used;
    if (room > 0) out.push(run(ellipsizeEnd(part.text, room), part.tone));
    break;
  }
  return out;
}

function textNode(path: string, text: string, walk: Walk, decoded: boolean, foldable = false): PresentationNode {
  if (text === "") return { kind: "empty", path, text: "empty" };
  const newline = text.endsWith("\n");
  const all = (newline ? text.slice(0, -1) : text).split("\n");
  // A foldable block is the whole value by definition: it wraps whatever the policy says of text.
  const wrapped = walk.policy.wrapText || foldable ? all.flatMap((line) => wrap(line, walk.context.columns)) : all;
  const lines: Run[][] = [];
  for (const line of wrapped) {
    if (!spend(walk)) break;
    const runs: Run[] = [];
    line.split("\t").forEach((part, at) => {
      if (at > 0) runs.push(run("⇥", "faint"));
      if (part !== "") runs.push(run(part, "literal"));
    });
    lines.push(fitRuns(runs, walk.context.columns));
  }
  const left = wrapped.length - lines.length;
  if (decoded) walk.notices.includes("Bytes read as utf-8") || walk.notices.push("Bytes read as utf-8");
  return { kind: "text", path, lines, newline: newline && left === 0, ...(foldable ? { disclosure: "folds" as const } : {}), ...(left > 0 ? { more: { lines: left, exact: walk.facts.whole !== false } } : {}) };
}

/** The single-line text of a scalar, when it is one: a string or decoded Bytes without a newline inside. */
function singleLineText(type: TypeShape, data: unknown): string | undefined {
  const open = unwrap(type, data);
  const described = typeShapeOf(open.data);
  if (described) return typeLine(described);
  const value = open.data instanceof DecodedBytes ? open.data.text : open.data;
  if (typeof value !== "string") return undefined;
  return value.replace(/\n$/, "").includes("\n") ? undefined : value;
}

/** The whole form of a scalar shown as a block: a described type opens field by field. */
function wholeText(type: TypeShape, data: unknown, text: string, format?: Format): string {
  const described = typeShapeOf(unwrap(type, data).data);
  if (described) return typeStructure(described);
  return format?.kind === "type" ? prettyType(text) : text;
}

/**
 * One rule for every scalar drawn on its own row (fields, the top-level value): a value that fits
 * its columns is a line. One that does not is laid out `OPENER` columns narrower, because the
 * renderer draws a ▸ or ▾ before it: a line cut to those columns with `more.chars` in preview —
 * the ▸ opens the value whole, in place — and a wrapped, foldable text block when expanded, in the
 * window, or opened by hand. A `type` format is applied only to the block form: on one line the
 * expression stays as written.
 */
function scalarNode(path: string, type: TypeShape, data: unknown, walk: Walk, format?: Format): PresentationNode | undefined {
  const text = singleLineText(type, data);
  const identifier = text !== undefined && isIdentifier(text);
  // A described record type is a structure however short its one-line form: it opens field by field.
  const described = typeShapeOf(unwrap(type, data).data);
  const structured = described !== undefined && typeStructure(described).includes("\n");
  const wide = text !== undefined && !identifier && (structured || width(text, walk.context.columns + 1) > walk.context.columns);
  if (!wide || text === undefined) return spend(walk) ? lineNode(path, scalarRuns(type, data, walk, format, walk.declared(path)), walk) : undefined;
  const beside = narrowed(walk, OPENER);
  const closed = walk.context.closed?.has(path) === true;
  const whole = wholeText(type, data, text, format);
  if (!closed && walk.policy.blockWideText) return textNode(path, whole, beside, false, true);
  if (!closed && walk.context.open?.has(path)) return spend(walk) ? textNode(path, whole, narrowed(ownBudget(walk), OPENER), false, true) : undefined;
  if (!spend(walk)) return undefined;
  const node = lineNode(path, scalarRuns(type, data, beside, format, walk.declared(path)), beside);
  const more: More = structured
    ? { lines: typeStructure(described).split("\n").length - 1, exact: true }
    : { chars: width(text, Number.POSITIVE_INFINITY) - beside.context.columns, exact: true };
  return { ...node, more, disclosure: "opens" };
}

/** The same walk with `by` fewer columns, never under the floor a value needs to say anything. */
function narrowed(walk: Walk, by: number): Walk {
  return { ...walk, context: { ...walk.context, columns: Math.max(8, walk.context.columns - by) } };
}

/**
 * The walk a value opened by hand is drawn with: where the policy gives hand-opened values lines
 * of their own (preview), a budget of that size, so the value is shown whole without the block
 * around it changing — the policy is the same, so everything beside it stays as it was. The row's
 * own line is spent by the caller, from the enclosing budget, as for any row.
 */
function ownBudget(walk: Walk): Walk {
  return walk.policy.ownLines > 0 ? { ...walk, budget: { left: walk.policy.ownLines } } : walk;
}

/** A line cut into pieces of `columns` display columns, for the expanded and window modes. */
function wrap(line: string, columns: number): string[] {
  if (columns <= 0 || width(line) <= columns) return [line];
  const out: string[] = [];
  let piece = "";
  let used = 0;
  for (const cluster of clusters(line)) {
    const w = width(cluster);
    if (used + w > columns) {
      out.push(piece);
      piece = "";
      used = 0;
    }
    piece += cluster;
    used += w;
  }
  if (piece !== "") out.push(piece);
  return out;
}

function bytesNode(path: string, bytes: DecodedBytes, walk: Walk): PresentationNode {
  if (bytes.pending) return lineNode(path, [run("decoding…", "faint")], walk);
  if (bytes.text !== undefined) return textNode(path, bytes.text, walk, true);
  spend(walk);
  return { kind: "bytes", path, size: bytes.size, note: bytes.problem ?? "not text" };
}

/** Whether a nested value at `depth` opens, by hand first and by the mode's depth budget second. */
function opens(path: string, depth: number, walk: Walk): boolean {
  if (walk.context.closed?.has(path)) return false;
  if (walk.context.open?.has(path)) return true;
  return depth <= walk.policy.depth;
}

/** A nested value kept closed: its summary, with the `▸` that opens it. */
function closedNested(path: string, summary: string, walk: Walk, more?: More): PresentationNode | undefined {
  if (!spend(walk)) return undefined;
  return { kind: "nested", path, summary, disclosure: "opens", ...(more ? { more } : {}) };
}

function fieldsNode(path: string, type: TypeShape, data: Readonly<Record<string, unknown>>, walk: Walk, depth: number, entry?: Entry): PresentationNode {
  const declared = type.kind === "record" && Array.isArray(type.fields) ? type.fields.map((field) => field.name) : [];
  const all = [...declared.filter((name) => name in data), ...Object.keys(data).filter((name) => !declared.includes(name))];
  const order = entry?.columns ? entry.columns.filter((name) => name in data) : all;
  // Every value, nested ones included, starts in the one value column after the widest name.
  const nameWidth = Math.min(MAX_NAME, Math.max(0, ...order.map((name) => width(name, MAX_NAME))));
  const inner = { ...walk, context: { ...walk.context, columns: Math.max(8, walk.context.columns - nameWidth - GAP) } };
  const rows: FieldRow[] = [];
  // A scalar that would not fit its own line is not packed: it keeps its row and its opener.
  const fits = (name: string) => { const text = singleLineText(fieldType(type, name), data[name]); return text === undefined || width(text, inner.context.columns + 1) <= inner.context.columns; };
  const scalars = order.filter((name) => isScalarLike(fieldType(type, name), data[name]) && fits(name));
  const nested = order.filter((name) => !scalars.includes(name));
  // Scalars share lines before the nested values, so a list under them still gets room (policy).
  const pack = walk.policy.packScalars && scalars.length > 1 && nested.length > 0;
  let shown = 0;
  if (pack) {
    for (const line of packScalars(path, scalars, type, data, walk, entry)) {
      if (!spend(walk)) break;
      rows.push({ name: "", node: { kind: "line", path: `${path}/`, runs: line.runs } });
      shown += line.count;
    }
  }
  for (const name of pack ? nested : order) {
    if (walk.budget.left <= 0) break;
    const at = childPath(path, name);
    const fieldShape = fieldType(type, name);
    const value = data[name];
    if (isScalarLike(fieldShape, value)) {
      const node = scalarNode(at, fieldShape, value, inner, entry?.formats[name]);
      if (!node) break;
      rows.push({ name, node });
      shown += 1;
      continue;
    }
    // Text is drawn beside its name, in the value column; a nested value's body goes under the
    // name, `INDENT` columns in, so it gets the record's width less that indent.
    const node = presentAt(at, fieldShape, value, isTextField(fieldShape, value) ? inner : walk, depth + 1, name);
    if (!node) break;
    rows.push({ name, node });
    shown += 1;
  }
  const left = order.length - shown;
  return { kind: "fields", path, rows, nameWidth, tree: { type, data, mode: walk.context.mode, ...declaredAt(walk, path) }, ...(left > 0 ? { more: { fields: left, exact: walk.facts.whole !== false } } : {}) };
}

/** A multi-line text field: drawn beside its name, continued under the value column. */
function isTextField(type: TypeShape, data: unknown): boolean {
  const value = unwrap(type, data).data;
  return typeof value === "string" || (value instanceof DecodedBytes && value.text !== undefined);
}

function packScalars(path: string, names: readonly string[], type: TypeShape, data: Readonly<Record<string, unknown>>, walk: Walk, entry?: Entry) {
  const lines: { readonly runs: Run[]; readonly count: number }[] = [];
  let line: Run[] = [];
  let used = 0;
  let onLine = 0;
  for (const name of names) {
    const value = scalarRuns(fieldType(type, name), data[name], walk, entry?.formats[name], walk.declared(childPath(path, name)));
    const piece = [run(name, "param"), run(" ", "faint"), ...value];
    const w = piece.reduce((sum, it) => sum + width(it.text), 0);
    if (used > 0 && used + 3 + w > walk.context.columns) {
      lines.push({ runs: line, count: onLine });
      line = [];
      used = 0;
      onLine = 0;
      if (lines.length >= walk.policy.packedLines) return lines;
    }
    if (used > 0) {
      line.push(run("   ", "faint"));
      used += 3;
    }
    line.push(...piece);
    used += w;
    onLine += 1;
  }
  if (line.length > 0) lines.push({ runs: line, count: onLine });
  return lines;
}

function disclosureWalk(walk: Walk, path: string, value: unknown): Walk {
  const pages = new Map(walk.context.pages);
  if (Array.isArray(value) && !pages.has(path)) pages.set(path, 0);
  return { ...walk, context: { ...walk.context, pages }, facts: { ...walk.facts, stopped: false },
    policy: POLICIES.expanded, budget: { left: expanded_lines() } };
}

/**
 * A list of records as a table: every column the type declares, in the type's order, then the
 * keys the shown rows add. The block's width never drops a column — the renderer scrolls the
 * table sideways, its key column staying put — so the only columns not drawn are the ones a
 * registry entry chose to leave out, named in `more.columns`. Width cuts cells, never columns.
 */
/**
 * Text as a filter compares it: case folded, with the dotted and dotless i of Turkish and Azerbaijani
 * read as one letter, so `YILMAZ`, `yılmaz` and `Yilmaz` find each other in any locale.
 */
export function folded(text: string): string {
  return text.toLowerCase().replace(/\u0307/g, "").replace(/ı/g, "i");
}

function tableNode(path: string, element: TypeShape, rows: readonly unknown[], walk: Walk, entry?: Entry): PresentationNode {
  const declared = element.kind === "record" && Array.isArray(element.fields) ? element.fields.map((field) => field.name) : [];
  /** The type's columns, then the keys the given rows add, in order. */
  const columnsOf = (some: readonly unknown[]) => {
    const seen = [...declared];
    for (const row of some) if (isObject(row)) for (const name of Object.keys(row)) if (!seen.includes(name)) seen.push(name);
    return seen;
  };
  const rowsAt = walk.declared(path);
  const plainCellOf = (row: unknown, name: string) => scalarRuns(fieldType(element, name), isObject(row) ? row[name] : undefined, walk, entry?.formats[name], rowsAt === undefined ? undefined : throughOptions(`${rowsAt}${ELEMENT}${fieldSegment(name)}`, fieldType(element, name)));

  const cellOf = (row: unknown, name: string) => entry ? declaredCell(entry, row, name, element, plainCellOf(row, name)).runs : plainCellOf(row, name);

  // Filtered rows keep their own index, so a row's disclosures keep their paths. A filter reads
  // every column any row has, so a match in a hidden or later column still counts.
  const query = folded(walk.context.filters?.get(path)?.trim() ?? "");
  const indexed = rows.map((row, index) => ({ row, index }));
  const searched = query ? columnsOf(rows) : [];
  const kept = query
    ? indexed.filter(({ row }) => searched.some((name) => folded(cellOf(row, name).map((it) => it.text).join("")).includes(query)))
    : indexed;
  const sort=walk.context.sorts?.get(path);
  if(sort)kept.sort((a,b)=>{
    const av=unwrap(fieldType(element,sort.column),isObject(a.row)?a.row[sort.column]:undefined).data;
    const bv=unwrap(fieldType(element,sort.column),isObject(b.row)?b.row[sort.column]:undefined).data;
    // Missing values are always last; exact integers/decimals never pass through Number.
    if(av===undefined || av===null)return bv===undefined || bv===null ? a.index-b.index : 1;
    if(bv===undefined || bv===null)return -1;
    const compared=isNumeric(av)&&isNumeric(bv) ? compareNumeric(av,numericText(bv)) : String(av).localeCompare(String(bv));
    return (sort.descending ? -compared : compared) || a.index-b.index;
  });
  const total = kept.length;

  // Which rows: a stopped stream shows its last window, everything else its first. Rows asked
  // for by hand under this table win over the block's budget.
  const asked = walk.policy.rows(walk.budget.left, walk.context.rows ?? pageSize());
  const shown = walk.context.shown?.get(path);
  const page = walk.context.pages?.get(path);
  const pageOffset = Math.min(Math.max(0, Math.ceil(total / pageSize()) - 1), page ?? 0) * pageSize();
  const count = Math.max(0, Math.min(total - pageOffset,
    page !== undefined ? Math.min(pageSize(), asked) : shown !== undefined ? Math.max(shown, asked) : asked));
  const offset = page === undefined && shown === undefined && !query && walk.facts.stopped ? total - count : pageOffset;
  const window = kept.slice(offset, offset + count);
  // The columns are the shown rows' own: a later page names the keys its rows have.
  const seen = columnsOf(window.map(({ row }) => row));
  const offered = entry?.columns ?? seen;
  const hiddenByEntry = entry?.columns ? seen.filter((name) => !entry.columns!.includes(name)) : [];
  const key = tableKey(element, declared.length ? declared : offered);
  const view = walk.context.tables?.[key];
  // The key column is how a row is known: it can scroll away, never be hidden.
  const hidden = (view?.hidden ?? []).filter((name) => name !== offered[0] && offered.includes(name));
  const wanted = offered.filter((name) => !hidden.includes(name));

  const cells = window.map(({ row }) => wanted.map((name) => cellOf(row, name)));
  const decorated = entry && (entry.styles || entry.rules) ? window.map(({ row }) => wanted.map((name) => declaredCell(entry, row, name, element, []).style)) : undefined;
  const tints = entry?.rules?.some((rule) => "row" in rule.target) ? window.map(({ row }) => rowTint(entry, row, element)) : undefined;
  const cap = walk.policy.cell;
  const widths = wanted.map((name, at) => {
    const set = view?.widths?.[name];
    return set ?? Math.min(cap, Math.max(width(name, cap), ...cells.map((row) => row[at]!.reduce((sum, it) => sum + width(it.text, cap), 0))));
  });
  const columns: Column[] = wanted.map((name, at) => ({
    name, width: widths[at]!, key: at === 0,
    label: ellipsizeEnd(name, widths[at]!), sized: view?.widths?.[name] !== undefined,
    numeric: window.every(({ row }) => numeric(isObject(row) ? unwrap(fieldType(element, name), row[name]).data : undefined) !== undefined),
  }));
  // Cells, each cut to its column: the renderer lays the grid out, so no padding is drawn here.
  const drawn = cells.map((row) => row.map((part, at) => fitRuns(part, columns[at]!.width)));
  if (count > 0 || total === 0) spend(walk, 1 + count);
  const leftRows = page === undefined ? total - count : total - offset - count;
  const more: More | undefined = leftRows > 0 || hiddenByEntry.length > 0
    ? { ...(leftRows > 0 ? { rows: leftRows } : {}), ...(hiddenByEntry.length > 0 ? { columns: hiddenByEntry } : {}), exact: walk.facts.whole !== false }
    : undefined;
  if (entry) for (const it of entry.offers) offer(walk, it.kind);
  const details = window.map(({ row, index }) => wanted.map((name) => {
    const type = fieldType(element, name);
    const raw = isObject(row) ? row[name] : undefined;
    const opened = unwrap(type, raw);
    const value = opened.data;
    if (opened.none || !(Array.isArray(value) ? value.length > 0 : isObject(value) && Object.keys(value).length > 0) || typeShapeOf(value)) return undefined;
    const at = childPath(childPath(path, index), name);
    const summary = summaryText(opened.type, value);
    if (!walk.context.open?.has(at) || walk.context.closed?.has(at)) {
      return { kind: "nested", path: at, summary, disclosure: "opens" } as PresentationNode;
    }
    // Explicit disclosure owns a bounded viewport, independent of the parent row budget.
    const inner = disclosureWalk(walk, at, value);
    const body = presentValue(at, opened.type, value, inner, EXPANDED_DEPTH + 1);
    return { kind: "nested", path: at, summary, disclosure: "folds", ...(body ? { body } : {}) } as PresentationNode;
  }));
  const arrangement: TableArrangement = {
    key, columns: offered, hidden, unpinned: view?.unpinned ?? (widths[0]!==undefined && widths[0]>walk.context.columns*0.45), step: walk.policy.step,
    ...(query ? { filter: { query: walk.context.filters!.get(path)!, matched: total, of: rows.length } } : {}),
  };
  return { kind: "table", path, columns, rows: drawn, ...(decorated?.some((row) => row.some(Boolean)) ? { styles: decorated } : {}), ...(tints?.some(Boolean) ? { tints } : {}), offset, total, arrangement, ...(sort ? {sort} : {}),
    inspection:{type:element,mode:walk.context.mode,whole:walk.facts.whole!==false,...declaredAt(walk,path,ELEMENT),rows:window.map(({row,index})=>({index,value:row}))},
    ...(details.some(row => row.some(Boolean)) ? { details } : {}),
    ...(page !== undefined ? { pagination: { offset, shown: count, total } } : {}), ...(more ? { more } : {}) };
}

function itemsNode(path: string, element: TypeShape, items: readonly unknown[], walk: Walk): PresentationNode {
  const exact = walk.facts.whole !== false;
  const listAt = walk.declared(path), itemsAt = listAt === undefined ? undefined : throughOptions(`${listAt}${ELEMENT}`, element);
  if (!walk.policy.packItems) {
    // One element per line, paged like a table: the page size grows with "show N more".
    const page = walk.context.pages?.get(path);
    const offset = Math.min(Math.max(0, Math.ceil(items.length / pageSize()) - 1), page ?? 0) * pageSize();
    const asked = Math.min(items.length - offset, page === undefined ? walk.context.rows ?? pageSize() : pageSize());
    const lines: Run[][] = [];
    for (const item of items.slice(offset, offset + asked)) {
      if (!spend(walk)) break;
      lines.push(scalarRuns(element, item, walk, undefined, itemsAt));
    }
    const left = items.length - offset - lines.length;
    return { kind: "items", path, items: lines[0] ?? [], lines, ...(page !== undefined ? { pagination: { offset, shown: lines.length, total: items.length } } : {}), ...(left > 0 ? { more: { items: left, exact } } : {}) };
  }
  if (!spend(walk)) return { kind: "items", path, items: [], more: { items: items.length, exact } };
  const out: Run[] = [];
  let used = 0;
  let shown = 0;
  for (const item of items) {
    const runs = scalarRuns(element, item, walk, undefined, itemsAt);
    const w = runs.reduce((sum, it) => sum + width(it.text), 0) + (shown > 0 ? 3 : 0);
    if (used + w > walk.context.columns - 12 && shown > 0) break;
    if (shown > 0) out.push(run(" · ", "faint"));
    out.push(...runs);
    used += w;
    shown += 1;
  }
  const left = items.length - shown;
  return { kind: "items", path, items: out, ...(left > 0 ? { more: { items: left, exact } } : {}) };
}

function processNode(path: string, type: TypeShape, data: Readonly<Record<string, unknown>>, walk: Walk): PresentationNode {
  const exit = numeric(data.exitCode);
  const inner = { ...walk, context: { ...walk.context, columns: Math.max(8, walk.context.columns - 8) } };
  const stream = (name: "stdout" | "stderr") => {
    const value = data[name];
    if (value instanceof DecodedBytes) return bytesNode(`${path}/${name}`, value, inner);
    if (typeof value === "string") return textNode(`${path}/${name}`, value, inner, false);
    spend(walk);
    return lineNode(`${path}/${name}`, scalarRuns(fieldType(type, name), value, walk, undefined, walk.declared(childPath(path, name))), inner);
  };
  const stdout = stream("stdout");
  const stderr = stream("stderr");
  return { kind: "process", path, ...(exit !== undefined ? { exit } : {}), stdout, stderr };
}


function isHelp(type: TypeShape, data: unknown): boolean {
  return type.kind === "record" && type.name === "wes.Help" && isObject(data) && typeof data.path === "string" && Array.isArray(data.children);
}

function isGraph(data: unknown): boolean {
  return isObject(data) && Array.isArray(data.nodes) && Array.isArray(data.edges) && !("view" in data);
}

/**
 * The presentation of one value at `path`, or undefined when the budget has no line left for it.
 * `label` names a nested value: text is drawn beside its name; anything bigger is a `nested` node
 * whose summary sits in the value column and whose body, when open, sits under the name,
 * `INDENT` columns in — one nesting level, whatever the record's name column is.
 */
function presentAt(path: string, type: TypeShape, data: unknown, walk: Walk, depth: number, label?: string): PresentationNode | undefined {
  const open = unwrap(type, data);
  if (open.none) return spend(walk) ? { kind: "empty", path, text: "none" } : undefined;
  const value = open.data;
  const shape = open.type;
  if (value === null || value === undefined) return spend(walk) ? { kind: "empty", path, text: value === null ? "null" : "empty" } : undefined;
  if (value instanceof DecodedBytes) return bytesNode(path, value, walk);
  if (typeof value === "string") {
    if (value.replace(/\n$/, "").includes("\n")) return textNode(path, value, walk, false);
    return scalarNode(path, shape, value, walk);
  }
  if (typeShapeOf(value)) return scalarNode(path, shape, value, walk);
  if (label === undefined) return presentValue(path, shape, value, walk, depth);
  if (isExactNumber(value) || typeof value !== "object") return spend(walk) ? lineNode(path, scalarRuns(shape, value, walk, undefined, walk.declared(path)), walk) : undefined;
  const summary = summaryText(shape, value);
  if (!opens(path, depth, walk)) return closedNested(path, summary, walk);
  if (!spend(walk)) return undefined;
  const inner=narrowed(walk.context.open?.has(path) ? disclosureWalk(walk,path,value) : walk,INDENT);
  if(inner.budget.left<=0)return {kind:"nested",path,summary,disclosure:"opens"};
  const body = presentValue(path, shape, value, inner, depth);
  // A table that got its header line but no row says nothing: fold it back to its summary, the
  // rows counted in the tail. The header's line was already spent, so no second spend here.
  if (body?.kind === "table" && body.rows.length === 0 && body.total > 0) {
    return { kind: "nested", path, summary, disclosure: "opens", more: { rows: body.total, exact: walk.facts.whole !== false } };
  }
  // The node carries no `more` of its own: `mores()` walks into the body, and copying the body's
  // count here doubled it in the tail (`+20 rows` for ten rows).
  return { kind: "nested", path, summary, disclosure: "folds", ...(body ? { body } : {}) };
}

/** The selection order for a value that is not a string, Bytes or empty. */
function presentValue(path: string, shape: TypeShape, value: unknown, walk: Walk, depth: number, requested?: string): PresentationNode | undefined {
  if (isHelp(shape, value)) {
    if (!spend(walk)) return undefined;
    return { kind: "custom", path, name: "help", data: value };
  }
  for (const notice of walk.registry.noticesFor(shape)) if (!walk.notices.includes(notice)) walk.notices.push(notice);
  const matched = walk.registry.match(shape, value);
  const module = valueViewModules.find(shape, value, requested ?? matched?.entry.kind, walk.viewModules);
  if (module && !walk.skipViews.has(module.id)) {
    const before = walk.budget.left;
    const offersBefore = walk.offers.length, noticesBefore = walk.notices.length;
    try {
      const shown = module.present({ path, type: shape, data: value, context: walk.context }, {
        remaining: () => walk.budget.left,
        spend: lines => { walk.budget.left = Math.max(0, walk.budget.left - Math.max(0, lines)); },
        child: (name, type, data, options) => (options
          ? presentValue(`${path}/${encodeURIComponent(name)}`, type, data, walk, depth + 1, options.view)
          : presentAt(`${path}/${encodeURIComponent(name)}`, type, data, walk, depth + 1))
          ?? { kind: "line", path: `${path}/${name}`, runs: [{ text: "…", tone: "faint" }], more: { fields: 1, exact: false } },
      });
      offer(walk, valueViewModules.address(module));
      return { kind: "view", path, view: module.id, ...shown, fallback: { viewModules: walk.viewModules, type: shape, data: value, context: walk.context } };
    } catch (error) {
      walk.budget.left = before;
      walk.offers.length = offersBefore;
      walk.notices.length = noticesBefore;
      walk.notices.push(error instanceof ViewInputError ? `View ${module.id}: ${error.message}; showing data` : `View ${module.id} unavailable; showing data`);
    }
  }
  if (matched) {
    const { entry, list } = matched;
    if (list && Array.isArray(value)) return tableNode(path, shape.kind === "list" ? shape.element : { kind: "unknown" }, value, walk, entry);
    if (isObject(value)) {
      if (entry.kind === "process") return processNode(path, shape, value, walk);
      const node = fieldsNode(path, shape, value, walk, depth, entry);
      for (const it of entry.offers) offer(walk, it.kind);
      return node;
    }
  }
  if (isGraph(value)) {
    if (!spend(walk)) return undefined;
    return { kind: "custom", path, name: "graph", data: value };
  }
  // 3. Structural rules.
  if (Array.isArray(value)) {
    const element = elementOf(shape);
    if (value.length === 0) return spend(walk) ? { kind: "empty", path, text: "no items" } : undefined;
    if (value.every((item) => isObject(unwrap(element, item).data))) return tableNode(path, element, value, walk);
    if(value.some(item=>!isScalarLike(element,item))) {
      const page=walk.context.pages?.get(path);
      const offset=Math.min(Math.max(0,Math.ceil(value.length/pageSize())-1),page??0)*pageSize();
      const asked=Math.min(value.length-offset,page===undefined?walk.context.rows??pageSize():pageSize());
      const rows:FieldRow[]=[];
      const nameWidth=Math.min(MAX_NAME,String(offset+asked-1).length+2);
      for(let i=offset;i<offset+asked && walk.budget.left>0;i++) {
        const name=`[${i}]`,node=presentAt(childPath(path,String(i)),element,value[i],narrowed(walk,nameWidth+GAP),depth+1,name);
        if(!node)break;rows.push({name,node});
      }
      const left=value.length-offset-rows.length;
      return {kind:"fields",path,rows,nameWidth,...(page!==undefined?{pagination:{offset,shown:rows.length,total:value.length}}:{}),...(left>0?{more:{items:left,exact:walk.facts.whole!==false}}:{})};
    }
    return itemsNode(path, element, value, walk);
  }
  if (isObject(value)) {
    if (Object.keys(value).length === 0) return spend(walk) ? { kind: "empty", path, text: "empty" } : undefined;
    return fieldsNode(path, shape, value, walk, depth);
  }
  // 4. A line for whatever is left.
  return spend(walk) ? lineNode(path, scalarRuns(shape, value, walk, undefined, walk.declared(path)), walk) : undefined;
}

/** What a closed nested value says beside its `▸`. */
function summaryText(shape: TypeShape, value: unknown): string {
  if (Array.isArray(value)) return value.length === 0 ? "no items" : `${shape.kind==="unknown"?"List":describeType(shape)} · ${grouped(value.length)}`;
  if (isObject(value)) return braces(Object.keys(value));
  return describeType(shape);
}

/** The line the verdict may lift out of the value. */
function summaryOf(root: PresentationNode, shape: TypeShape, value: unknown): Summary {
  if (root.kind === "process" && root.exit !== undefined) return { facts: [run(`exit ${root.exit}`, root.exit === 0 ? "ok" : "warn")] };
  if (root.kind === "view") return { facts: root.summary };
  const open = unwrap(shape, value);
  if (Array.isArray(open.data)) return { facts: [run(grouped(open.data.length), "dim")] };
  return { facts: [] };
}

/** Lines one field row takes: a nested value's summary shares the name's line, its body follows. */
export function rowLines(row: FieldRow): number {
  if (row.name === "") return linesOf(row.node);
  if (row.node.kind === "text") return Math.max(1, row.node.lines.length);
  if (row.node.kind === "nested") return linesOf(row.node);
  return beside(row.node) ? 1 : 1 + linesOf(row.node);
}

/** Kinds drawn beside a field name on the same line. */
export const BESIDE: ReadonlySet<PresentationNode["kind"]> = new Set(["line", "empty", "bytes", "nested", "items"]);

/** Whether a node fits beside its field name: a packed items line does, a list of lines does not. */
export function beside(node: PresentationNode): boolean {
  if (node.kind === "items") return (node.lines?.length ?? 1) <= 1;
  if (node.kind === "nested") return node.body === undefined;
  return BESIDE.has(node.kind);
}

export function linesOf(node: PresentationNode): number {
  switch (node.kind) {
    case "fields": return node.rows.reduce((sum, row) => sum + rowLines(row), 0);
    case "table": return node.rows.length + 1 + (node.details?.flat().reduce((sum, detail) => sum + (detail?.kind === "nested" && detail.body ? 1 + linesOf(detail.body) : 0), 0) ?? 0);
    case "text": return node.lines.length;
    case "process": return linesOf(node.stdout) + linesOf(node.stderr);
    case "view": return node.ownLines + node.children.reduce((sum, child) => sum + linesOf(child), 0);
    case "notice": return node.lines.length;
    case "stream": return node.body ? linesOf(node.body) : 1;
    case "items": return Math.max(1, node.lines?.length ?? 1);
    case "nested": return 1 + (node.body ? linesOf(node.body) : 0);
    default: return 1;
  }
}

export function present({ prepared, facts = {}, context, registry, skipViews = new Set<string>() }: PresentInput): Presentation {
  const meta = prepared.meta;
  const walk: Walk = { viewModules: prepared.viewModules, skipViews, context, policy: POLICIES[context.mode], registry, facts, budget: { left: Math.max(1, context.lines) }, offers: [], notices: [],
    ...(meta ? { meta } : {}), declared: pointer => meta ? declarationPath(prepared.type, pointer) : undefined };
  const root = presentAt("", prepared.type, prepared.data, walk, 0) ?? { kind: "empty", path: "", text: "empty" };
  return { root, lines: linesOf(root), summary: summaryOf(root, prepared.type, prepared.data), notices: walk.notices, offers: walk.offers };
}

/** Every `more` in the tree, gathered in document order. */
function mores(node: PresentationNode, out: More[] = [], treeOwnsCounts = false): More[] {
  if (treeOwnsCounts && node.kind === "fields" && node.tree) return out;
  // A table says its own left-out rows under itself, where its `show more` is; the tail does not repeat them.
  if (node.more) out.push(node.kind === "table" ? { ...node.more, rows: undefined } : node.more);
  if (node.kind === "fields") for (const row of node.rows) if (row.node !== node) mores(row.node, out, treeOwnsCounts);
  if (node.kind === "table") for (const detail of node.details?.flat() ?? []) if (detail?.kind === "nested" && detail.body && !detail.body.pagination) mores(detail.body, out, treeOwnsCounts);
  if (node.kind === "process") { mores(node.stdout, out, treeOwnsCounts); mores(node.stderr, out, treeOwnsCounts); }
  if (node.kind === "view") for (const child of node.children) mores(child, out, treeOwnsCounts);
  if ((node.kind === "stream" || node.kind === "nested") && node.body) mores(node.body, out, treeOwnsCounts);
  return out;
}

/**
 * What the tail says about this value: counts of what did not fit, each kind kept apart, then the
 * hidden columns by name and the notices. `+N` is only a total when the value was read whole.
 * With `paged`, the root's rows or items are left to the pager drawn under the value.
 * With `treeOwnsCounts`, structural renderers report their own omissions locally.
 */
export function tailOf(presentation: Presentation, paged = false, treeOwnsCounts = false): Run[] {
  const found = mores(presentation.root, [], treeOwnsCounts);
  // A pager under the value already says how many rows or items its root left out.
  const pagedOut = paged && presentation.root.kind !== "table" ? presentation.root.more : undefined;
  const sum = (key: "rows" | "fields" | "lines" | "items" | "headers") => found.reduce((total, more) => total + (more[key] ?? 0), 0)
    - (pagedOut && (key === "rows" || key === "items") ? pagedOut[key] ?? 0 : 0);
  const exact = found.every((more) => more.exact);
  const plus = exact ? "+" : "+≥";
  const parts: string[] = [];
  const count = (n: number, one: string, many: string) => { if (n > 0) parts.push(`${plus}${grouped(n)} ${n === 1 ? one : many}`); };
  count(sum("rows"), "row", "rows");
  count(sum("fields"), "field", "fields");
  count(sum("lines"), "line", "lines");
  count(sum("items"), "item", "items");
  count(sum("headers"), "header", "headers");
  const hidden = [...new Set(found.flatMap((more) => more.columns ?? []))];
  if (hidden.length > 0) parts.push(`not shown: ${hidden.join(", ")}`);
  parts.push(...presentation.notices);
  return parts.length === 0 ? [] : [run(parts.join(" · "), "faint")];
}

/** Text of runs, for tests and accessible labels. */
export function runText(runs: readonly Run[]): string { return runs.map(it=>it.text).join(""); }
export { pad };
