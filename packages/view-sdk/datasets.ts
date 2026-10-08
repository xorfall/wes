/**
 * Read-only Dataset inputs.
 *
 * A Dataset field of a View's input is a descriptor of one committed snapshot, never its records.
 * Records are read one bounded page at a time through the host, which alone knows which input is
 * drawn and may read it. A View names only a path inside its own input and a position; it never
 * holds an address, a token or a network capability, and nothing it asks for runs a command,
 * starts a source, records or refreshes anything.
 *
 * Every count, ordinal and byte size is an exact decimal string and may exceed a JavaScript number.
 */

/** The identity of one committed snapshot, exactly as Wes wrote it. */
export interface DatasetReference {
  readonly store: string; readonly dataset: string; readonly generation: string;
  readonly manifest: string; readonly manifestDigest: string; readonly manifestBytes: string;
  readonly schemaDigest: string; readonly records: string; readonly authorizationGeneration: string;
}
/** A Dataset field as a View receives it. `Element` is the declared record contract of its rows. */
export interface DatasetRef<Element = unknown> {
  readonly kind: "dataset";
  readonly reference: DatasetReference;
  /** Type-only marker for the row contract; never present at runtime. */
  readonly __element?: Element;
}
/**
 * How the snapshot stands. `prefix`: this snapshot is an earlier committed prefix and later
 * generations exist; it is not a stopped or interrupted writer. An open snapshot is `interrupted`
 * only when it is the latest and its writer was lost. The recording coverage of an `open` or `prefix`
 * snapshot never records a termination.
 */
export type DatasetLifecycle = "open" | "prefix" | "sealed" | "incomplete" | "interrupted" | "cancelled" | "restricted" | "deleted";
/** Where a page starts: a canonical ordinal, or the cursor a previous page returned. */
export type DatasetPosition = { readonly from: string } | { readonly cursor: string };

export type RecordingTermination = "natural" | "manual" | "cancelled" | "source_failed" | "rejected" | "overloaded" | "limit" | "write_failed" | "unconfirmed";
/**
 * What an event recording committed into exactly this snapshot, as its manifest records it. It is
 * fixed with the snapshot: it is not the writer's current progress, and a null termination does not
 * mean a recording is active. Present only for Datasets made by a recording.
 */
export interface DatasetRecordingCoverage {
  /** Sequences `first` through `committedThrough` are this snapshot's records. */
  readonly first: string;
  readonly committedThrough: string;
  /** Accepted by the writer; events past `committedThrough` were not committed in this snapshot. */
  readonly acceptedThrough: string;
  /** Accepted but uncommitted events, or null when that count is unknown. */
  readonly pending: string | null;
  readonly rejected: string;
  /** How this snapshot's recording ended, or null when this snapshot records no end. */
  readonly termination: RecordingTermination | null;
}
export interface DatasetSnapshot {
  readonly generation: string;
  /** Records committed in this snapshot. The end of the snapshot is not the end of its producer. */
  readonly records: string;
  readonly lifecycle: DatasetLifecycle;
  readonly recording?: DatasetRecordingCoverage;
}
export interface DatasetRow<Element> {
  readonly ordinal: string; readonly sourceStart: string; readonly sourceEnd: string;
  readonly value: Element;
}
export interface DatasetPage<Element> extends DatasetSnapshot {
  readonly first: string;
  readonly next: string;
  readonly rows: readonly DatasetRow<Element>[];
  /** Whether this page reached the end of the committed snapshot. */
  readonly extentExhausted: boolean;
  /** Opaque continuation for the next page, or null at the end of the snapshot. */
  readonly cursor: string | null;
}
export interface DatasetInspection extends DatasetSnapshot {
  readonly protected: boolean;
  readonly segmentBytes: string;
}

/**
 * Why a read did not complete. Coarse on purpose: a View learns what it may do next, never a
 * private detail of the store.
 * - `unavailable`: this host or input cannot read Datasets.
 * - `busy`: too many reads in flight; try again later.
 * - `changed`: the input or session changed; read the new input.
 * - `withdrawn`: access to the data was withdrawn.
 * - `limit`: the page is too large; ask for fewer rows.
 * - `invalid`: the path, position or limit is not acceptable, or the reply was not one page.
 * - `failed`: the read failed.
 */
export type DatasetErrorCode = "unavailable" | "busy" | "changed" | "withdrawn" | "limit" | "invalid" | "failed";
export const DATASET_ERROR_CODES: readonly DatasetErrorCode[] = ["unavailable", "busy", "changed", "withdrawn", "limit", "invalid", "failed"];
export class ViewDatasetError extends Error {
  constructor(readonly code: DatasetErrorCode) { super(`Dataset read ${code}`); this.name = "ViewDatasetError"; }
}

/**
 * Page reads for the Dataset fields of this View's input. `select` is a JSON Pointer into the input
 * (`/events`, `/runs/0/events`), never a URL or identifier. At most two reads are in flight; a new
 * input rejects every read still in flight with `changed`.
 */
export interface ViewDatasets {
  page<Element = unknown>(select: string, position?: DatasetPosition, limit?: number): Promise<DatasetPage<Element>>;
  inspect(select: string): Promise<DatasetInspection>;
}

export const DATASET_READS_IN_FLIGHT = 2;
export const DATASET_PAGE_LIMIT_MAX = 100;
const DECIMAL = /^(?:0|[1-9][0-9]{0,19})$/;
const CURSOR = /^[A-Za-z0-9_-]{1,4096}$/;

/** The request a View may make, checked before it leaves the frame. Undefined when it is not one. */
export function datasetRequest(operation: "page" | "inspect", select: unknown, position?: unknown, limit?: unknown):
  { operation: "page" | "inspect"; select: string; position?: DatasetPosition; limit?: number } | undefined {
  if (typeof select !== "string" || select.length > 2048 || (select !== "" && !select.startsWith("/"))) return undefined;
  if (operation === "inspect") return position === undefined && limit === undefined ? { operation, select } : undefined;
  let checked: DatasetPosition = { from: "0" };
  if (position !== undefined) {
    if (typeof position !== "object" || position === null) return undefined;
    const keys = Object.keys(position);
    if (keys.length !== 1) return undefined;
    const raw = position as Record<string, unknown>;
    if (keys[0] === "from" && typeof raw.from === "string" && DECIMAL.test(raw.from) && BigInt(raw.from) <= 18446744073709551615n) checked = { from: raw.from };
    else if (keys[0] === "cursor" && typeof raw.cursor === "string" && CURSOR.test(raw.cursor)) checked = { cursor: raw.cursor };
    else return undefined;
  }
  if (limit !== undefined && (typeof limit !== "number" || !Number.isInteger(limit) || limit < 1 || limit > DATASET_PAGE_LIMIT_MAX)) return undefined;
  return { operation, select, position: checked, ...(limit === undefined ? {} : { limit }) };
}

/**
 * The frame side: sends requests through the View's own port and settles them from the host's
 * replies. `send` throws when the message budget is exceeded.
 */
export class FrameDatasets implements ViewDatasets {
  private next = 0;
  private readonly pending = new Map<number, { resolve: (value: unknown) => void; reject: (error: ViewDatasetError) => void }>();
  constructor(private readonly send: (message: unknown) => void) {}
  page<Element>(select: string, position?: DatasetPosition, limit?: number): Promise<DatasetPage<Element>> {
    return this.request("page", select, position, limit) as Promise<DatasetPage<Element>>;
  }
  inspect(select: string): Promise<DatasetInspection> {
    return this.request("inspect", select) as Promise<DatasetInspection>;
  }
  private request(operation: "page" | "inspect", select: string, position?: DatasetPosition, limit?: number): Promise<unknown> {
    const checked = datasetRequest(operation, select, position, limit);
    if (!checked) return Promise.reject(new ViewDatasetError("invalid"));
    if (this.pending.size >= DATASET_READS_IN_FLIGHT) return Promise.reject(new ViewDatasetError("busy"));
    const request = ++this.next;
    return new Promise((resolve, reject) => {
      this.pending.set(request, { resolve, reject });
      try { this.send({ kind: "dataset-read", request, ...checked }); }
      catch { this.pending.delete(request); reject(new ViewDatasetError("failed")); }
    });
  }
  /** A reply from the host. Unknown or late replies are ignored. */
  settle(message: { request?: unknown; ok?: unknown; result?: unknown; error?: unknown }): void {
    if (typeof message.request !== "number") return;
    const waiting = this.pending.get(message.request);
    if (!waiting) return;
    this.pending.delete(message.request);
    if (message.ok === true) waiting.resolve(deepFreeze(message.result));
    else waiting.reject(new ViewDatasetError(DATASET_ERROR_CODES.includes(message.error as DatasetErrorCode) ? message.error as DatasetErrorCode : "failed"));
  }
  /** The input changed or the frame is closing: nothing in flight can complete for it. */
  cancelAll(code: DatasetErrorCode = "changed"): void {
    const waiting = [...this.pending.values()];
    this.pending.clear();
    for (const entry of waiting) entry.reject(new ViewDatasetError(code));
  }
}

function deepFreeze<T>(value: T): T {
  if (value && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const child of Object.values(value as object)) deepFreeze(child);
  }
  return value;
}
