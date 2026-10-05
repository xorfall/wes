/**
 * What `/edit` says about itself, apart from the code it is editing.
 *
 * Editor labels projected from the session: the head that names what
 * is bound and how to run it, the output pane's own head, and the footer's keys. Pure, like every
 * other model here, so the sentences are testable without mounting an editor.
 */
import { primaryGlyph } from "../platform-keys";
import type { Segment } from "./MonoLine";

/**
 * What the editor is bound to, once it has run something.
 *
 * A cell, not a result: a run the engine refused made no result but did make a cell, and that cell
 * is what `⌘R` repeats and what the scrollback shows. So `node` is absent for exactly that case,
 * and the head says what happened rather than printing an identifier nobody can use.
 */
export interface Bound {
  /** The result the last run made, when it made one. */
  readonly node?: string;
  /** Its given name, when it has one, so the head can say `$revenue` rather than an id. */
  readonly name?: string;
  /** When it last ran, as the head prints it. */
  readonly ran?: string;
}

const DOT: Segment = { text: "  ·  ", role: "mono-faint" };
const GAP: Segment = { text: "   ", role: "mono-faint" };

/** How a bound result is referred to: its name if it has one, else the id the engine gave it. */
export function boundName(bound: Bound | undefined): string | undefined {
  if (!bound || bound.node === undefined) return undefined;
  return bound.name ? `$${bound.name}` : bound.node;
}

/**
 * `$revenue   ⌘⏎ run · ⌘R run again · esc back to the session, the draft kept`
 *
 * The screen's own chrome
 * prints its name beside the subject, as it does for every other summoned screen. Repeating it here
 * would put `/edit   /edit` on the line.
 */
export function editHead(bound: Bound | undefined): Segment[] {
  const named = boundName(bound);
  const mod = primaryGlyph();
  return [
    ...(named ? ([{ text: named, role: "mono-ref" }, { text: "   " }] as Segment[]) : []),
    { text: `${mod}⏎`, role: "mono-ref" }, { text: " run", role: "mono-dim" }, DOT,
    // Nothing has been run yet, so there is nothing to run again and the key is not offered.
    ...(bound ? ([{ text: `${mod}R`, role: "mono-ref" }, { text: " run again", role: "mono-dim" }, DOT] as Segment[]) : []),
    { text: "esc", role: "mono-ref" },
    { text: " back to the session, the draft kept", role: "mono-dim" },
  ];
}

/** The session's own top line, with what is being edited on the end of it. */
export function editingTop(top: readonly Segment[], bound: Bound | undefined): Segment[] {
  const named = boundName(bound);
  if (!named) return [...top];
  return [...top, DOT, { text: "editing", role: "mono-meta" }, { text: " " }, { text: named, role: "mono-ref" }];
}

/**
 * `output   $revenue · ran 09:16 · ⌘R runs the same node again · the scrollback's cell updates with it`
 *
 * The last clause is the point of the pane: it is not a second result, it is the same node seen
 * from here. Before the first run it says so instead, because a pane that pretended to hold
 * something would be the one thing a person cannot check.
 */
export function outputHead(bound: Bound | undefined): Segment[] {
  const mod = primaryGlyph();
  if (!bound) {
    return [
      { text: "output", role: "mono-dim" }, GAP,
      { text: "not run yet", role: "mono-faint" }, DOT,
      { text: `${mod}⏎`, role: "mono-ref" }, { text: " runs it", role: "mono-dim" },
    ];
  }
  const named = boundName(bound);
  return [
    { text: "output", role: "mono-dim" }, GAP,
    // A refused run made no result to name, and saying so is the honest half of "it ran".
    ...(named
      ? ([{ text: named, role: "mono-ref" }, DOT] as Segment[])
      : ([{ text: "it made no result", role: "mono-warn" }, DOT] as Segment[])),
    ...(bound.ran ? ([{ text: `ran ${bound.ran}`, role: "mono-faint" }, DOT] as Segment[]) : []),
    { text: `${mod}R`, role: "mono-ref" },
    { text: named ? " runs the same node again" : " runs it again", role: "mono-dim" }, DOT,
    { text: "the scrollback's cell updates with it", role: "mono-faint" },
  ];
}

/**
 * `⌘⏎ run   ⌘R run again   ⇧⏎ newline   ⌘/ comment   ⇥ indent   ⌃space complete   esc back to the session, the draft kept`
 *
 * Exactly the editor's keymap (`calc-editor.ts`), with `⌘` read as the platform's primary modifier.
 * `⇥` indents — CodeMirror's `indentWithTab` — and an open completion list is accepted with `⏎`;
 * `esc` closes that list first and only then leaves.
 */
export function editKeys(): Segment[] {
  const mod = primaryGlyph();
  return [
    { text: `${mod}⏎`, role: "mono-ref" }, { text: " run", role: "mono-dim" }, GAP,
    { text: `${mod}R`, role: "mono-ref" }, { text: " run again", role: "mono-dim" }, GAP,
    { text: "⇧⏎", role: "mono-ref" }, { text: " newline", role: "mono-dim" }, GAP,
    { text: `${mod}/`, role: "mono-ref" }, { text: " comment", role: "mono-dim" }, GAP,
    { text: "⇥", role: "mono-ref" }, { text: " indent", role: "mono-dim" }, GAP,
    { text: "⌃space", role: "mono-ref" }, { text: " complete", role: "mono-dim" }, GAP,
    { text: "esc", role: "mono-ref" }, { text: " back to the session, the draft kept", role: "mono-dim" },
  ];
}

/** `09:16` — when a run happened, as the head prints it. */
export function ranAt(at: string | undefined): string | undefined {
  if (at === undefined || at === "") return undefined;
  const date = new Date(at);
  if (Number.isNaN(date.getTime())) return undefined;
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}
