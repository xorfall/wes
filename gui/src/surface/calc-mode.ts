/**
 * The `:calc` editing mode, generated from the language package the engine serves.
 *
 * The editor colours a word as a keyword because the package says
 * it is one, offers an operation because the package has it, and refuses `!==` because the package
 * does not. Nothing here carries a list of its own.
 *
 * It is the drawn highlighter's own reading, said in the shapes an editor needs — ranges rather
 * than rows, diagnostics rather than caret lines, a bracket's partner rather than a colour. There
 * is one tokeniser (`calc-highlight.ts`) and this asks it; a second one would eventually disagree,
 * and the disagreement would be a word that is one colour at the prompt and another in the editor.
 *
 * Pure, and free of the editor library on purpose: what is decided is testable without mounting
 * anything, and the library is left holding only the part that is genuinely about a view.
 */
import { highlightCalc, type Mistake, type Span } from "./calc-highlight";
import type { Language } from "./language";
import type { MonoRole, Segment } from "./MonoLine";
import { CLOSES, gutterMark, lineAt, OPENS, pairAt as pairAtSpans, type Pair } from "./source-mode";

export { CLOSES, gutterMark, lineAt, OPENS };
export type { Pair };

/** Columns used to indent one level of nesting. */
export const INDENT = 2;

export interface CalcDiagnostic {
  readonly from: number;
  readonly to: number;
  /** Counted from zero, the way the gutter counts before it adds one. */
  readonly line: number;
  readonly column: number;
  /** The offending token itself, which is how wide the caret row under it is. */
  readonly text: string;
  /** What is wrong, said in one line. */
  readonly message: string;
  /** What the language has instead, in the roles the mistake row draws it in. */
  readonly hint: readonly Segment[];
}

export interface CalcMode {
  readonly spans: readonly Span[];
  readonly diagnostics: readonly CalcDiagnostic[];
  /** Which lines carry a mistake, so the gutter can mark them. Counted from zero. */
  readonly marked: readonly number[];
}

/** Everything the editor needs to draw one source, read once. */
export function readMode(source: string, language: Language): CalcMode {
  const { mistakes, spans } = highlightCalc(source, language);
  const diagnostics = mistakes.map(asDiagnostic);
  return { spans, diagnostics, marked: [...new Set(diagnostics.map((it) => it.line))].sort((a, b) => a - b) };
}

function asDiagnostic(mistake: Mistake): CalcDiagnostic {
  return {
    from: mistake.from,
    to: mistake.to,
    line: mistake.line,
    column: mistake.column,
    text: mistake.text,
    message: mistake.said,
    hint: mistake.hint,
  };
}

/**
 * The bracket the caret is beside, and its partner.
 *
 * `source-mode.ts` does the finding, from the spans alone; this hands it the ones a calc mode has.
 */
export function pairAt(source: string, caret: number, mode: CalcMode): Pair | undefined {
  return pairAtSpans(source, caret, mode.spans);
}

/**
 * How far a line should be indented, counted in spaces.
 *
 * What is open decides it: every bracket still unclosed before this line is one level, and a line
 * that begins by closing one is dedented by that level rather than by a guess. Strings and comments
 * are skipped for the same reason as above — a `{` inside a string opens nothing.
 */
export function indentAt(source: string, at: number, mode: CalcMode): number {
  const start = source.lastIndexOf("\n", Math.max(0, at - 1)) + 1;
  let depth = 0;
  for (const span of mode.spans) {
    if (span.from >= start) break;
    if (span.to - span.from !== 1) continue;
    const character = source[span.from]!;
    if (OPENS.includes(character)) depth += 1;
    else if (CLOSES.includes(character)) depth = Math.max(0, depth - 1);
  }
  const rest = source.slice(start);
  const first = /^[ \t]*(\S)/.exec(rest)?.[1];
  if (first !== undefined && CLOSES.includes(first)) depth = Math.max(0, depth - 1);
  return depth * INDENT;
}

/**
 * The word being typed at the caret, which is what completion is about.
 *
 * A dot ends it: `$orders.fil` is completing `fil` on `$orders`, not completing `$orders.fil`. A
 * leading `$` is part of it, because it is what says a name is wanted rather than an operation.
 * The prompt and the editor both ask this, so both complete the same word from the same source.
 */
export function wordAt(source: string, caret: number): { readonly text: string; readonly from: number } {
  const before = source.slice(0, caret);
  const match = /(\$?[\p{L}\p{N}_]*)$/u.exec(before);
  const text = match?.[1] ?? "";
  return { text, from: caret - text.length };
}

/**
 * Every role the mode can give a span, which is every role the editor's theme has to answer for.
 *
 * Listed here rather than discovered from a document, because a theme that only styles what one
 * program happened to contain is a theme with holes in it.
 */
export const MODE_ROLES: readonly MonoRole[] = [
  "mono-ink", "mono-dim", "mono-faint", "mono-meta", "mono-provider",
  "mono-param", "mono-literal", "mono-ref", "mono-bad", "mono-ink-strong",
];
