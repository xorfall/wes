/**
 * Bounded reads of one Dataset held by a stored result: `GET /datasets/{handle}`.
 *
 * A read names the stored value by its handle and the Dataset inside it by a JSON Pointer, so it
 * can only reach a Dataset the person can already read; there is no way to name a dataset, a file
 * or a producer directly. A read never starts recording, a source, an analysis or a refresh.
 *
 * Every ordinal, count and byte size stays the exact decimal string the server wrote. Arithmetic
 * on them is done in BigInt and only for bounded steps (a previous page start, a remaining count);
 * a value is never passed through a float.
 *
 * The decoder accepts exactly the documented reply and refuses the rest as a whole, so a
 * malformed or mismatched page is never drawn as if it were records.
 */
import { readExactJson } from "./exact-json";
import type { StoredValue } from "./protocol";
import { DATASET_REFERENCE_FIELDS, decodeDatasetReference, type DatasetReference } from "./presentation/dataset";
import { typeShapeOf } from "./presentation/type-shape";
import { withValidMeta } from "./value-meta";

/** Rows one page may ask for; the server refuses anything outside this range. */
export const DATASET_PAGE_MIN = 1;
export const DATASET_PAGE_MAX = 100;
export const DATASET_PAGE_DEFAULT = 50;
/** Longest select pointer and cursor the server accepts. */
const MAX_SELECT_BYTES = 2048;
const MAX_CURSOR_BYTES = 4096;
const U64_MAX = 18446744073709551615n;
const DECIMAL = /^(?:0|[1-9][0-9]{0,19})$/;
const CURSOR = /^[A-Za-z0-9_-]+$/;

/**
 * How the selected manifest stands. `prefix` is read-only: this exact open manifest is an earlier
 * committed prefix and later generations of it exist. It says nothing about the writer and is not
 * `interrupted`: an open manifest reads as interrupted only when it is the latest and its writer was lost.
 */
export const DATASET_LIFECYCLES = ["open", "prefix", "sealed", "incomplete", "interrupted", "cancelled", "restricted", "deleted"] as const;
export type DatasetLifecycle = (typeof DATASET_LIFECYCLES)[number];

/**
 * Which typed stream of the selected manifest a read pages. `outputs` are the Dataset's own records,
 * the ones its descriptor counts. `coverage` are the records native forensic framing skipped in an
 * analysis tree: their own dense ordinals, counted by the read's coverage, never by the descriptor.
 */
export const DATASET_STREAMS = ["outputs", "coverage"] as const;
export type DatasetStream = (typeof DATASET_STREAMS)[number];

/** Where a page starts: a canonical ordinal, or the cursor the previous page returned. */
export type DatasetPosition = { readonly from: string } | { readonly cursor: string };

export interface DatasetPageQuery {
  /** JSON Pointer to the Dataset inside the stored value; empty for the value itself. */
  readonly select: string;
  readonly position: DatasetPosition;
  readonly limit: number;
}

export interface DatasetRow {
  readonly ordinal: string;
  readonly sourceStart: string;
  readonly sourceEnd: string;
  readonly value: StoredValue;
  /** A coverage row's own record, read from `value`; present exactly on coverage pages. */
  readonly rejection?: ScanRejection;
}

export const REJECTION_REASONS = ["raw_limit", "invalid_utf8", "decoded_limit", "span_limit"] as const;
export type RejectionReason = (typeof REJECTION_REASONS)[number];

/**
 * One record native framing refused and skipped. Offsets are original source bytes: content
 * `sourceStart–sourceEnd`, then its delimiter, empty when the record is unterminated. `reason` is
 * the framing refusal, not a failed transition; nothing in the record was interpreted. `excerpt` is
 * the stored base64 of at most the coverage's excerpt bound of original content bytes from its start,
 * never a decoded message or the whole record.
 */
export interface ScanRejection {
  readonly reason: RejectionReason;
  /** The skipped record's ordinal among all source records, not its coverage ordinal. */
  readonly recordOrdinal: string;
  readonly sourceStart: string;
  readonly sourceEnd: string;
  readonly delimiterStart: string;
  readonly delimiterEnd: string;
  readonly unterminated: boolean;
  readonly reasonStart: string;
  readonly reasonEnd: string;
  readonly excerpt: string;
  /** Bytes the excerpt holds, exact. */
  readonly excerptSize: string;
  readonly excerptTruncated: boolean;
}

/**
 * The skipped-record stream of an analysis tree, as its acknowledged manifest counts it: `records`
 * skipped records holding `inputBytes` original bytes, the last of them source record `lastOrdinal`
 * ending at byte `through`. It says nothing about producer completion or how much was interpreted.
 */
export interface DatasetCoverage {
  readonly records: string;
  readonly inputBytes: string;
  readonly lastOrdinal: string | null;
  readonly through: string | null;
  readonly excerptBytes: string;
  readonly segmentBytes: string;
}

export interface DatasetPage {
  readonly first: string;
  /** The ordinal after the last row: where the next page starts. */
  readonly next: string;
  readonly rows: readonly DatasetRow[];
  /** Whether this page reached the end of the committed snapshot; not that the producer ended. */
  readonly extentExhausted: boolean;
  readonly limitedBy: string | null;
  readonly cursor: string | null;
}

export const RECORDING_TERMINATIONS = ["natural", "manual", "cancelled", "source_failed", "rejected", "overloaded", "limit", "write_failed", "unconfirmed"] as const;
export type RecordingTermination = (typeof RECORDING_TERMINATIONS)[number];

/**
 * What an event recording committed into exactly this generation: sequences `first` through
 * `committedThrough` are its records. It is the manifest's own account, fixed with the snapshot —
 * not the writer's current progress and not a live source window. `pending` null means the count
 * of accepted but uncommitted events is unknown; `termination` null means this generation records
 * no end, which says nothing about whether a writer is active now.
 */
export interface DatasetRecording {
  readonly run: string;
  readonly epoch: string;
  readonly first: string;
  readonly acceptedThrough: string;
  readonly committedThrough: string;
  readonly pending: string | null;
  readonly rejected: string;
  readonly termination: RecordingTermination | null;
}

export interface DatasetRead {
  readonly reference: DatasetReference;
  /** The stream this read pages; a page's rows and bounds belong to it alone. */
  readonly stream: DatasetStream;
  readonly lifecycle: DatasetLifecycle;
  readonly protected: boolean;
  readonly persistence: string;
  readonly segmentBytes: string;
  readonly page?: DatasetPage;
  /** Present only when the selected manifest carries recording coverage. */
  readonly recording?: DatasetRecording;
  /** Present only when the selected manifest is an analysis tree with forensic framing coverage. */
  readonly coverage?: DatasetCoverage;
}

/** How a failed read should be shown and whether anything may be retried. */
export type DatasetFailureKind =
  /** Readers are busy and the server said the same read may be retried. */
  | "busy"
  /** Access or the dataset was withdrawn: every cached detail must go. */
  | "withdrawn"
  /** The stored result is no longer there. */
  | "missing"
  /** The page or reply exceeds a server limit; a smaller page may fit. */
  | "limit"
  /** The workspace session changed; the reply belongs to an older session and is dropped. */
  | "session"
  /**
   * The followed head no longer has the saved snapshot's committed identity or analysis attempt.
   * Nothing switches to the newer attempt; the saved snapshot stays readable.
   */
  | "continuity"
  /** A refused request or a reply that does not decode. */
  | "invalid"
  /** The server failed to read stored data. */
  | "failed";

export class DatasetReadError extends Error {
  constructor(readonly kind: DatasetFailureKind, readonly status: number, readonly code: string, message: string, readonly retryable: boolean) {
    super(message);
    this.name = "DatasetReadError";
  }
}

/** Whether `text` is a canonical unsigned 64-bit decimal: no sign, no leading zero, no exponent. */
export function canonicalOrdinal(text: string): boolean {
  return DECIMAL.test(text) && BigInt(text) <= U64_MAX;
}

/**
 * Which committed snapshot of a stored result's Dataset a read names. `frozen` is the result's own
 * snapshot. `head` inspects the current committed head of the same EventLog epoch or analysis
 * attempt. `extent` pages a newer snapshot this client already read as that head. The stored
 * selection stays the authority; a descriptor alone never is. Only the stored-result route takes
 * these; View reads are always frozen.
 */
export type DatasetTarget = { readonly kind: "frozen" } | { readonly kind: "head" } | { readonly kind: "extent"; readonly reference: DatasetReference };
const FROZEN: DatasetTarget = { kind: "frozen" };
/** Longest extent descriptor the server accepts in the query. */
const MAX_EXTENT_BYTES = 4096;

/**
 * The query string for one page or one inspection, refusing what the server would refuse. Outputs
 * are the server's default stream and are not named. A head is only ever inspected on outputs:
 * skipped records are read from a fixed snapshot and are never followed.
 */
export function datasetQuery(select: string, position?: DatasetPosition, limit?: number, target: DatasetTarget = FROZEN, stream: DatasetStream = "outputs"): string {
  if (select !== "" && !select.startsWith("/")) throw new RangeError("select must be a JSON Pointer");
  if (new TextEncoder().encode(select).length > MAX_SELECT_BYTES) throw new RangeError("select is too long");
  if (!(DATASET_STREAMS as readonly string[]).includes(stream)) throw new RangeError("unknown stream");
  if (stream !== "outputs" && target.kind === "head") throw new RangeError("a head is inspected on outputs only");
  const query = new URLSearchParams();
  if (select !== "") query.set("select", select);
  if (stream !== "outputs") query.set("stream", stream);
  if (target.kind === "head") {
    // The head is only ever inspected; its records are then paged as a known extent.
    if (position) throw new RangeError("a head read is an inspection");
    query.set("inspect", "true");
    query.set("head", "true");
    return query.toString();
  }
  if (target.kind === "extent") {
    if (!position) throw new RangeError("an extent read is a page");
    // Exactly the descriptor's own fields, as the server wrote them.
    const extent = JSON.stringify(Object.fromEntries(DATASET_REFERENCE_FIELDS.map(name => [name, target.reference[name]])));
    if (new TextEncoder().encode(extent).length > MAX_EXTENT_BYTES) throw new RangeError("extent is too long");
    query.set("extent", extent);
  }
  if (!position) {
    // Inspection reads lifecycle and identity only; it cannot carry page arguments.
    query.set("inspect", "true");
    return query.toString();
  }
  if ("cursor" in position) {
    if (position.cursor.length > MAX_CURSOR_BYTES || !CURSOR.test(position.cursor)) throw new RangeError("invalid cursor");
    query.set("cursor", position.cursor);
  } else {
    if (!canonicalOrdinal(position.from)) throw new RangeError("from must be a canonical ordinal");
    query.set("from", position.from);
  }
  if (limit !== undefined) {
    if (!Number.isInteger(limit) || limit < DATASET_PAGE_MIN || limit > DATASET_PAGE_MAX) throw new RangeError("limit is out of range");
    query.set("limit", String(limit));
  }
  return query.toString();
}

function plain(value: unknown): value is Readonly<Record<string, unknown>> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}
function keysWithin(value: Readonly<Record<string, unknown>>, required: readonly string[], optional: readonly string[] = []): boolean {
  for (const key of required) if (!Object.hasOwn(value, key)) return false;
  for (const key of Object.keys(value)) if (!required.includes(key) && !optional.includes(key)) return false;
  return true;
}
const ordinalText = (value: unknown): value is string => typeof value === "string" && canonicalOrdinal(value);

function decodeStoredValue(raw: unknown): StoredValue | undefined {
  if (!plain(raw) || !keysWithin(raw, ["type", "provenance", "data"], ["meta"])) return undefined;
  if (!typeShapeOf(raw.type) || !plain(raw.provenance)) return undefined;
  if (Object.values(raw.provenance).some(value => typeof value !== "string")) return undefined;
  return withValidMeta(raw as unknown as StoredValue);
}

function decodeRow(raw: unknown): DatasetRow | undefined {
  if (!plain(raw) || !keysWithin(raw, ["ordinal", "sourceStart", "sourceEnd", "value"])) return undefined;
  const { ordinal, sourceStart, sourceEnd } = raw;
  if (!ordinalText(ordinal) || !ordinalText(sourceStart) || !ordinalText(sourceEnd)) return undefined;
  if (BigInt(sourceStart) > BigInt(sourceEnd)) return undefined;
  const value = decodeStoredValue(raw.value);
  return value && { ordinal, sourceStart, sourceEnd, value };
}

const REJECTION_FIELDS = ["kind", "reason", "recordOrdinal", "sourceStart", "sourceEnd", "delimiterStart", "delimiterEnd", "unterminated",
  "reasonStart", "reasonEnd", "excerpt", "excerptStart", "excerptTruncated"];
const REJECTION_OFFSETS = ["recordOrdinal", "sourceStart", "sourceEnd", "delimiterStart", "delimiterEnd", "reasonStart", "reasonEnd", "excerptStart"] as const;
/** The primitive each native rejection field is declared as; its enums are Text, offsets decimal Text. */
const REJECTION_FIELD_TYPES: ReadonlyMap<string, string> = new Map<string, string>([
  ["kind", "TEXT"], ["reason", "TEXT"], ...REJECTION_OFFSETS.map(name => [name, "TEXT"] as const),
  ["unterminated", "BOOL"], ["excerptTruncated", "BOOL"], ["excerpt", "BYTES"],
]);
/** Standard padded base64, as stored Bytes are written. */
const BASE64 = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;
const BASE64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const EXCERPT_MAX = 4096n;
/** Longest base64 text an excerpt of at most `EXCERPT_MAX` bytes encodes to. */
const EXCERPT_BASE64_MAX = Number((EXCERPT_MAX + 2n) / 3n * 4n);

/**
 * Whether `text` is canonical standard base64 of a bounded excerpt: checked by length before any
 * pattern, and with zero pad bits in the last sextet, so one byte string has exactly one encoding.
 */
function canonicalExcerpt(text: string): boolean {
  if (text.length > EXCERPT_BASE64_MAX || !BASE64.test(text)) return false;
  const padding = text.endsWith("==") ? 2 : text.endsWith("=") ? 1 : 0;
  if (padding === 0) return true;
  return BASE64_ALPHABET.indexOf(text.charAt(text.length - padding - 1)) % (padding === 2 ? 16 : 4) === 0;
}

/**
 * A coverage row's record, when it is exactly one native framing rejection that agrees with its own
 * row: non-empty content immediately followed by its delimiter (empty exactly when unterminated), a
 * non-empty reason span inside the content, and an excerpt that starts the content and holds the
 * excerpt bound or the whole content, whichever is smaller, truncated exactly when it is shorter. The
 * row's source range is the content start through the delimiter end.
 */
function decodeRejection(row: DatasetRow, excerptBytes: string): ScanRejection | undefined {
  const { type, data } = row.value;
  if (type.kind !== "record" || type.name !== "ScanRejection" || type.fields.length !== REJECTION_FIELDS.length
    || !REJECTION_FIELDS.every(name => type.fields.some(field => field.name === name))
    || !type.fields.every(field => field.type.kind === "primitive" && field.type.name === REJECTION_FIELD_TYPES.get(field.name))) return undefined;
  if (!plain(data) || !keysWithin(data, REJECTION_FIELDS)) return undefined;
  if (data.kind !== "rejected" || !(REJECTION_REASONS as readonly unknown[]).includes(data.reason)) return undefined;
  if (!REJECTION_OFFSETS.every(name => ordinalText(data[name]))) return undefined;
  const { unterminated, excerptTruncated, excerpt } = data;
  if (typeof unterminated !== "boolean" || typeof excerptTruncated !== "boolean") return undefined;
  if (typeof excerpt !== "string" || !canonicalExcerpt(excerpt)) return undefined;
  const at = (name: (typeof REJECTION_OFFSETS)[number]) => BigInt(data[name] as string);
  const start = at("sourceStart"), end = at("sourceEnd"), delimiterStart = at("delimiterStart"), delimiterEnd = at("delimiterEnd");
  if (start >= end || delimiterStart !== end || delimiterEnd < delimiterStart || unterminated !== (delimiterStart === delimiterEnd)) return undefined;
  if (at("reasonStart") < start || at("reasonEnd") > end || at("reasonStart") >= at("reasonEnd") || at("excerptStart") !== start) return undefined;
  const padding = excerpt.endsWith("==") ? 2 : excerpt.endsWith("=") ? 1 : 0;
  const size = BigInt(excerpt.length / 4 * 3 - padding), content = end - start, bound = BigInt(excerptBytes);
  if (size !== (bound < content ? bound : content) || excerptTruncated !== (size < content)) return undefined;
  if (row.sourceStart !== data.sourceStart || row.sourceEnd !== data.delimiterEnd) return undefined;
  return Object.freeze({
    reason: data.reason as RejectionReason, recordOrdinal: data.recordOrdinal as string,
    sourceStart: data.sourceStart as string, sourceEnd: data.sourceEnd as string, delimiterStart: data.delimiterStart as string, delimiterEnd: data.delimiterEnd as string,
    unterminated, reasonStart: data.reasonStart as string, reasonEnd: data.reasonEnd as string, excerpt, excerptSize: size.toString(), excerptTruncated,
  });
}

/**
 * The coverage rows of a page, when they are one ordered run of skipped records the coverage counts:
 * each a different source record no earlier than its coverage ordinal and leaving room for the
 * skipped records after it, none overlapping the one before, all within the coverage's last record
 * and byte, and the final skipped record exactly that last record and byte.
 */
function withRejections(rows: readonly DatasetRow[], coverage: DatasetCoverage): DatasetRow[] | undefined {
  if (coverage.lastOrdinal === null || coverage.through === null) return rows.length === 0 ? [] : undefined;
  const lastRecord = BigInt(coverage.lastOrdinal), through = BigInt(coverage.through), final = BigInt(coverage.records) - 1n;
  const checked: DatasetRow[] = [];
  let previous: ScanRejection | undefined;
  for (const row of rows) {
    const rejection = decodeRejection(row, coverage.excerptBytes);
    if (!rejection) return undefined;
    const ordinal = BigInt(row.ordinal), record = BigInt(rejection.recordOrdinal), delimiterEnd = BigInt(rejection.delimiterEnd);
    if (record < ordinal || lastRecord - record < final - ordinal || delimiterEnd > through) return undefined;
    if (previous && (record <= BigInt(previous.recordOrdinal) || BigInt(rejection.sourceStart) < BigInt(previous.delimiterEnd))) return undefined;
    if (ordinal === final && (record !== lastRecord || delimiterEnd !== through)) return undefined;
    checked.push(Object.freeze({ ...row, rejection }));
    previous = rejection;
  }
  return checked;
}

/**
 * The page, when it is one coherent bounded run of the selected stream: rows numbered from `first`
 * without gaps, no more than asked for, ending at `next`, within `records` — the descriptor's count
 * for outputs, the coverage's own count for skipped records — and carrying a cursor exactly when the
 * stream has more. Coverage rows must each be one skipped record that coverage accounts for.
 */
function decodePage(raw: unknown, records: string, limit: number, coverage?: DatasetCoverage): DatasetPage | undefined {
  if (!plain(raw) || !keysWithin(raw, ["first", "next", "rows", "extentExhausted", "limitedBy", "cursor"])) return undefined;
  const { first, next, extentExhausted, limitedBy, cursor } = raw;
  if (!ordinalText(first) || !ordinalText(next) || typeof extentExhausted !== "boolean") return undefined;
  if (limitedBy !== null && typeof limitedBy !== "string") return undefined;
  if (cursor !== null && (typeof cursor !== "string" || cursor.length === 0 || cursor.length > MAX_CURSOR_BYTES || !CURSOR.test(cursor))) return undefined;
  if (!Array.isArray(raw.rows) || raw.rows.length > limit) return undefined;
  const count = BigInt(records);
  const start = BigInt(first), end = BigInt(next);
  if (end !== start + BigInt(raw.rows.length) || end > count) return undefined;
  if ((cursor === null) !== extentExhausted || extentExhausted !== (end === count)) return undefined;
  const decoded: DatasetRow[] = [];
  for (const [at, item] of raw.rows.entries()) {
    const row = decodeRow(item);
    if (!row || BigInt(row.ordinal) !== start + BigInt(at)) return undefined;
    decoded.push(row);
  }
  const rows = coverage ? withRejections(decoded, coverage) : decoded;
  return rows && { first, next, rows, extentExhausted, limitedBy, cursor };
}

const COVERAGE_KEYS = ["records", "inputBytes", "lastOrdinal", "through", "excerptBytes", "segmentBytes"];

/**
 * The forensic coverage of the selected analysis tree, when it is one consistent count: no skipped
 * records and nothing else, or skipped records of at least one original byte each, a last source
 * record that leaves room for all of them, and a positive end byte no smaller than their bytes. The
 * excerpt bound is the frozen 1–4096.
 */
function decodeCoverage(raw: unknown): DatasetCoverage | undefined {
  if (!plain(raw) || !keysWithin(raw, COVERAGE_KEYS)) return undefined;
  const { records, inputBytes, lastOrdinal, through, excerptBytes, segmentBytes } = raw;
  if (!ordinalText(records) || !ordinalText(inputBytes) || !ordinalText(excerptBytes) || !ordinalText(segmentBytes)) return undefined;
  if ((lastOrdinal !== null && !ordinalText(lastOrdinal)) || (through !== null && !ordinalText(through))) return undefined;
  if (BigInt(excerptBytes) < 1n || BigInt(excerptBytes) > EXCERPT_MAX) return undefined;
  if (records === "0") {
    if (inputBytes !== "0" || lastOrdinal !== null || through !== null) return undefined;
  } else {
    const count = BigInt(records), bytes = BigInt(inputBytes);
    if (lastOrdinal === null || through === null || through === "0") return undefined;
    if (bytes < count || BigInt(lastOrdinal) < count - 1n || bytes > BigInt(through)) return undefined;
  }
  return Object.freeze({ records, inputBytes, lastOrdinal: lastOrdinal as string | null, through: through as string | null, excerptBytes, segmentBytes });
}

/**
 * A successful reply, or undefined when it is not exactly one. `expected` is the reference the
 * presented descriptor names: a reply for any other snapshot is refused rather than mixed in.
 */
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const RECORDING_KEYS = ["run", "epoch", "first", "acceptedThrough", "committedThrough", "pending", "rejected", "termination"];

/**
 * The recording coverage of the selected generation, when it is exactly one consistent account of
 * that generation: its epoch is the dataset itself, its committed interval holds exactly the
 * snapshot's records, nothing is committed beyond what was accepted, a known pending count is the
 * difference between them, and a sealed generation ended naturally or by request. `open` and `prefix`
 * are only ever read from a stored open manifest, which records no end; `interrupted` may be read from
 * that same open manifest or from a stored interrupted one that does, so it is not constrained here.
 */
function decodeRecording(raw: unknown, reference: DatasetReference, lifecycle: DatasetLifecycle): DatasetRecording | undefined {
  if (!plain(raw) || !keysWithin(raw, RECORDING_KEYS)) return undefined;
  const { run, epoch, first, acceptedThrough, committedThrough, pending, rejected, termination } = raw;
  if (typeof run !== "string" || !UUID.test(run) || typeof epoch !== "string" || epoch !== reference.dataset) return undefined;
  if (!ordinalText(first) || first === "0" || !ordinalText(acceptedThrough) || !ordinalText(committedThrough) || !ordinalText(rejected)) return undefined;
  if (pending !== null && !ordinalText(pending)) return undefined;
  if (termination !== null && !(RECORDING_TERMINATIONS as readonly unknown[]).includes(termination)) return undefined;
  const before = BigInt(first) - 1n, accepted = BigInt(acceptedThrough), committed = BigInt(committedThrough);
  if (committed < before || accepted < committed || committed - before !== BigInt(reference.records)) return undefined;
  if (pending !== null && BigInt(pending) !== accepted - committed) return undefined;
  if (lifecycle === "sealed" && termination !== "natural" && termination !== "manual") return undefined;
  if ((lifecycle === "open" || lifecycle === "prefix") && termination !== null) return undefined;
  return Object.freeze({ run, epoch, first, acceptedThrough, committedThrough, pending: pending as string | null, rejected, termination: termination as RecordingTermination | null });
}

/**
 * A read of `stream`, when the reply is exactly one. The reply names its stream and must name the one
 * asked for. A skipped-record read requires coverage and is bounded by its count; the descriptor's
 * count stays the outputs' count whichever stream is read. Recording and forensic coverage never
 * describe the same manifest.
 */
export function decodeDatasetRead(raw: unknown, expected: DatasetReference, limit?: number, stream: DatasetStream = "outputs"): DatasetRead | undefined {
  if (!plain(raw) || !keysWithin(raw, ["reference", "stream", "lifecycle", "protected", "persistence", "segmentBytes"], ["page", "recording", "coverage"])) return undefined;
  if (raw.stream !== stream) return undefined;
  const reference = decodeDatasetReference(raw.reference);
  if (!reference || !sameReference(reference, expected)) return undefined;
  const lifecycle = raw.lifecycle;
  if (typeof lifecycle !== "string" || !(DATASET_LIFECYCLES as readonly string[]).includes(lifecycle)) return undefined;
  if (typeof raw.protected !== "boolean" || typeof raw.persistence !== "string" || !ordinalText(raw.segmentBytes)) return undefined;
  let recording: DatasetRecording | undefined;
  if (Object.hasOwn(raw, "recording")) {
    recording = decodeRecording(raw.recording, reference, lifecycle as DatasetLifecycle);
    if (!recording) return undefined;
  }
  let coverage: DatasetCoverage | undefined;
  if (Object.hasOwn(raw, "coverage")) {
    coverage = decodeCoverage(raw.coverage);
    if (!coverage) return undefined;
  }
  if (recording && coverage) return undefined;
  if (stream === "coverage" && !coverage) return undefined;
  const base = { reference, stream, lifecycle: lifecycle as DatasetLifecycle, protected: raw.protected, persistence: raw.persistence, segmentBytes: raw.segmentBytes,
    ...(recording ? { recording } : {}), ...(coverage ? { coverage } : {}) };
  if (limit === undefined) return Object.hasOwn(raw, "page") ? undefined : base;
  const page = stream === "coverage" ? decodePage(raw.page, coverage!.records, limit, coverage) : decodePage(raw.page, reference.records, limit);
  return page && { ...base, page };
}

export function sameReference(left: DatasetReference, right: DatasetReference): boolean {
  return (Object.keys(left) as (keyof DatasetReference)[]).every(key => left[key] === right[key]);
}

/**
 * Whether `next` is a committed extension of `previous` as far as identity can show: the same store,
 * dataset, schema and authorization, a generation and record count never lower, and exactly the
 * same snapshot at the same generation. The server proves ancestry; this refuses anything else.
 */
export function extendsReference(next: DatasetReference, previous: DatasetReference): boolean {
  return next.store === previous.store && next.dataset === previous.dataset
    && next.schemaDigest === previous.schemaDigest && next.authorizationGeneration === previous.authorizationGeneration
    && BigInt(next.generation) >= BigInt(previous.generation) && BigInt(next.records) >= BigInt(previous.records)
    && (next.generation !== previous.generation || sameReference(next, previous));
}

/**
 * A head inspection, when it is exactly one and names a committed extension of both the stored
 * result's own snapshot and the newest head already shown. Anything else — another dataset, schema
 * or authorization, a lower generation or count, a page — is refused whole.
 */
export function decodeDatasetHead(raw: unknown, original: DatasetReference, shown: DatasetReference): DatasetRead | undefined {
  if (!plain(raw)) return undefined;
  const head = decodeDatasetReference(raw.reference);
  if (!head || !extendsReference(head, original) || !extendsReference(head, shown)) return undefined;
  return decodeDatasetRead(raw, head);
}

const FAILURES: Readonly<Record<string, DatasetFailureKind>> = {
  DATASET_READ_BUSY: "busy", DATASET_RETIREMENT_BUSY: "busy",
  DATASET_WITHDRAWN: "withdrawn", DATASET_ACCESS_REFUSED: "withdrawn",
  DATASET_MISSING: "missing",
  DATASET_READ_LIMIT: "limit", DATASET_REPLY_LIMIT: "limit",
  DATASET_SESSION_CHANGED: "session", DATASET_SESSION_ENDED: "session",
  DATASET_CONTINUITY_CHANGED: "continuity",
  DATASET_INVALID: "invalid",
  DATASET_READ_FAILED: "failed", DATASET_ENCODING_FAILED: "failed",
};

/** The failure a refused read reports. Withdrawal and access refusal are recognised by status as well as code. */
export async function datasetFailure(response: Response): Promise<DatasetReadError> {
  let code = `HTTP_${response.status}`, message = "The dataset read failed without an explanation.", retryable = false;
  try {
    const body = await readExactJson(response) as { error?: unknown };
    if (plain(body?.error) && typeof body.error.code === "string" && typeof body.error.message === "string" && typeof body.error.retryable === "boolean") {
      ({ code, message, retryable } = body.error as { code: string; message: string; retryable: boolean });
    }
  } catch { /* An unreadable body keeps the status explanation. */ }
  const kind = response.status === 410 || response.status === 403 ? "withdrawn"
    : FAILURES[code] ?? (response.status === 404 ? "missing" : response.status === 413 ? "limit" : response.status === 409 ? "session" : response.status >= 500 ? "failed" : "invalid");
  // A withdrawn answer never carries a reason that could describe the withdrawn data.
  return new DatasetReadError(kind, response.status, code, kind === "withdrawn" ? "Access withdrawn" : message, kind === "busy" && retryable);
}

/** The ordinal one page before `first`, never below zero. */
export function previousStart(first: string, limit: number): string {
  const start = BigInt(first) - BigInt(limit);
  return (start < 0n ? 0n : start).toString();
}

/** Records after `next` in a snapshot of `records`, as an exact decimal. */
export function recordsAfter(next: string, records: string): string {
  const left = BigInt(records) - BigInt(next);
  return (left < 0n ? 0n : left).toString();
}

/** The ordinal of the last record, or undefined for an empty snapshot. */
export function lastOrdinal(records: string): string | undefined {
  return records === "0" ? undefined : (BigInt(records) - 1n).toString();
}

/** Whether a typed go-to ordinal names a record of the snapshot. */
export function withinSnapshot(ordinal: string, records: string): boolean {
  return canonicalOrdinal(ordinal) && BigInt(ordinal) < BigInt(records);
}
