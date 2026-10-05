/**
 * The `types`/`env` editor, as CodeMirror is told about it — `calc-editor.ts`'s twin.
 *
 * Decides nothing: every colour, candidate and indent comes from `yaml-mode.ts`/`yaml-complete.ts`,
 * which read `yaml-highlight.ts` against the workspace's own type vocabulary. This file is only the
 * wiring, reached by dynamic import so a session that never opens one of these buffers never loads
 * CodeMirror for it.
 */
import { autocompletion, closeCompletion, completionKeymap, type Completion, type CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { indentService, indentUnit } from "@codemirror/language";
import { tooltips } from "@codemirror/view";
import { linter, type Diagnostic } from "@codemirror/lint";
import { Compartment, Facet, EditorSelection, EditorState, type Extension } from "@codemirror/state";
import {
  Decoration, EditorView, GutterMarker, ViewPlugin, gutter, keymap, lineNumbers,
  type DecorationSet, type ViewUpdate,
} from "@codemirror/view";
import { completionsAt } from "./yaml-complete";
import { CLOSES, INDENT, indentAt, pairAt, readMode, newlineAt, type YamlMode } from "./yaml-mode";
import type { TypeVocabulary } from "./yaml-highlight";
import { lineText } from "./MonoLine";
import { yamlLocation, type YamlContext, type YamlSchema } from "./yaml-schema";
import { editorSelection } from "./editor-selection";

export interface YamlEditorWiring {
  readonly vocabulary: TypeVocabulary;
  readonly context?: YamlContext;
  readonly schema?: YamlSchema;
  /** `⌘S` or the screen's own Save chip — both call this with the current text. */
  readonly onSave: () => void;
  /** Validate and plan/load the current buffer without saving it. */
  readonly onRun?: () => void;
  /** `esc` — back to the session, the draft kept. */
  readonly onLeave: () => void;
}

const schemaSlot = new Compartment();
const schemaFacet = Facet.define<YamlSchema | undefined, YamlSchema | undefined>({ combine: values => values[0] });
const schemaChanged = (update: ViewUpdate) => update.startState.facet(schemaFacet) !== update.state.facet(schemaFacet);

/** Reconfigure metadata only: document, selection, undo and dirty state stay intact. */
export function updateYamlSchema(view: EditorView, schema: YamlSchema): void {
  view.dispatch({ effects: schemaSlot.reconfigure(schemaFacet.of(schema)) });
}

class Mistaken extends GutterMarker {
  override toDOM(): Node {
    const mark = document.createElement("span");
    mark.className = "mono-bad";
    mark.textContent = "●";
    return mark;
  }
}
const MISTAKEN = new Mistaken();

type ModeReader = (state: EditorState) => YamlMode;
/** One tokenization per document/schema/vocabulary, shared by paint, lint and gutter markers. */
export function yamlModeReader(vocabulary: TypeVocabulary, context: YamlContext): ModeReader {
  let previous: { doc: EditorState["doc"]; schema?: YamlSchema; names: string; mode: YamlMode } | undefined;
  return state => {
    const schema = state.facet(schemaFacet);
    const names = vocabulary.names.join("\0");
    if (!previous || previous.doc !== state.doc || previous.schema !== schema || previous.names !== names) {
      previous = { doc: state.doc, schema, names, mode: readMode(state.doc.toString(), vocabulary, context, schema) };
    }
    return previous.mode;
  };
}

function painted(read: ModeReader): Extension {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) { this.decorations = this.paint(view); }
      update(update: ViewUpdate) {
        if (update.docChanged || update.selectionSet || schemaChanged(update)) this.decorations = this.paint(update.view);
      }
      paint(view: EditorView): DecorationSet {
        const source = view.state.doc.toString();
        const mode = read(view.state);
        const marks = mode.spans
          .filter((span) => span.role !== undefined && span.to > span.from)
          .map((span) => Decoration.mark({ class: span.role }).range(span.from, span.to));
        const pair = pairAt(source, view.state.selection.main.head, mode);
        if (pair) {
          marks.push(Decoration.mark({ class: "mono-ink-strong" }).range(pair.open, pair.open + 1));
          marks.push(Decoration.mark({ class: "mono-ink-strong" }).range(pair.close, pair.close + 1));
        }
        return Decoration.set(marks.sort((a, b) => a.from - b.from || a.to - b.to), true);
      }
    },
    { decorations: (plugin) => plugin.decorations },
  );
}

/** Every mistake `yaml-mode.ts` finds, published as CodeMirror diagnostics — never a line under the word. */
function refusals(read: ModeReader): Extension {
  return linter(
    (view): Diagnostic[] =>
      read(view.state).diagnostics.map((wrong) => ({
        from: wrong.from,
        to: wrong.to,
        severity: "error",
        message: `${wrong.message} — ${lineText(wrong.hint)}`,
        markClass: "calc-refused",
      })),
    { delay: 120, needsRefresh: schemaChanged, tooltipFilter: () => [] },
  );
}

function marks(read: ModeReader): Extension {
  return gutter({
    class: "cm-mistakes",
    lineMarker: (view, block) => {
      const mode = read(view.state);
      const line = view.state.doc.lineAt(block.from).number - 1;
      return mode.marked.includes(line) ? MISTAKEN : null;
    },
    lineMarkerChange: (update) => update.docChanged || schemaChanged(update),
    renderEmptyElements: true,
  });
}

/** Exported to exercise the exact source installed in CodeMirror. */
export function yamlCompletionSource(wiring: YamlEditorWiring) {
  return (context: CompletionContext): CompletionResult | null => {
    const whole = context.state.doc.toString();
    const schema = context.state.facet(schemaFacet) ?? wiring.schema;
    const place = yamlLocation(whole, context.pos, wiring.context ?? "types", schema);
    const offered = completionsAt(whole, context.pos, wiring.vocabulary, 50, wiring.context ?? "types", schema);
    if (offered.length === 0) return null;
    return {
      from: place.from,
      to: place.to,
      options: offered.map<Completion>((candidate) => ({
        label: candidate.text,
        displayLabel: candidate.label,
        detail: candidate.detail,
        type: candidate.kind,
        apply: candidate.kind === "property" && !/^[ \t]*:/.test(whole.slice(place.to)) ? `${candidate.text}: ` : candidate.text,
      })),
    };
  };
}
function offering(wiring: YamlEditorWiring): Extension {
  const source = yamlCompletionSource(wiring);
  return [
    autocompletion({ override: [source], icons: false, tooltipClass: () => "calc-completion surface-terminal" }),
    tooltips({
      tooltipSpace: (view) => {
        const card = view.dom.closest(".edit-card");
        const room = card?.getBoundingClientRect();
        return room
          ? { top: room.top, left: room.left, bottom: room.bottom, right: room.right }
          : { top: 0, left: 0, bottom: innerHeight, right: innerWidth };
      },
    }),
  ];
}

/** A closing bracket typed at the head of a line takes that line back out — `calc-editor.ts`'s same rule. */
function dedenting(vocabulary: TypeVocabulary, context: YamlContext): Extension {
  return EditorView.inputHandler.of((view, from, to, text) => {
    if (text.length !== 1 || !CLOSES.includes(text)) return false;
    const line = view.state.doc.lineAt(from);
    if (view.state.doc.sliceString(line.from, from).trim() !== "") return false;
    const whole = view.state.doc.toString();
    const next = whole.slice(0, from) + text + whole.slice(to);
    const want = indentAt(next, line.from, readMode(next, vocabulary, context, view.state.facet(schemaFacet)), context, view.state.facet(schemaFacet));
    view.dispatch({
      changes: { from: line.from, to, insert: `${" ".repeat(want)}${text}` },
      selection: { anchor: line.from + want + 1 },
      userEvent: "input.type",
    });
    return true;
  });
}

export function yamlNewline(context: YamlContext): (view: EditorView) => boolean {
  return view => {
    if (view.state.readOnly) return false;
    view.dispatch(view.state.changeByRange(range => {
      const edit = newlineAt(view.state.doc.toString(), range.from, context, view.state.facet(schemaFacet));
      return { changes: { from: edit.from, to: range.to, insert: edit.insert },
        range: EditorSelection.cursor(edit.from + edit.insert.length) };
    }), { scrollIntoView: true, userEvent: "input" });
    return true;
  };
}
function keys(wiring: YamlEditorWiring): Extension {
  return keymap.of([
    { key: "Mod-s", run: (view) => (closeCompletion(view), wiring.onSave(), true) },
    { key: "Mod-r", run: (view) => (closeCompletion(view), wiring.onRun?.(), true), preventDefault: true },
    { key: "Shift-Enter", run: yamlNewline(wiring.context ?? "types") },
    { key: "Escape", run: view => closeCompletion(view) || (wiring.onLeave(), true) },
  ]);
}

const dress = EditorView.theme({
  "&": { backgroundColor: "transparent", color: "var(--mono-ink)" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": {
    fontFamily: "var(--type-mono-family)",
    fontSize: "var(--type-mono-size)",
    lineHeight: "var(--type-mono-leading)",
    letterSpacing: "var(--type-mono-tracking)",
  },
  ".cm-content": { padding: "0", caretColor: "var(--mono-ref)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--mono-ref)" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection": {
    backgroundColor: "var(--sel)",
  },
  ".cm-gutters": { backgroundColor: "transparent", border: "none", color: "var(--mono-faint)" },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "transparent" },
  ".calc-refused": { textDecoration: "none" },
});

export function yamlExtensions(wiring: YamlEditorWiring, onChange: (source: string) => void): Extension[] {
  const { vocabulary, context = "types" } = wiring;
  const read = yamlModeReader(vocabulary, context);
  return [
    schemaSlot.of(schemaFacet.of(wiring.schema)),
    lineNumbers(),
    editorSelection(),
    marks(read),
    history(),
    indentUnit.of(" ".repeat(INDENT)),
    indentService.of((indentContext, at) => {
      const source = indentContext.state.doc.toString();
      return indentAt(source, at, read(indentContext.state), context, indentContext.state.facet(schemaFacet));
    }),
    dedenting(vocabulary, context),
    painted(read),
    refusals(read),
    offering(wiring),
    keys(wiring),
    keymap.of([...completionKeymap, { key: "Enter", run: yamlNewline(wiring.context ?? "types") }, ...historyKeymap, indentWithTab, ...defaultKeymap]),
    EditorView.lineWrapping,
    dress,
    EditorView.updateListener.of((update) => {
      if (update.docChanged) onChange(update.state.doc.toString());
    }),
  ];
}

export function makeYamlEditor(
  parent: HTMLElement,
  source: string,
  wiring: YamlEditorWiring,
  onChange: (source: string) => void,
  takeFocus = true,
): EditorView {
  const view = new EditorView({
    parent,
    state: EditorState.create({ doc: source, extensions: yamlExtensions(wiring, onChange) }),
  });
  if (takeFocus) view.focus();
  view.dispatch({ selection: { anchor: view.state.doc.length } });
  return view;
}

export { EditorView };
