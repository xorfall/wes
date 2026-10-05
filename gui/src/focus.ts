/**
 * A template the console is locked to, and the arithmetic of typing into it.
 *
 * <p>`/focus sh run cmd:"_"` fixes everything but the part that changes, so a session spent on one
 * capability only needs to edit the changing argument instead of retyping the command prefix.
 *
 * <p><b>Nothing here resolves anything.</b> Pressing Enter joins two strings. A `$bars` typed into the
 * hole stays `$bars`; the engine's resolver turns it into a node, exactly as it always did. Once the
 * line is joined, the engine, the journal, the cell and the rerun button all behave as though `/focus`
 * had never existed — a session that has forgotten the mode replays the same journal correctly.
 *
 * <p>Kept separate from the components for the reason `choose.ts` is: the interesting part is the offset
 * arithmetic, and the arithmetic is what can be wrong.
 */
export interface Focus {
  /** The template, always holding exactly one hole character. */
  readonly template: string;
  /** Where the hole is. `template[hole]` is always {@link HOLE}. */
  readonly hole: number;
}

/**
 * The character that stands for what you type.
 *
 * <p>`$` was the first idea and it collides: `$bars` is already a reference to a result, and both appear
 * on the same line — `/focus ta series bars:$bars period:_`. An underscore is a hole in half the
 * languages that have one, and means nothing to this grammar.
 */
export const HOLE = "_";

/**
 * Focused on nothing: the template is the hole itself.
 *
 * <p>So that the unfocused console is the same arithmetic rather than a branch around it —
 * {@link expand} returns what was typed, and {@link acceptInto} reduces to a plain splice.
 */
export const UNFOCUSED: Focus = { template: HOLE, hole: 0 };

/**
 * Reads a template.
 *
 * <p>A template with no hole gets one at the end, so `/focus sh run` and `/focus sh run _` are the same
 * thing — appending is a hole in the last position, not a second mechanism.
 *
 * @param template what was written after `/focus`
 * @return the focus, or a sentence saying why it is not one
 */
export function focusOn(template: string): Focus | string {
  const written = template.trim();
  if (written === "") {
    return "say what to focus on, as in '/focus sh run cmd:\"_\"'";
  }
  const holes = written.split(HOLE).length - 1;
  if (holes > 1) {
    /*
     * Refused rather than resolved to the first one. Two holes is somebody meaning two different things,
     * and filling only the first would build a command they did not write — silently, every time.
     */
    return `'${written}' has ${holes} '${HOLE}' in it, and only one can be the hole`;
  }
  return holes === 1
    ? { template: written, hole: written.indexOf(HOLE) }
    : { template: `${written} ${HOLE}`, hole: written.length + 1 };
}

/** What the prompt should show: the template, with the hole where your text goes. */
export function describe(focus: Focus): string {
  return focus.template;
}

/**
 * Puts what was typed into the hole.
 *
 * @param focus the template
 * @param typed what is in the input right now
 * @return the whole command, as the engine will see it
 */
export function expand(focus: Focus, typed: string): string {
  return focus.template.slice(0, focus.hole) + typed + focus.template.slice(focus.hole + 1);
}

/** Where the caret sits once the line is expanded. */
export function expandCaret(focus: Focus, caret: number): number {
  return focus.hole + caret;
}

/**
 * Takes a suggestion made about the expanded line and puts it back into what was typed.
 *
 * <p>Completion runs on the expanded line so that it sees a real command — otherwise `ls` in
 * `/focus sh run cmd:"_"` would be completed as though it were a provider name. But the suggestion comes
 * back with an offset into that expanded line, and the input holds only the typed part.
 *
 * <p>Two cases, and the second is the one worth explaining. When the suggested word begins inside the
 * template — completing `$ba` in `symbol:_` means the word is `symbol:$ba` — the template contributed
 * its first characters, and the suggestion must keep them. It always does: {@code offer} filters
 * suggestions by the prefix of the word it found, and that word starts with the template's contribution.
 * So dropping exactly that many characters off the front is safe, and leaves what belongs in the hole.
 *
 * @param focus      the template
 * @param typed      what is in the input right now
 * @param caret      where the caret is in the input
 * @param from       where the suggested word starts, as an index into the expanded line
 * @param suggestion the whole word being suggested
 * @param separate whether a completed command word should be followed by a token separator
 * @return what the input should hold, and where the caret goes
 */
export function acceptInto(
  focus: Focus,
  typed: string,
  caret: number,
  from: number,
  suggestion: string,
  separate = false,
): { readonly text: string; readonly caret: number } {
  let put: { readonly text: string; readonly caret: number };
  if (from >= focus.hole) {
    const start = Math.min(from - focus.hole, caret);
    put = {
      text: typed.slice(0, start) + suggestion + typed.slice(caret),
      caret: start + suggestion.length,
    };
  } else {
    const fromTemplate = Math.min(focus.hole - from, suggestion.length);
    const kept = suggestion.slice(fromTemplate);
    put = { text: kept + typed.slice(caret), caret: kept.length };
  }
  if (!separate || suggestion.endsWith(":")) return put;
  const following = put.text.slice(put.caret);
  if (/^[ \t]/.test(following)) return { ...put, caret: put.caret + 1 };
  // Never insert into a token, cross a newline, or add data inside a template's closing quote.
  if (following !== "" || focus.template.slice(focus.hole + 1) !== "") return put;
  return { text: `${put.text} `, caret: put.caret + 1 };
}
