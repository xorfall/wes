/**
 * A `:calc` program, drawn: a gutter, the highlighted lines beside it, and the mistake rows.
 *
 * Drawing only, and only for the design gallery. What somebody actually types into is `/edit`,
 * which is CodeMirror over the same reading of the source (`calc-mode.ts`) — this is how the
 * gallery shows a program in both palettes without mounting an editor, which is what a gallery is
 * for. The two cannot disagree about a colour, because the colours come from the one tokeniser.
 */
import { useMemo, type Ref } from "react";
import { highlightCalc, mistakeRows, type Mistake } from "./calc-highlight";
import type { Language } from "./language";
import { MonoLine, type Segment } from "./MonoLine";
import "./surface.css";
import "./editor.css";

/** Past twelve lines the editor scrolls rather than eating the scrollback. */
export const MAX_LINES = 12;

/**
 * What the highlighter made of the source: the rows to draw and the tokens it refuses.
 *
 * Without a package there is nothing to highlight against — the prompt has not grown yet and the
 * caller draws its one line instead — so an absent language answers with an empty drawing rather
 * than making every caller guard the call.
 */
export function useHighlight(source: string, language: Language | undefined): Drawn {
  return useMemo(
    () => (language ? highlightCalc(source, language) : { lines: [], mistakes: [] }),
    [source, language],
  );
}

/** The rows to draw and the tokens the package refuses. */
export interface Drawn {
  readonly lines: readonly (readonly Segment[])[];
  readonly mistakes: readonly Mistake[];
}

/** Right-aligned in three columns, as the surface's gutter is. */
export function gutter(number: number | undefined): string {
  return number === undefined ? "   " : String(number + 1).padStart(3, " ");
}

/** The numbers beside the lines. Scrolled by the field, never by itself. */
export function EditorGutter({ count, innerRef }: { readonly count: number; readonly innerRef?: Ref<HTMLDivElement> }) {
  return (
    <div className="editor-gutter" ref={innerRef} aria-hidden="true">
      {Array.from({ length: Math.min(count, MAX_LINES) }, (_, number) => (
        <MonoLine key={number} segments={[{ text: gutter(number), role: "mono-faint" }]} className="editor-gutter-line" />
      ))}
    </div>
  );
}

/** The highlighted source. This is the height the field takes, so the two never drift apart. */
export function EditorLines({ lines, innerRef }: { readonly lines: readonly (readonly Segment[])[]; readonly innerRef?: Ref<HTMLDivElement> }) {
  return (
    <div className="editor-lines" ref={innerRef} aria-hidden="true">
      {lines.slice(0, MAX_LINES).map((line, number) => (
        <MonoLine key={number} segments={line} className="editor-line" />
      ))}
    </div>
  );
}

/**
 * What the language has instead, under the block, pointing at the token's column.
 *
 * Never a red line under the expression: a line says "somewhere in here", and the package knows
 * exactly which token it was.
 */
export function EditorMistakes({ mistakes }: { readonly mistakes: readonly Mistake[] }) {
  if (mistakes.length === 0) return null;
  return (
    <div className="editor-mistakes">
      {mistakes.map((mistake) => {
        const rows = mistakeRows(mistake);
        return (
          <div className="editor-mistake" key={`${mistake.line}:${mistake.from}`}>
            <MonoLine segments={rows.carets} className="editor-carets" />
            <MonoLine segments={rows.said} className="editor-said" />
          </div>
        );
      })}
    </div>
  );
}

export interface EditorProps {
  readonly source: string;
  readonly language: Language;
}

/** The whole block with nothing typing into it, for the gallery. The prompt assembles its own. */
export function Editor({ source, language }: EditorProps) {
  const { lines, mistakes } = useHighlight(source, language);
  return (
    <div className="editor surface-sunk">
      <div className="editor-body">
        <EditorGutter count={lines.length} />
        <div className="editor-column">
          <EditorLines lines={lines} />
          <EditorMistakes mistakes={mistakes} />
        </div>
      </div>
    </div>
  );
}
