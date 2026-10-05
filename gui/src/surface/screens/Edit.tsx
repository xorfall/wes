/**
 * `/edit` — the prompt's program, with room and an answer beside it.
 *
 * A dedicated editor complements the plain multiline prompt with a gutter, a mistake that names its
 * token, a completion list that does not cover the scrollback, and the result of the last run.
 *
 * The output pane and the scrollback's cell are two views of one node, never two results. `⌘⏎` runs
 * the text and binds the editor to what it made; `⌘R` runs that same node again through the same
 * path as `r` on its cell, guard and all. So what the pane shows is what the cell shows, because it
 * is the cell's node — and a person who scrolls back to it finds it already changed.
 *
 * CodeMirror is loaded on demand here or by the inline read-only source viewer: the session's
 * bundle must not carry an editor nobody has opened.
 */
import { useEffect, useRef, useState, type ReactNode } from "react";
import { mistakeRows } from "./../calc-highlight";
import { readMode } from "./../calc-mode";
import { boundName, editHead, editKeys, editingTop, outputHead, type Bound } from "./../edit-model";
import type { Language } from "./../language";
import { MonoLine, type Segment } from "./../MonoLine";
import { Screen } from "./../Screen";
import "./../surface.css";
import "./../editor.css";

export interface EditProps {
  readonly autoFocus?: boolean;
  readonly top: readonly Segment[];
  readonly source: string;
  readonly language: Language;
  /** The workspace's names, so `$` offers what there is. */
  readonly names: readonly string[];
  /** The node the last run made, when there has been one. */
  readonly bound?: Bound;
  /** The bound node's cell, drawn in minimal chrome — the same Cell the scrollback draws. */
  readonly output?: ReactNode;
  readonly context: readonly Segment[];
  readonly onChange: (source: string) => void;
  readonly onRun: (source: string) => void;
  readonly onRunAgain: () => void;
  readonly onClose: () => void;
  readonly chrome?: "full" | "pane";
}

export function EditScreen({
  top, source, language, names, bound, output, context, onChange, onRun, onRunAgain, onClose, chrome = "full", autoFocus = true,
}: EditProps) {
  const card = useRef<HTMLDivElement>(null);
  /** What is typed, so the mistake row under the block follows the caret rather than the last run. */
  const [written, setWritten] = useState(source);
  const latest = useRef({ source: written, onChange, onRun, onRunAgain, onClose, autoFocus });
  latest.current = { source: written, onChange, onRun, onRunAgain, onClose, autoFocus };

  /*
   * One editor, made once, fed by callbacks that read the latest props through a ref.
   *
   * Rebuilding it on every render would throw the caret, the undo history and the selection away on
   * each keystroke, which is every editor bug at once. So the view is made for this mount and the
   * wiring reads what is current rather than what was captured.
   */
  useEffect(() => {
    const parent = card.current;
    if (!parent) return;
    let view: { destroy: () => void } | undefined;
    let dead = false;
    void import("./../calc-editor").then(({ makeEditor }) => {
      if (dead || !card.current) return;
      view = makeEditor(card.current, latest.current.source, {
        language,
        names,
        onChange: (text) => { setWritten(text); latest.current.onChange(text); },
        onRun: () => latest.current.onRun(latest.current.source),
        onRunAgain: () => latest.current.onRunAgain(),
        onLeave: () => latest.current.onClose(),
      }, latest.current.autoFocus);
    });
    return () => { dead = true; view?.destroy(); };
    // The editor is made for this mount: the language and the names it was made with are enough.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [language]);

  /*
   * The mistake rows are drawn beside the editor, not inside it, so they have to be told where the
   * code starts. The gutter's width is measured rather than assumed: it changes with the face, with
   * the density, and with how many digits the line count has reached.
   */
  useEffect(() => {
    const parent = card.current;
    if (!parent) return;
    const measure = () => {
      const gutters = parent.querySelector(".cm-gutters");
      const width = gutters instanceof HTMLElement ? gutters.getBoundingClientRect().width : 0;
      parent.parentElement?.style.setProperty("--edit-gutter", `${Math.round(width)}px`);
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const watching = new ResizeObserver(measure);
    watching.observe(parent);
    return () => watching.disconnect();
  }, [written]);

  const mode = readMode(written, language);

  return (
    <Screen
      name="/edit"
      top={editingTop(top, bound)}
      chrome={chrome}
      subject={editHead(bound)}
      onClose={onClose}
      footer={editKeys()}
    >
      <div className="edit-body">
        <div className="edit-card surface-sunk">
          <div className="edit-code" ref={card} />
          {/* Under the block, in the code's own column: the package knows which token it was. */}
          {mode.diagnostics.length > 0 && (
            <div className="editor-mistakes">
              {mode.diagnostics.map((wrong) => {
                const rows = mistakeRows({ ...wrong, said: wrong.message });
                return (
                  <div className="editor-mistake" key={`${wrong.line}:${wrong.from}`}>
                    <MonoLine segments={rows.carets} className="editor-carets" />
                    <MonoLine segments={rows.said} className="editor-said" />
                  </div>
                );
              })}
            </div>
          )}
        </div>

        <MonoLine segments={outputHead(bound)} className="edit-output-head" />
        <div className="edit-output">{output}</div>
      </div>
      <MonoLine segments={context} className="edit-context" />
    </Screen>
  );
}

/** What the head calls the bound result, for whoever needs it outside the screen. */
export { boundName };
