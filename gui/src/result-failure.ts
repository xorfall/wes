/** Result storage/read evidence from authoritative codes; messages and HTTP status are not parsed. */
export type ResultFailureKind = "capacity" | "forbidden" | "stale" | "unavailable" | "closed" | "corrupt" | "uncertain" | "failed" | "busy";
export interface ResultFailure {
  readonly kind: ResultFailureKind;
  readonly title: string;
  readonly next: string;
  /** Manual same-result reading only; never permission to resubmit a command or Keep. */
  readonly canReadAgain: boolean;
}

const failures: Record<ResultFailureKind, ResultFailure> = {
  capacity: { kind: "capacity", title: "Result capacity exceeded", next: "Review capacity limits. For an existing stored result, select a smaller field or use a bounded source scan. This refusal does not mean the result was deleted.", canReadAgain: false },
  forbidden: { kind: "forbidden", title: "Read access refused", next: "Review current access before selecting this result again.", canReadAgain: false },
  stale: { kind: "stale", title: "Result selection changed", next: "Select the existing result again in the current workspace.", canReadAgain: false },
  unavailable: { kind: "unavailable", title: "Stored result unavailable", next: "Inspect retained results and source dependencies.", canReadAgain: false },
  closed: { kind: "closed", title: "Result owner unavailable", next: "Reconnect to the owning session before reading. This does not establish that the result is missing.", canReadAgain: false },
  corrupt: { kind: "corrupt", title: "Stored result validation failed", next: "Inspect application diagnostics or another retained copy.", canReadAgain: false },
  uncertain: { kind: "uncertain", title: "Storage outcome unconfirmed", next: "Check storage or retention state before any new operation. Do not automatically repeat Keep, removal or the command.", canReadAgain: false },
  failed: { kind: "failed", title: "Could not read result", next: "Inspect diagnostics or retry only this result read. No command will be rerun.", canReadAgain: true },
  busy: { kind: "busy", title: "Result readers busy", next: "Read this same result again shortly. Do not rerun the command.", canReadAgain: true },
};
const kinds: Record<string, ResultFailureKind> = {
  RUN007: "capacity", VALUE_CAPACITY: "capacity", VALUE_PREVIEW_LIMIT: "capacity", ANALYSIS_CAPACITY: "capacity",
  RUN008: "unavailable", VALUE_UNAVAILABLE: "unavailable", ANALYSIS_UNAVAILABLE: "unavailable",
  RUN009: "forbidden", VALUE_ACCESS_REFUSED: "forbidden", VALUE_ACCESS_WITHDRAWN: "forbidden",
  VALUE_PRIVATE_UNAVAILABLE: "forbidden",
  ANALYSIS_ACCESS_REFUSED: "forbidden", ANALYSIS_ACCESS_WITHDRAWN: "forbidden", ANALYSIS_SCOPE_ENDED: "forbidden",
  RUN010: "stale", VALUE_REVISION_CONFLICT: "stale", VALUE_SESSION_CHANGED: "stale", VALUE_READ_AUTHORITY_CHANGED: "stale", ANALYSIS_CHANGED: "stale",
  RUN011: "closed", VALUE_OWNER_UNAVAILABLE: "closed", VALUE_SESSION_UNAVAILABLE: "closed", ANALYSIS_OWNER_UNAVAILABLE: "closed",
  RUN012: "corrupt", VALUE_CORRUPT: "corrupt", ANALYSIS_CORRUPT: "corrupt",
  RUN013: "uncertain", VALUE_STORAGE_UNCONFIRMED: "uncertain", ANALYSIS_UNCONFIRMED: "uncertain",
  RUN014: "failed", VALUE_READ_FAILED: "failed", VALUE_ENCODING_FAILED: "failed", ANALYSIS_READ_FAILED: "failed",
  VALUE_READ_BUSY: "busy", ANALYSIS_BUSY: "busy",
  DATASET_READ_LIMIT: "capacity", DATASET_REPLY_LIMIT: "capacity",
  DATASET_MISSING: "unavailable", DATASET_ACCESS_REFUSED: "forbidden", DATASET_WITHDRAWN: "forbidden",
  DATASET_CONTINUITY_CHANGED: "stale", DATASET_SESSION_CHANGED: "stale", DATASET_READ_AUTHORITY_CHANGED: "stale",
  DATASET_OWNER_UNAVAILABLE: "closed", DATASET_CORRUPT: "corrupt", DATASET_READ_UNCONFIRMED: "uncertain",
  DATASET_READ_FAILED: "failed", DATASET_ENCODING_FAILED: "failed", DATASET_READ_BUSY: "busy",
};
export function resultFailure(code: string | undefined): ResultFailure | undefined {
  if (code === "VALUE_PREVIEW_LIMIT") return { ...failures.capacity, title: "Preview limit exceeded", next: "Open the result to inspect it. This refusal does not mean the result was deleted." };
  if (code === "VALUE_PREVIEW_UNAVAILABLE") return { ...failures.forbidden, title: "Preview not available", next: "Open the result to inspect its current read access." };
  const kind = code === undefined || !Object.hasOwn(kinds, code) ? undefined : kinds[code];
  return kind === undefined ? undefined : failures[kind];
}
