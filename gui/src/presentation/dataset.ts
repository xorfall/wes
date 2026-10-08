/**
 * A Dataset value as the browser receives it: `{kind: "dataset", reference}`, where the reference
 * identifies one committed generation of a stored extent. It is a descriptor, not the records and
 * not a List; nothing here reads, pages, counts or collects records. The descriptor grants no
 * access: reading records is a separate request the server authorizes on its own.
 *
 * The decoder accepts exactly what the engine writes and refuses everything else as a whole.
 * Numbers arrive as canonical decimal strings so that a 64-bit count stays exact; they stay
 * strings here and are only grouped for display.
 */
import type { TypeShape } from "../protocol";
import { describeType } from "../protocol";
import { groupedDigits } from "./format";
import type { Run } from "./types";

/** The descriptor fields, in the order the surface shows them. */
export const DATASET_REFERENCE_FIELDS = [
  "records", "generation", "dataset", "store", "manifest",
  "manifestDigest", "manifestBytes", "schemaDigest", "authorizationGeneration",
] as const;
export type DatasetReferenceField = (typeof DATASET_REFERENCE_FIELDS)[number];
export type DatasetReference = { readonly [K in DatasetReferenceField]: string };

const IDENTITIES: readonly DatasetReferenceField[] = ["store", "dataset", "manifest"];
const DIGESTS: readonly DatasetReferenceField[] = ["manifestDigest", "schemaDigest"];
/** Fields holding unsigned 64-bit counts, written as canonical decimal strings. */
export const DATASET_COUNTS: ReadonlySet<DatasetReferenceField> = new Set(["generation", "manifestBytes", "records", "authorizationGeneration"]);
/** Counts that identify something and therefore start at one; an empty dataset has zero records. */
const POSITIVE: ReadonlySet<DatasetReferenceField> = new Set(["generation", "manifestBytes", "authorizationGeneration"]);

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const DIGEST = /^sha256:[0-9a-f]{64}$/;
const DECIMAL = /^(?:0|[1-9][0-9]{0,19})$/;
const U64_MAX = 18446744073709551615n;
const ENVELOPE_KEYS = ["kind", "reference"] as const;

function plainObject(value: unknown): value is Readonly<Record<string, unknown>> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

/** Whether the object's own enumerable keys are exactly `keys`, stopping at the first surplus key. */
function hasExactly(value: Readonly<Record<string, unknown>>, keys: readonly string[]): boolean {
  let count = 0;
  for (const key in value) {
    if (!Object.hasOwn(value, key)) continue;
    if (++count > keys.length || !keys.includes(key)) return false;
  }
  return count === keys.length;
}

function canonicalCount(text: string, positive: boolean): boolean {
  return DECIMAL.test(text) && BigInt(text) <= U64_MAX && (!positive || text !== "0");
}

/** The reference of a committed dataset, or undefined when it is not exactly one. */
export function decodeDatasetReference(raw: unknown): DatasetReference | undefined {
  if (!plainObject(raw) || !hasExactly(raw, DATASET_REFERENCE_FIELDS)) return undefined;
  for (const name of DATASET_REFERENCE_FIELDS) if (typeof raw[name] !== "string") return undefined;
  const text = raw as Readonly<Record<DatasetReferenceField, string>>;
  if (!IDENTITIES.every(name => UUID.test(text[name]))) return undefined;
  if (!DIGESTS.every(name => DIGEST.test(text[name]))) return undefined;
  for (const name of DATASET_COUNTS) if (!canonicalCount(text[name], POSITIVE.has(name))) return undefined;
  const reference = {} as Record<DatasetReferenceField, string>;
  for (const name of DATASET_REFERENCE_FIELDS) reference[name] = text[name];
  return Object.freeze(reference);
}

/** The reference a Dataset value's data carries, or undefined when the envelope is not exact. */
export function decodeDatasetData(raw: unknown): DatasetReference | undefined {
  if (!plainObject(raw) || !hasExactly(raw, ENVELOPE_KEYS) || raw.kind !== "dataset") return undefined;
  return decodeDatasetReference(raw.reference);
}

/** `1 record`, `12 345 records`: the committed count, exact at any size. */
export function recordsText(reference: DatasetReference): string {
  return `${groupedDigits(reference.records)} ${reference.records === "1" ? "record" : "records"}`;
}

/** What the result header says about a Dataset: its committed count, or that the descriptor is invalid. */
export function datasetFacts(data: unknown): readonly Run[] {
  const reference = decodeDatasetData(data);
  return reference ? [{ text: recordsText(reference), tone: "dim" }] : [{ text: "invalid descriptor", tone: "warn" }];
}

/** A Dataset on one line, as a table cell or a closed nested value: `Dataset<Row> · 12 records`. */
export function datasetSummary(type: TypeShape, data: unknown): string {
  const reference = decodeDatasetData(data);
  return `${describeType(type)} · ${reference ? recordsText(reference) : "invalid descriptor"}`;
}

/**
 * A presented Dataset the surface may page: the descriptor it shows and, when the server can
 * address it, the JSON Pointer that names it inside the stored value.
 */
export interface DatasetAnchor {
  readonly reference: DatasetReference;
  /** The Dataset's type, `Dataset<T>`. */
  readonly type: Extract<TypeShape, { kind: "dataset" }>;
  /** The pointer a read selects it by; absent when it sits behind a value the server cannot traverse. */
  readonly select?: string;
}

const LIST_INDEX = /^(?:0|[1-9][0-9]*)$/;

/**
 * The pointer that selects the Dataset at `pointer` in a value of type `root`, or undefined when a
 * read cannot reach it. A read may only step through record fields and list indices and must end
 * on the Dataset itself: an Option on the way or around it is not traversed, so a Dataset behind
 * one is shown without paging rather than read through a guessed path.
 */
export function datasetSelect(root: TypeShape, pointer: string): string | undefined {
  if (pointer !== "" && !pointer.startsWith("/")) return undefined;
  let shape = root;
  for (const raw of pointer === "" ? [] : pointer.slice(1).split("/")) {
    if (/~[^01]|~$/.test(raw)) return undefined;
    const segment = raw.replace(/~1/g, "/").replace(/~0/g, "~");
    if (shape.kind === "record") {
      const field = shape.fields.find(it => it.name === segment);
      if (!field) return undefined;
      shape = field.type;
    } else if (shape.kind === "list") {
      if (!LIST_INDEX.test(segment)) return undefined;
      shape = shape.element;
    } else return undefined;
  }
  return shape.kind === "dataset" ? pointer : undefined;
}
