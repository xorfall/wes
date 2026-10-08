/**
 * The receipt a finite record analysis returns inside its `ScanResult`, read as the engine typed it.
 *
 * Read only from a value whose type is the engine's `ScanResult` record with a `ScanReceipt` field
 * and, when metadata travelled with it, whose contract is `ScanResult`; then every receipt field must
 * be present with its declared encoding (Options are `{kind:"none"}` / `{kind:"some",value}`). Any
 * mismatch yields no summary at all — the generic value view still shows the data — rather than a
 * summary guessed from a similar shape. Nothing here changes the value or its metadata.
 */
import { isExactNumber } from "../exact-json";
import { counterText } from "../protocol-decode";
import type { StoredValue, TypeShape } from "../protocol";
import type { MonoRole, Segment } from "./MonoLine";
import { grouped } from "./record-progress";

export interface ScanLimits {
  readonly work: string; readonly inputCharge: string; readonly inputRecords: string;
  readonly heldCharge: string; readonly outputCharge: string; readonly outputRecords: string;
  readonly recordWork: string; readonly recordCharge: string; readonly stateCharge: string;
  readonly contextCharge: string; readonly durationMs: string;
}
export interface ScanReceipt {
  readonly status: "complete" | "stopped" | "cancelled";
  readonly position: string; readonly readPosition: string; readonly extent: string;
  readonly positionUnit: "bytes" | "records";
  readonly inputChargeUnit: "raw_bytes" | "logical_charge";
  readonly inputCharge: string; readonly inputRecords: string;
  readonly outputCharge: string; readonly outputRecords: string;
  readonly work: string; readonly workAllowance: string;
  readonly heldCharge: string; readonly highWaterCharge: string;
  readonly finishApplied: boolean;
  /** `undefined` is the engine's none: whether the producer completed is not known. */
  readonly sourceComplete: boolean | undefined;
  readonly failureCode?: string; readonly failureMessage?: string; readonly exhausted?: string;
  readonly rejectedStart?: string; readonly rejectedEnd?: string;
  readonly analysisId: string; readonly transitionRevision: string; readonly finishRevision?: string;
  readonly profile: string; readonly profileRevision: string;
  readonly sourceNode?: string; readonly sourceRun?: string; readonly sourceRevision?: string;
  readonly sourcePort?: string; readonly sourcePath: readonly string[];
  readonly durableResume: boolean;
  readonly limits: ScanLimits;
}

const FIELDS = ["status", "position", "readPosition", "extent", "positionUnit", "inputChargeUnit", "inputCharge", "inputRecords",
  "outputCharge", "outputRecords", "work", "workAllowance", "heldCharge", "highWaterCharge", "finishApplied", "sourceComplete",
  "failureCode", "failureMessage", "exhausted", "rejectedStart", "rejectedEnd", "analysisId", "transitionRevision", "finishRevision",
  "profile", "profileRevision", "sourceNode", "sourceRun", "sourceRevision", "sourcePort", "sourcePath", "durableResume", "limits"] as const;
const LIMITS = ["work", "inputCharge", "inputRecords", "heldCharge", "outputCharge", "outputRecords", "recordWork", "recordCharge",
  "stateCharge", "contextCharge", "durationMs"] as const;
/** The engine's stable dimension names (scan/ledger.rs), said in words. An unlisted name is shown as written. */
const DIMENSIONS: Readonly<Record<string, string>> = {
  work: "work", input_charge: "input charge", input_records: "input records", held_charge: "held charge",
  output_charge: "output charge", output_records: "output records", aggregate_charge: "aggregate held charge",
};
/** The failure message is the engine's bounded text; the summary bounds it again for its rows. */
const MESSAGE_CHARS = 512;

class Mismatch extends Error {}
const no = (): never => { throw new Mismatch(); };
const object = (value: unknown): Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value) && !isExactNumber(value) ? value as Record<string, unknown> : no();
const exactKeys = (value: Record<string, unknown>, keys: readonly string[]) => {
  if (Object.keys(value).length !== keys.length || keys.some(key => !(key in value))) no();
};
const int = (value: unknown): string => counterText(value) ?? no();
const text = (value: unknown): string => typeof value === "string" ? value : no();
const bool = (value: unknown): boolean => typeof value === "boolean" ? value : no();
const oneOf = <T extends string>(value: unknown, allowed: readonly T[]): T => (allowed as readonly unknown[]).includes(value) ? value as T : no();
/** `{kind:"none"}` or `{kind:"some", value}`, exactly. */
function option<T>(value: unknown, read: (value: unknown) => T): T | undefined {
  const wrapped = object(value);
  if (wrapped.kind === "none" && Object.keys(wrapped).length === 1) return undefined;
  if (wrapped.kind === "some" && Object.keys(wrapped).length === 2 && "value" in wrapped) return read(wrapped.value);
  return no();
}

const named = (shape: TypeShape | undefined, name: string): shape is Extract<TypeShape, { kind: "record" }> =>
  shape?.kind === "record" && shape.name === name;

/** The receipt of a `ScanResult` value, or nothing when the value is not exactly one. */
export function scanReceiptOf(value: StoredValue | undefined): ScanReceipt | undefined {
  if (!value || !named(value.type, "ScanResult")) return undefined;
  if (!named(value.type.fields.find(field => field.name === "receipt")?.type, "ScanReceipt")) return undefined;
  if (value.meta && value.meta.contract.name !== "ScanResult") return undefined;
  try {
    const raw = object(object(value.data).receipt);
    exactKeys(raw, FIELDS);
    const rawLimits = object(raw.limits);
    exactKeys(rawLimits, LIMITS);
    const limits = Object.fromEntries(LIMITS.map(key => [key, int(rawLimits[key])])) as unknown as ScanLimits;
    const sourcePath = Array.isArray(raw.sourcePath) ? raw.sourcePath.map(text) : no();
    return {
      status: oneOf(raw.status, ["complete", "stopped", "cancelled"] as const),
      position: int(raw.position), readPosition: int(raw.readPosition), extent: int(raw.extent),
      positionUnit: oneOf(raw.positionUnit, ["bytes", "records"] as const),
      inputChargeUnit: oneOf(raw.inputChargeUnit, ["raw_bytes", "logical_charge"] as const),
      inputCharge: int(raw.inputCharge), inputRecords: int(raw.inputRecords),
      outputCharge: int(raw.outputCharge), outputRecords: int(raw.outputRecords),
      work: int(raw.work), workAllowance: int(raw.workAllowance),
      heldCharge: int(raw.heldCharge), highWaterCharge: int(raw.highWaterCharge),
      finishApplied: bool(raw.finishApplied),
      sourceComplete: option(raw.sourceComplete, bool),
      failureCode: option(raw.failureCode, text), failureMessage: option(raw.failureMessage, text),
      exhausted: option(raw.exhausted, text),
      rejectedStart: option(raw.rejectedStart, int), rejectedEnd: option(raw.rejectedEnd, int),
      analysisId: text(raw.analysisId), transitionRevision: text(raw.transitionRevision),
      finishRevision: option(raw.finishRevision, text),
      profile: text(raw.profile), profileRevision: text(raw.profileRevision),
      sourceNode: option(raw.sourceNode, text), sourceRun: option(raw.sourceRun, text),
      sourceRevision: option(raw.sourceRevision, int), sourcePort: option(raw.sourcePort, text),
      sourcePath, durableResume: bool(raw.durableResume), limits,
    };
  } catch (error) {
    if (error instanceof Mismatch) return undefined;
    throw error;
  }
}

const SEP: Segment = { text: " · ", role: "mono-faint" };
const dim = (text: string): Segment => ({ text, role: "mono-dim" });
const ink = (text: string, role: MonoRole = "mono-ink"): Segment => ({ text, role });
const n = (digits: string) => ink(grouped(digits));

/** One line: what the run ended as, and how far it got. For a disclosure's own summary. */
export function receiptHeadline(receipt: ScanReceipt): Segment[] {
  return [
    dim("analysis receipt"), SEP, ink(receipt.status, receipt.status === "complete" ? "mono-ok" : "mono-warn"), SEP,
    dim("committed through "), n(receipt.position), dim(` of ${grouped(receipt.extent)} ${receipt.positionUnit}`),
  ];
}

/**
 * The receipt as bounded rows, in reading order: outcome, coverage, counts, failure, identities,
 * captured limits. Absent options are said as unknown or none, never as zero or as success.
 */
export function receiptRows(receipt: ScanReceipt): Segment[][] {
  const rows: Segment[][] = [];
  const unit = receipt.positionUnit;
  rows.push([dim("outcome "), ink(receipt.status, receipt.status === "complete" ? "mono-ok" : "mono-warn"), SEP,
    dim("finish "), ink(receipt.finishApplied ? "applied" : "not applied")]);
  rows.push([dim("selected extent "), n(receipt.extent), dim(` ${unit}`), SEP, dim("producer completion "),
    receipt.sourceComplete === undefined ? ink("unknown", "mono-warn") : ink(receipt.sourceComplete ? "complete" : "incomplete")]);
  const pending = BigInt(receipt.readPosition) > BigInt(receipt.position);
  rows.push([dim("committed through "), n(receipt.position), SEP, dim("read through "), n(receipt.readPosition), dim(` ${unit}`),
    ...(pending ? [dim(" · read, not committed")] : [])]);
  rows.push([n(receipt.inputRecords), dim(" records in"), SEP, dim(receipt.inputChargeUnit === "raw_bytes" ? "input raw bytes " : "input logical charge "),
    n(receipt.inputCharge), SEP, n(receipt.outputRecords), dim(" outputs"), SEP, dim("output logical charge "), n(receipt.outputCharge)]);
  rows.push([dim("work "), n(receipt.work), dim(" used"), SEP, n(receipt.workAllowance), dim(" earned allowance"), SEP,
    dim("absolute cap "), n(receipt.limits.work)]);
  rows.push([dim("logical held charge "), n(receipt.heldCharge), SEP, dim("high-water "), n(receipt.highWaterCharge), SEP,
    dim("cap "), n(receipt.limits.heldCharge), dim(" · not memory use or stored bytes")]);
  if (receipt.failureCode !== undefined || receipt.failureMessage !== undefined) {
    const message = receipt.failureMessage ?? "";
    rows.push([dim("failure "), ...(receipt.failureCode ? [ink(receipt.failureCode, "mono-bad"), SEP] : []),
      ink(message.length > MESSAGE_CHARS ? `${message.slice(0, MESSAGE_CHARS)}…` : message, "mono-bad"),
      ...(message.length > MESSAGE_CHARS ? [dim(" (shortened here; the whole text is in the value)")] : [])]);
  }
  if (receipt.exhausted !== undefined) rows.push([dim("limit reached "), ink(DIMENSIONS[receipt.exhausted] ?? receipt.exhausted, "mono-warn")]);
  if (receipt.rejectedStart !== undefined || receipt.rejectedEnd !== undefined) {
    rows.push([dim("rejected original bytes "), ink(receipt.rejectedStart === undefined ? "?" : grouped(receipt.rejectedStart)),
      dim("–"), ink(receipt.rejectedEnd === undefined ? "?" : grouped(receipt.rejectedEnd)), dim(" (half-open)")]);
  }
  rows.push([dim("analysis "), ink(receipt.analysisId, "mono-ref")]);
  rows.push(receipt.sourceNode === undefined
    ? [dim("source "), ink("literal input · no producing node")]
    : [dim("source "), ink(receipt.sourceNode, "mono-ref"),
      ...(receipt.sourcePort !== undefined ? [dim(` ${receipt.sourcePort}`)] : []),
      ...(receipt.sourcePath.length ? [dim(` .${receipt.sourcePath.join(".")}`)] : []),
      SEP, dim("run "), ink(receipt.sourceRun ?? "unknown"), SEP, dim("revision "), ink(receipt.sourceRevision === undefined ? "unknown" : grouped(receipt.sourceRevision))]);
  rows.push([dim("profile "), ink(receipt.profile), SEP, ink(receipt.profileRevision, "mono-dim")]);
  rows.push([dim("transition "), ink(receipt.transitionRevision, "mono-dim")]);
  rows.push([dim("finish definition "), receipt.finishRevision === undefined ? ink("none") : ink(receipt.finishRevision, "mono-dim")]);
  const l = receipt.limits;
  rows.push([dim("captured limits · input "), n(l.inputRecords), dim(" records, "), n(l.inputCharge), dim(" charge"), SEP,
    dim("output "), n(l.outputRecords), dim(" records, "), n(l.outputCharge), dim(" charge")]);
  rows.push([dim("per record · work "), n(l.recordWork), SEP, dim("charge "), n(l.recordCharge), SEP,
    dim("state "), n(l.stateCharge), SEP, dim("context "), n(l.contextCharge), SEP, dim("duration "), n(l.durationMs), dim(" ms")]);
  rows.push([dim("durable checkpoint "), ink(receipt.durableResume ? "yes" : "none")]);
  return rows;
}
