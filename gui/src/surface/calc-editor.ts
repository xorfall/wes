/**
 * The `:calc` editor, as CodeMirror is told about it.
 *
 * This is the only file in the client that knows CodeMirror exists, and it is reached by dynamic
 * import alone: `/edit` or an inline source viewer fetches it when opened.
 *
 * It decides nothing. Every colour, every candidate, every refusal and every indent comes from
 * `calc-mode.ts`, which reads the language package — so what this file holds is
 * the wiring and the view, and a change to the language changes neither.
 *
 * The roles are the surface's own CSS classes rather than a CodeMirror highlight style. A span is
 * decorated with `mono-provider` and `roles.css` colours it, exactly as it colours the same word in
 * the scrollback — one vocabulary of colour for the whole surface, and no second theme to keep in
 * step with the first.
 */
import { autocompletion, closeCompletion, completionKeymap, completionStatus, type Completion, type CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab, insertNewlineAndIndent, toggleComment } from "@codemirror/commands";
import { indentService, indentUnit } from "@codemirror/language";
import { tooltips } from "@codemirror/view";
import { linter, type Diagnostic } from "@codemirror/lint";
import { EditorState, type Extension } from "@codemirror/state";
import {
  Decoration, EditorView, GutterMarker, ViewPlugin, gutter, keymap, lineNumbers,
  type DecorationSet, type ViewUpdate,
} from "@codemirror/view";
import { completions, completionSource } from "./calc-complete";
import { CLOSES, INDENT, indentAt, pairAt, readMode, wordAt } from "./calc-mode";
import type { Language } from "./language";
import { lineText } from "./MonoLine";
import { editorSelection } from "./editor-selection";

export interface EditorWiring {
  readonly language: Language;
  /** The workspace's names, so `$` offers what there is. The prompt's own list, same source. */
  readonly names: readonly string[];
  readonly onChange: (source: string) => void;
  /** `⌘⏎` — submit the whole text and bind the editor to what it makes. */
  readonly onRun: () => void;
  /** `⌘R` — run the bound node again, through the same path as `r` on its cell. */
  readonly onRunAgain: () => void;
  /** `esc` — back to the session, the draft kept. */
  readonly onLeave: () => void;
}

/** The mark a line with a mistake carries in the gutter: `●`, in the role that says it is wrong. */
class Mistaken extends GutterMarker {
  override toDOM(): Node {
    const mark = document.createElement("span");
    mark.className = "mono-bad";
    mark.textContent = "●";
    return mark;
  }
}
const MISTAKEN = new Mistaken();

/**
 * The colours, and the bracket the caret is beside.
 *
 * Rebuilt whenever the document or the selection changes, which is what a mode is: there is no
 * incremental parse here because the tokeniser reads the whole source anyway, and a `:calc`
 * program is twelve lines rather than twelve thousand.
 */
function painted(language: Language): Extension {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) { this.decorations = this.paint(view); }
      update(update: ViewUpdate) {
        if (update.docChanged || update.selectionSet) this.decorations = this.paint(update.view);
      }
      paint(view: EditorView): DecorationSet {
        const source = view.state.doc.toString();
        const mode = readMode(source, language);
        const marks = mode.spans
          .filter((span) => span.role !== undefined && span.to > span.from)
          .map((span) => Decoration.mark({ class: span.role }).range(span.from, span.to));
        const pair = pairAt(source, view.state.selection.main.head, mode);
        if (pair) {
          // The matched pair is the one thing the roles do not already say, so it says it itself.
          marks.push(Decoration.mark({ class: "mono-ink-strong" }).range(pair.open, pair.open + 1));
          marks.push(Decoration.mark({ class: "mono-ink-strong" }).range(pair.close, pair.close + 1));
        }
        return Decoration.set(marks.sort((a, b) => a.from - b.from || a.to - b.to), true);
      }
    },
    { decorations: (plugin) => plugin.decorations },
  );
}

/**
 * The mistakes, as CodeMirror's own diagnostics.
 *
 * Published so the editor knows what is wrong — the gutter's mark is driven from this, and so is
 * anything that later wants to walk them. Diagnostics appear in the gutter and the row under
 * the block rather than as an underline across the expression.
 */
function refusals(language: Language): Extension {
  return linter(
    (view): Diagnostic[] =>
      readMode(view.state.doc.toString(), language).diagnostics.map((wrong) => ({
        from: wrong.from,
        to: wrong.to,
        severity: "error",
        message: `${wrong.message} — ${lineText(wrong.hint)}`,
        markClass: "calc-refused",
      })),
    { delay: 120, tooltipFilter: () => [] },
  );
}

function marks(language: Language): Extension {
  return gutter({
    class: "cm-mistakes",
    lineMarker: (view, block) => {
      const mode = readMode(view.state.doc.toString(), language);
      const line = view.state.doc.lineAt(block.from).number - 1;
      return mode.marked.includes(line) ? MISTAKEN : null;
    },
    lineMarkerChange: (update) => update.docChanged,
    renderEmptyElements: true,
  });
}

/**
 * What the editor offers, which is what the prompt offers.
 *
 * One source (`calc-complete`), asked here and at the prompt, so an operation the package gained is
 * offered in both places and an operation it never had is offered in neither. The popup flips above
 * the caret's line when there is no room below it, which is CodeMirror's own behaviour and the same
 * as the prompt's list opening upward: a list somewhere the caret cannot see is not an offer.
 */
function offering(language: Language, names: readonly string[]): Extension {
  const source = (context: CompletionContext): CompletionResult | null => {
    // The same word the prompt would complete: a dot ends it, and a leading `$` belongs to it.
    const word = wordAt(context.state.doc.toString(), context.pos);
    if (word.text === "" && !context.explicit) return null;
    const offered = completions(word.text, language, names);
    if (offered.length === 0) return null;
    return {
      from: word.from,
      options: offered.map<Completion>((candidate) => ({
        label: candidate.text,
        displayLabel: candidate.label,
        detail: candidate.detail,
        type: candidate.kind,
      })),
    };
  };
  return [
    autocompletion({ override: [source], icons: false, tooltipClass: () => "calc-completion surface-terminal" }),
    /*
     * The list stays inside the card it belongs to.
     *
     * CodeMirror keeps a tooltip inside the window by default, and the window is not the boundary
     * that matters: a list that spills past the card covers the diagnostic under it and the output
     * pane under that. Handing it the card's own rectangle makes it fit there — and flip above the
     * caret's line when below will not do — which is what the prompt's list already does.
     */
    tooltips({
      tooltipSpace: (view) => {
        const card = view.dom.closest(".edit-card");
        const room = card?.getBoundingClientRect();
        return room
          ? { top: room.top, left: room.left, bottom: room.bottom, right: room.right }
          : { top: 0, left: 0, bottom: innerHeight, right: innerWidth };
      },
    }),
    saying(language),
  ];
}

/**
 * `from the workspace's calc/default.yaml — no JS globals`, under the candidates.
 *
 * The popup is CodeMirror's and has no footer to fill, so the line is put there when the popup
 * appears. Somebody reading a list of operations they have never seen
 * should be told where the list came from — the answer to "is this JavaScript?" is on the list.
 */
function saying(language: Language): Extension {
  const said = lineText(completionSource(language));
  return EditorView.updateListener.of((update) => {
    const popup = update.view.dom.querySelector(".calc-completion");
    if (!popup || popup.querySelector(".calc-completion-source")) return;
    const line = document.createElement("div");
    line.className = "calc-completion-source mono-faint";
    line.textContent = said;
    popup.append(line);
  });
}

/**
 * A closing bracket typed at the head of a line takes that line back out.
 *
 * The indent service answers when a line is *begun*, and a line begun inside a block is indented —
 * correctly, because at that moment nothing has closed it. The dedent can only be known when the
 * bracket arrives, which is here: the line is re-laid with what it should now have, and the caret
 * follows the bracket rather than the whitespace.
 */
function dedenting(language: Language): Extension {
  return EditorView.inputHandler.of((view, from, to, text) => {
    if (text.length !== 1 || !CLOSES.includes(text)) return false;
    const line = view.state.doc.lineAt(from);
    // Only a bracket that begins its line moves it; one at the end of an expression is just typing.
    if (view.state.doc.sliceString(line.from, from).trim() !== "") return false;
    const whole = view.state.doc.toString();
    const next = whole.slice(0, from) + text + whole.slice(to);
    const want = indentAt(next, line.from, readMode(next, language));
    view.dispatch({
      changes: { from: line.from, to, insert: `${" ".repeat(want)}${text}` },
      selection: { anchor: line.from + want + 1 },
      userEvent: "input.type",
    });
    return true;
  });
}

/** Editor-specific shortcuts, ahead of CodeMirror's own defaults. */
function keys(wiring: EditorWiring): Extension {
  return keymap.of([
    // Running is done with what is written, not with what was being offered, so the list shuts.
    { key: "Mod-Enter", run: (view) => (closeCompletion(view), wiring.onRun(), true) },
    { key: "Mod-r", run: (view) => (closeCompletion(view), wiring.onRunAgain(), true) },
    { key: "Shift-Enter", run: insertNewlineAndIndent },
    { key: "Mod-/", run: toggleComment },
    /*
     * `esc` closes the list first and leaves second, which is what the prompt's own `esc` does.
     * Answering `false` hands the key on to the completion keymap rather than deciding for it.
     */
    {
      key: "Escape",
      run: (view) => (completionStatus(view.state) === null ? (wiring.onLeave(), true) : false),
    },
  ]);
}

/** How the surface dresses a CodeMirror: its own face, and nothing of CodeMirror's own chrome. */
const dress = EditorView.theme({
  "&": { backgroundColor: "transparent", color: "var(--mono-ink)" },
  "&.cm-focused": { outline: "none" },
  // The content's own focus ring would be clipped by the scroller to a bare left edge; the view's
  // box shows focus instead (`.cell-source-view:focus-within`, `.edit-code:focus-within`).
  ".cm-content:focus-visible, .cm-content:focus": { outline: "none" },
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
  // The refusal is said in a row under the block, never as a line under the expression.
  ".calc-refused": { textDecoration: "none" },
});

/** Everything the editor is, assembled from what the package said. */
export function calcExtensions(wiring: EditorWiring): Extension[] {
  const { language, names } = wiring;
  return [
    editorSelection(),
    lineNumbers(),
    marks(language),
    history(),
    /*
     * No auto-closing brackets. Bracket matching highlights the pair the caret is beside,
     * drawn strong — and a program usually arrives here already whole: handed over from the prompt,
     * reopened from a cell, or pasted. An editor that inserts a `}` into a program that has one
     * turns `:calc { … }` into a parse error, which is what it did before this comment existed.
     */
    indentUnit.of(" ".repeat(INDENT)),
    calcIndentation(language),
    dedenting(language),
    painted(language),
    refusals(language),
    offering(language, names),
    keys(wiring),
    keymap.of([...completionKeymap, ...historyKeymap, indentWithTab, ...defaultKeymap]),
    EditorView.lineWrapping,
    dress,
    EditorView.updateListener.of((update) => {
      if (update.docChanged) wiring.onChange(update.state.doc.toString());
    }),
  ];
}

/**
 * The indentation of the line at `at`, from the brackets open before it. `Enter` asks before the
 * break exists — CodeMirror simulates it at the caret — so the source is read as it will be once
 * the break is in, or the first new line under `{` would take the `{` line's own indent.
 */
export function calcIndentation(language: Language): Extension {
  return indentService.of((context, at) => {
    const doc = context.state.doc.toString();
    const broken = context.simulatedBreak;
    const source = broken !== null && broken === at ? `${doc.slice(0, at)}\n${doc.slice(at)}` : doc;
    const pos = broken !== null && broken === at ? at + 1 : at;
    return indentAt(source, pos, readMode(source, language));
  });
}

/**
 * Makes one, for the screen to put somewhere. Kept here so the view library has one doorway.
 *
 * It takes the focus as it arrives. `/edit` is a screen somebody summoned in order to type, and a
 * screen that opens with the caret somewhere else is a screen where the first thing typed is lost —
 * the same argument as the prompt's own `autoFocus`. `esc` still leaves, because the editor answers
 * that key itself rather than letting the screen behind it answer.
 */
export function makeEditor(parent: HTMLElement, source: string, wiring: EditorWiring, takeFocus = true): EditorView {
  const view = new EditorView({
    parent,
    state: EditorState.create({ doc: source, extensions: calcExtensions(wiring) }),
  });
  if (takeFocus) view.focus();
  // The caret goes to the end of what was handed over, which is where somebody stopped typing.
  view.dispatch({ selection: { anchor: view.state.doc.length } });
  return view;
}

/** A source viewer has selection/navigation, but no input, completion or execution wiring. */
export function sourceExtensions(language: Language): Extension[] {
  return [
    EditorState.readOnly.of(true),
    EditorView.editable.of(false),
    EditorState.changeFilter.of(() => false),
    EditorView.contentAttributes.of({
      tabindex: "0", role: "textbox", "aria-label": "Command source",
      "aria-readonly": "true", "aria-multiline": "true",
    }),
    lineNumbers(),
    painted(language),
    keymap.of(defaultKeymap),
    dress,
  ];
}

export function makeSourceViewer(parent: HTMLElement, source: string, language: Language): EditorView {
  return new EditorView({ parent, state: EditorState.create({ doc: source, extensions: sourceExtensions(language) }) });
}

export { EditorView };
export type { Completion };
