/**
 * The draft source editor: CodeMirror dressed for the draft grammar.
 *
 * It judges nothing. Syntax and semantics both come from the backend's validation, pushed in as
 * marks tied to the exact text they were computed for; the first edit after that turns them stale
 * (drawn hollow and faint) and they neither block nor approve anything until a new answer lands.
 * Completion offers only grammar keys, HTTP methods, native type names and this draft's own types;
 * status and media candidates appear only when asked for, and nothing is inserted until chosen.
 */
import { EditorState, RangeSet, StateEffect, StateField, type Extension, type Text } from "@codemirror/state";
import { Decoration, EditorView, GutterMarker, gutter, keymap, lineNumbers, type DecorationSet } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { indentRange, indentService, indentUnit } from "@codemirror/language";
import { autocompletion, closeCompletion, completionKeymap, startCompletion, type Completion, type CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import { cursorAt, type DraftSeverity } from "../draft-api";
import { jsonSyntax } from "./json-syntax";

export const DRAFT_INDENT = 2;
const UNIT = " ".repeat(DRAFT_INDENT);

/* ---------- completion ---------- */

export const HTTP_METHODS = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "TRACE"] as const;
export const NATIVE_TYPES = ["Text", "Int", "Decimal", "Bool", "Instant", "Duration", "Interval", "Bytes", "Unknown", "Record"] as const;
const GENERIC_TYPES = ["List<>", "Map<Text,>", "Option<>", "Union<,>"] as const;
const KEYS: Record<string, readonly string[]> = {
  root: ["draftVersion", "provider", "types", "operations", "problems", "diagnostics"],
  operation: ["path", "method", "route", "summary", "auth", "parameters", "responses"],
  parameter: ["name", "wire", "location", "type", "required", "encoding"],
  response: ["status", "mediaType", "type"],
  problem: ["target", "message"],
  type: ["base", "fields", "enum", "min", "max", "minLength", "maxLength", "minItems", "maxItems"],
  field: ["type", "optional"],
  auth: ["scheme", "secret"],
};
const LOCATIONS = ["path", "query", "header", "body"];
const ENCODINGS = ["scalar", "repeat", "deepObject", "json"];
/** Status and media candidates: grammar only, never a guess about this API. */
const STATUSES = ["200", "201", "202", "204", "null"];
const MEDIA = ['"application/json"', "null"];

type Slot = { kind: "key"; keys: readonly string[] } | { kind: "value"; values: string[]; info?: string };

/** What belongs at the caret, from the containers open around it. */
export function draftSlot(text: string, offset: number, typeNames: readonly string[]): Slot | undefined {
  const { path, position } = cursorAt(text, offset);
  const shape = path.map(p => typeof p === "number" ? "#" : p);
  const at = (...pattern: string[]) => pattern.length === shape.length && pattern.every((p, i) => p === "*" || p === shape[i]);
  const types = [...NATIVE_TYPES, ...GENERIC_TYPES, ...typeNames.filter(n => !(NATIVE_TYPES as readonly string[]).includes(n))].map(t => JSON.stringify(t));
  if (position === "key") {
    if (at()) return { kind: "key", keys: KEYS.root! };
    if (at("operations", "#")) return { kind: "key", keys: KEYS.operation! };
    if (at("operations", "#", "parameters", "#")) return { kind: "key", keys: KEYS.parameter! };
    if (at("operations", "#", "responses", "#")) return { kind: "key", keys: KEYS.response! };
    if (at("operations", "#", "auth", "#")) return { kind: "key", keys: KEYS.auth! };
    if (at("problems", "#")) return { kind: "key", keys: KEYS.problem! };
    if (at("types", "*")) return { kind: "key", keys: KEYS.type! };
    if (at("types", "*", "fields", "*")) return { kind: "key", keys: KEYS.field! };
    return undefined;
  }
  if (position !== "value") return undefined;
  if (at("operations", "#", "method")) return { kind: "value", values: HTTP_METHODS.map(m => JSON.stringify(m)) };
  if (at("operations", "#", "parameters", "#", "location")) return { kind: "value", values: LOCATIONS.map(l => JSON.stringify(l)) };
  if (at("operations", "#", "parameters", "#", "encoding")) return { kind: "value", values: ENCODINGS.map(e => JSON.stringify(e)) };
  if (at("operations", "#", "parameters", "#", "required")) return { kind: "value", values: ["true", "false", "null"] };
  if (at("operations", "#", "parameters", "#", "type") || at("types", "*", "fields", "*", "type") || at("types", "*", "base") || at("types", "*", "fields", "*") || at("types", "*")) return { kind: "value", values: types };
  if (at("operations", "#", "responses", "#", "type")) return { kind: "value", values: [...types, "null"], info: "null is an empty body; leave the key out when unknown" };
  if (at("operations", "#", "responses", "#", "status")) return { kind: "value", values: STATUSES, info: "status key · from the guide or your own" };
  if (at("operations", "#", "responses", "#", "mediaType")) return { kind: "value", values: MEDIA, info: "media type · from the guide or your own" };
  if (at("types", "*", "fields", "*", "optional")) return { kind: "value", values: ["true", "false"] };
  if (at("draftVersion")) return { kind: "value", values: ["1"] };
  return undefined;
}

/**
 * The token under the caret: the string it is inside (possibly unterminated), or a bare literal.
 * Outside a string a quote is never a token start — after `"status": ` the key's closing quote
 * must not read as an empty string the user began typing.
 */
function tokenAround(context: CompletionContext, inString: boolean): { from: number; to: number; typed: string } {
  const before = context.matchBefore(inString ? /"(?:[^"\\\n]|\\.)*/ : /[A-Za-z0-9_+\-.<>,/]+/);
  const from = before ? before.from : context.pos;
  const line = context.state.doc.lineAt(context.pos);
  const after = /^(?:[^"\\\n,}\]]|\\.)*"?/.exec(context.state.sliceDoc(context.pos, line.to));
  const quoted = before?.text.startsWith('"');
  const to = quoted && after && after[0].endsWith('"') ? context.pos + after[0].length : context.pos;
  return { from, to, typed: before?.text ?? "" };
}

export function draftCompletion(typeNames: () => readonly string[]) {
  return (context: CompletionContext): CompletionResult | null => {
    const text = context.state.doc.toString();
    const slot = draftSlot(text, context.pos, typeNames());
    if (!slot) return null;
    const { from, to, typed } = tokenAround(context, cursorAt(text, context.pos).inString);
    // Nothing opens by itself on an empty token: the list appears when asked for (⌘␣) or once typing starts.
    if (!context.explicit && typed.replace(/^"/, "") === "") return null;
    const options: Completion[] = slot.kind === "key"
      ? slot.keys.map(key => ({
        label: JSON.stringify(key), type: "property", detail: "key",
        apply: (view: EditorView, _c: Completion, f: number, t: number) => {
          const rest = view.state.sliceDoc(t, Math.min(view.state.doc.length, t + 8));
          const insert = /^\s*:/.test(rest) ? JSON.stringify(key) : `${JSON.stringify(key)}: `;
          view.dispatch({ changes: { from: f, to: t, insert }, selection: { anchor: f + insert.length }, userEvent: "input.complete" });
        },
      }))
      : slot.values.map(value => ({
        label: value, type: "constant", ...(slot.info ? { detail: slot.info } : {}),
        apply: (view: EditorView, _c: Completion, f: number, t: number) => {
          // A generic lands with the caret between its brackets, waiting for the element type.
          const hole = value.indexOf("<>") >= 0 ? value.indexOf("<>") + 1 : value.indexOf(",>") >= 0 ? value.indexOf(",>") + 1 : value.length;
          view.dispatch({ changes: { from: f, to: t, insert: value }, selection: { anchor: f + hole }, userEvent: "input.complete" });
        },
      }));
    return { from, to, options, filter: true };
  };
}

/* ---------- indentation ---------- */

/** Bracket depth before `at`, ignoring brackets inside strings. */
function depthAt(doc: string, at: number): number {
  let depth = 0;
  let inString = false;
  for (let i = 0; i < at; i++) {
    const ch = doc[i];
    if (inString) {
      if (ch === "\\") i++;
      else if (ch === '"' || ch === "\n") inString = false;
    } else if (ch === '"') inString = true;
    else if (ch === "{" || ch === "[") depth++;
    else if (ch === "}" || ch === "]") depth = Math.max(0, depth - 1);
  }
  return depth;
}

/** Indentation of the line starting at `at`: one level per open bracket, one less if it begins by closing one. */
export function draftIndentAt(doc: string, at: number): number {
  const rest = doc.slice(at).match(/^[ \t]*([}\]])?/);
  return Math.max(0, depthAt(doc, at) - (rest?.[1] ? 1 : 0)) * DRAFT_INDENT;
}

/*
 * `Enter` asks before the break exists; `textAfterPos` already accounts for the simulated break
 * (empty between `{` and `}`, where CodeMirror opens a blank line and keeps the closer's own indent).
 */
export const draftIndentation: Extension = indentService.of((context, at) => {
  const doc = context.state.doc.toString();
  const start = context.simulatedBreak === at ? at : context.state.doc.lineAt(at).from;
  const closes = /^\s*[}\]]/.test(context.textAfterPos(start, 1));
  return Math.max(0, depthAt(doc, start) - (closes ? 1 : 0)) * DRAFT_INDENT;
});

/** A closing bracket typed first on its line steps that line out one level. */
const outdenting = EditorView.inputHandler.of((view, from, to, text) => {
  if (text !== "}" && text !== "]") return false;
  const line = view.state.doc.lineAt(from);
  if (view.state.doc.sliceString(line.from, from).trim() !== "") return false;
  const doc = view.state.doc.toString();
  const want = Math.max(0, depthAt(doc, line.from) - 1) * DRAFT_INDENT;
  view.dispatch({ changes: { from: line.from, to, insert: `${" ".repeat(want)}${text}` }, selection: { anchor: line.from + want + 1 }, userEvent: "input.type" });
  return true;
});

/** ⇧⌥F: every line re-indented from its brackets. Values are never touched. */
export function reindentDraft(view: EditorView): boolean {
  const changes = indentRange(view.state, 0, view.state.doc.length);
  if (!changes.empty) view.dispatch({ changes, userEvent: "input.indent" });
  return true;
}

/* ---------- backend-owned marks ---------- */

export interface DraftMark { from: number; to: number; severity: DraftSeverity; message: string }
interface MarkState { marks: { from: number; to: number; severity: DraftSeverity; message: string }[]; stale: boolean; decorations: DecorationSet }
export const setDraftMarks = StateEffect.define<{ marks: readonly DraftMark[] }>();

function clampMark(doc: Text, mark: DraftMark): DraftMark {
  const from = Math.max(0, Math.min(mark.from, doc.length));
  return { ...mark, from, to: Math.max(from, Math.min(mark.to, doc.length)) };
}
function decorate(doc: Text, marks: MarkState["marks"], stale: boolean): DecorationSet {
  const ranges = marks.flatMap(mark => {
    const kind = `draft-mark draft-mark-${mark.severity}${stale ? " draft-mark-stale" : ""}`;
    const line = doc.lineAt(mark.from);
    const lineDeco = Decoration.line({ class: `draft-line-${mark.severity}${stale ? " draft-mark-stale" : ""}` }).range(line.from);
    return mark.to > mark.from ? [lineDeco, Decoration.mark({ class: kind, attributes: { "aria-description": mark.message } }).range(mark.from, mark.to)] : [lineDeco];
  });
  return Decoration.set(ranges, true);
}
export const draftMarks = StateField.define<MarkState>({
  create: () => ({ marks: [], stale: false, decorations: Decoration.none }),
  update(value, tr) {
    for (const effect of tr.effects) {
      if (effect.is(setDraftMarks)) {
        const marks = effect.value.marks.map(m => clampMark(tr.state.doc, m));
        return { marks, stale: false, decorations: decorate(tr.state.doc, marks, false) };
      }
    }
    if (!tr.docChanged) return value;
    const marks = value.marks.map(m => {
      const from = tr.changes.mapPos(m.from, 1);
      return { ...m, from, to: Math.max(from, tr.changes.mapPos(m.to, -1)) };
    });
    return { marks, stale: true, decorations: decorate(tr.state.doc, marks, true) };
  },
  provide: field => EditorView.decorations.from(field, value => value.decorations),
});

class SeverityMarker extends GutterMarker {
  constructor(readonly severity: DraftSeverity, readonly stale: boolean) { super(); }
  override eq(other: SeverityMarker) { return other.severity === this.severity && other.stale === this.stale; }
  override toDOM() {
    const span = document.createElement("span");
    span.className = `draft-gutter draft-gutter-${this.severity}${this.stale ? " draft-mark-stale" : ""}`;
    span.textContent = this.severity === "error" ? (this.stale ? "○" : "●") : (this.stale ? "△" : "▲");
    return span;
  }
}
const severityGutter = gutter({
  class: "draft-severity-gutter",
  markers: view => {
    const { marks, stale } = view.state.field(draftMarks);
    const byLine = new Map<number, DraftSeverity>();
    for (const mark of marks) {
      const line = view.state.doc.lineAt(mark.from).from;
      if (byLine.get(line) !== "error") byLine.set(line, mark.severity);
    }
    return RangeSet.of([...byLine].sort((a, b) => a[0] - b[0]).map(([at, severity]) => new SeverityMarker(severity, stale).range(at)));
  },
});

/** Where diagnostic `index` sits in the current text: its own offsets, carried through every edit since. */
export function markRange(state: EditorState, index: number): { from: number; to: number } | undefined {
  const mark = state.field(draftMarks, false)?.marks[index];
  return mark ? { from: mark.from, to: mark.to } : undefined;
}

/* ---------- assembly ---------- */

export interface DraftWiring {
  onChange: (text: string) => void;
  onSave: () => void;
  onCheck: () => void;
  onCaret: (offset: number) => void;
  typeNames: () => readonly string[];
}

const dress = EditorView.theme({
  "&": { backgroundColor: "transparent", color: "var(--mono-ink)" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--type-mono-family)", fontSize: "var(--type-mono-size)", lineHeight: "var(--type-mono-leading)", overflow: "auto", maxHeight: "52vh" },
  ".cm-content": { padding: "12px 0", caretColor: "var(--mono-ref)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--mono-ref)" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection": { backgroundColor: "var(--sel)" },
  ".cm-gutters": { backgroundColor: "transparent", border: "none", color: "var(--mono-faint)" },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "transparent" },
});

export function draftExtensions(wiring: DraftWiring): Extension[] {
  return [
    // Only `\n` breaks a line, so a saved `\r` survives a round trip and offsets stay the backend's.
    EditorState.lineSeparator.of("\n"),
    jsonSyntax,
    lineNumbers(),
    severityGutter,
    draftMarks,
    history(),
    indentUnit.of(UNIT),
    draftIndentation,
    outdenting,
    autocompletion({ override: [draftCompletion(wiring.typeNames)], icons: false }),
    keymap.of([
      { key: "Mod-s", run: view => (closeCompletion(view), wiring.onSave(), true) },
      { key: "Mod-Enter", run: view => (closeCompletion(view), wiring.onCheck(), true) },
      { key: "Shift-Alt-f", run: reindentDraft },
      { key: "Mod-Space", run: startCompletion },
    ]),
    // Tab indents and never accepts a completion: completionKeymap binds no Tab.
    keymap.of([...completionKeymap, ...defaultKeymap, ...historyKeymap, indentWithTab]),
    EditorView.updateListener.of(update => {
      if (update.docChanged) wiring.onChange(update.state.doc.toString());
      if (update.selectionSet || update.docChanged) wiring.onCaret(update.state.selection.main.head);
    }),
    EditorView.contentAttributes.of({ "aria-label": "Draft JSON source" }),
    dress,
  ];
}

export function makeDraftEditor(parent: HTMLElement, text: string, wiring: DraftWiring): EditorView {
  return new EditorView({ parent, state: EditorState.create({ doc: text, extensions: draftExtensions(wiring) }) });
}
