import { MonoLine, lineText, type Segment } from "./MonoLine";
import type { VerdictField } from "./session-model";

/**
 * The run band's fixed columns.
 *
 * State, duration and retention each keep one place and one width whatever they say, so a ticking
 * duration or a long `outcome unknown` never moves its neighbours. Which column a field belongs to
 * is the projection's metadata (`VerdictField.slot`), never something read off its text. Every
 * other field is a note: explanatory text drawn whole on its own line beneath the columns.
 */
export type RunSlot = NonNullable<VerdictField["slot"]>;

/** Left to right, the order the columns are drawn in. */
export const RUN_SLOTS: readonly RunSlot[] = ["state", "duration", "retention"];

const SLOT_NAME: Record<RunSlot, string> = { state: "State", duration: "Duration", retention: "Retention" };
const SEPARATOR: Segment = { text: " · ", role: "mono-faint" };

export interface RunBand {
  /** One line per column; empty when the engine has not said it, which is not zero. */
  readonly slots: Readonly<Record<RunSlot, readonly Segment[]>>;
  /** The unslotted fields in their projected order, ` · ` between them. */
  readonly notes: readonly Segment[];
}

function joinFields(fields: readonly VerdictField[]): Segment[] {
  return fields.flatMap((field, at) => (at === 0 ? [...field.segments] : [SEPARATOR, ...field.segments]));
}

/**
 * Splits a verdict into its columns and notes.
 *
 * A verdict that declares no state column — a hand-written gallery fixture — has its first field
 * read as the state, since that is the order every verdict is written in.
 *
 * @param verdict the projected verdict fields
 * @return the segments of each column and the notes line
 */
export function runBandOf(verdict: readonly VerdictField[]): RunBand {
  const declared = verdict.some(field => field.slot === "state");
  const slotOf = (field: VerdictField, at: number): RunSlot | undefined => field.slot ?? (!declared && at === 0 ? "state" : undefined);
  const placed = verdict.map((field, at) => ({ field, slot: slotOf(field, at) }));
  const column = (slot: RunSlot) => joinFields(placed.filter(it => it.slot === slot).map(it => it.field));
  return {
    slots: { state: column("state"), duration: column("duration"), retention: column("retention") },
    notes: joinFields(placed.filter(it => it.slot === undefined).map(it => it.field)),
  };
}

/**
 * One column of the run band. A cut value keeps its whole text in its title and accessible name;
 * an empty one still holds its width and says nothing.
 *
 * @param slot which column this is
 * @param segments what the column says, possibly nothing
 */
export function RunSlotValue({ slot, segments }: { readonly slot: RunSlot; readonly segments: readonly Segment[] }) {
  const text = lineText(segments);
  if (text === "") return <span className={`cell-slot cell-slot-${slot}`} aria-hidden="true" />;
  return <span className={`cell-slot cell-slot-${slot}`} role="group" aria-label={`${SLOT_NAME[slot]}: ${text}`} title={text}>
    <MonoLine segments={segments} />
  </span>;
}
