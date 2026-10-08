/**
 * How a recording's end is said, wherever it is shown: the immutable coverage of a committed
 * generation and the latest writer status alike. `warn` marks ends that lost or refused events.
 */
import type { RecordingTermination } from "../dataset-read";

export const TERMINATION_TEXT: Readonly<Record<RecordingTermination, { readonly text: string; readonly warn: boolean }>> = {
  natural: { text: "the source ended", warn: false },
  manual: { text: "stopped on request", warn: false },
  cancelled: { text: "cancelled", warn: true },
  source_failed: { text: "the source failed", warn: true },
  rejected: { text: "ended on rejected events", warn: true },
  overloaded: { text: "ended: the writer was overloaded", warn: true },
  limit: { text: "ended at its limit", warn: true },
  write_failed: { text: "ended: a write failed", warn: true },
  unconfirmed: { text: "end unconfirmed", warn: true },
};
