/**
 * What every editor mode is made of, independent of which language it reads.
 *
 * `calc-mode.ts` and `yaml-mode.ts` each read a different source into the same shapes — a run with a
 * role, a mistake with a caret and a hint, a bracket's partner, a gutter's line number — so the
 * editor wiring (`calc-editor.ts`) and the mistake row (`Edit.tsx`) ask these once and never learn
 * which language produced them. A mode that needs its own reading is a new tokeniser beside this
 * file, not a fork of it.
 */
import type { MonoRole, Segment } from "./MonoLine";

/** One run of a source, where it is and what role it takes. */
export interface Span {
  readonly from: number;
  readonly to: number;
  /** Absent for whitespace, which carries no role and is decorated by nothing. */
  readonly role?: MonoRole;
}

export interface Mistake {
  /** Where the offending run starts and ends in the source. */
  readonly from: number;
  readonly to: number;
  /** Which line it is on, counted from zero, and which column within that line. */
  readonly line: number;
  readonly column: number;
  readonly text: string;
  /** What is wrong, in the bad role. */
  readonly said: string;
  /** What the language has instead. Drawn beside the mistake, never as a red line under the code. */
  readonly hint: readonly Segment[];
}

/** The two halves of a bracket pair, or nothing when the caret is not beside one. */
export interface Pair {
  readonly open: number;
  readonly close: number;
}

export const OPENS = "([{";
export const CLOSES = ")]}";

function isBracket(character: string | undefined): boolean {
  return character !== undefined && (OPENS.includes(character) || CLOSES.includes(character));
}

/**
 * The bracket the caret is beside, and its partner.
 *
 * Looked for on both sides of the caret, the way every editor does it: a caret after `)` is at that
 * bracket as much as a caret before `(` is. A bracket inside a string or a comment is text, so the
 * search walks the spans rather than the characters — the tokeniser has already decided which is
 * which, and deciding it twice is how the two come to disagree.
 */
export function pairAt(source: string, caret: number, spans: readonly Span[]): Pair | undefined {
  const structural = spans.filter((span) => span.to - span.from === 1 && isBracket(source[span.from]));
  const at = structural.find((span) => span.from === caret || span.to === caret);
  if (!at) return undefined;
  const character = source[at.from]!;
  const forward = OPENS.includes(character);
  const partner = forward ? CLOSES[OPENS.indexOf(character)] : OPENS[CLOSES.indexOf(character)];
  const order = forward ? structural.filter((it) => it.from > at.from) : structural.filter((it) => it.from < at.from).reverse();
  let depth = 0;
  for (const span of order) {
    const here = source[span.from]!;
    if (here === character) depth += 1;
    else if (here === partner) {
      if (depth === 0) return forward ? { open: at.from, close: span.from } : { open: span.from, close: at.from };
      depth -= 1;
    }
  }
  return undefined;
}

/** The gutter's mark for one line: `  3 ●` where a mistake sits, `  4  ` where none does. */
export function gutterMark(line: number, marked: readonly number[]): Segment[] {
  const number: Segment = { text: String(line + 1).padStart(3, " "), role: "mono-faint" };
  return marked.includes(line)
    ? [number, { text: " " }, { text: "●", role: "mono-bad" }]
    : [number, { text: " " }, { text: " " }];
}

/** The line a source offset is on, counted from zero. */
export function lineAt(source: string, at: number): number {
  let line = 0;
  for (let index = 0; index < at && index < source.length; index += 1) if (source[index] === "\n") line += 1;
  return line;
}

/**
 * The two rows a mistake draws: carets under the offending run, and the hint beside them.
 *
 * Carets point at the precise token rather than underlining the whole expression.
 */
export function mistakeRows(mistake: Mistake): { readonly carets: Segment[]; readonly said: Segment[] } {
  return {
    carets: [
      { text: " ".repeat(mistake.column) },
      { text: "^".repeat(Math.max(1, mistake.text.length)), role: "mono-bad" },
    ],
    said: [
      { text: mistake.said, role: "mono-bad" },
      { text: "   ", role: "mono-faint" },
      ...mistake.hint,
    ],
  };
}

/** A one-line hint: `lead` in dim, then the thing the language has instead, or nothing at all. */
export function hint(lead: string, what: Segment): Segment[] {
  return what.text === "" ? [{ text: lead, role: "mono-dim" }] : [{ text: lead, role: "mono-dim" }, what];
}
