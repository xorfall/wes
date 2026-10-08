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
import { nativeIntText } from "../protocol-decode";
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
  /** `work` is the conservatively charged total; `measuredWork` is what was actually measured. */
  readonly work: string; readonly measuredWork: string; readonly workAllowance: string;
  /** The acknowledged prepaid work grant: a reservation, not lost work. `undefined` without a durable checkpoint. */
  readonly outstandingWork?: string;
  /** Elapsed time including conservatively charged interrupted intervals; never an estimate of what remains. */
  readonly durationChargedMs: string;
  /** The acknowledged prepaid interval. `undefined` without a durable checkpoint. */
  readonly durationOutstandingMs?: string;
  /** The durable checkpoint attempt and its actual predecessor, as the engine recorded them. */
  readonly attempt?: string; readonly previousAttempt?: string;
  /**
   * The immutable bounds receipt of a durable checkpoint: its digest, the attempt that issued it and
   * the digest of the receipt it succeeded. They identify limits and grant nothing. `undefined` without one.
   */
  readonly budgetDigest?: string; readonly budgetIssuedAttempt?: string; readonly budgetPrevious?: string;
  /** Work credit explicitly authorized by continuations so far, and the latest such delta. Not input-earned. */
  readonly authorizedWork: string; readonly workGrant: string;
  /** Measured lateness past the duration limit; a cooperative limit, not preemption. */
  readonly durationOverrunMs?: string;
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
  /** The engine's offer of ordinary resume; independent of whether a durable `attempt` exists. */
  readonly durableResume: boolean;
  readonly limits: ScanLimits;
}

const FIELDS = ["status", "position", "readPosition", "extent", "positionUnit", "inputChargeUnit", "inputCharge", "inputRecords",
  "outputCharge", "outputRecords", "work", "measuredWork", "workAllowance", "outstandingWork", "durationChargedMs", "durationOutstandingMs",
  "heldCharge", "highWaterCharge", "finishApplied", "sourceComplete",
  "failureCode", "failureMessage", "exhausted", "rejectedStart", "rejectedEnd", "analysisId", "attempt", "previousAttempt",
  "budgetDigest", "budgetIssuedAttempt", "budgetPrevious", "authorizedWork", "workGrant", "durationOverrunMs",
  "transitionRevision", "finishRevision",
  "profile", "profileRevision", "sourceNode", "sourceRun", "sourceRevision", "sourcePort", "sourcePath", "durableResume", "limits"] as const;
const LIMITS = ["work", "inputCharge", "inputRecords", "heldCharge", "outputCharge", "outputRecords", "recordWork", "recordCharge",
  "stateCharge", "contextCharge", "durationMs"] as const;
/** The engine's stable dimension names (scan/ledger.rs), said in words. An unlisted name is shown as written. */
const DIMENSIONS: Readonly<Record<string, string>> = {
  work: "work absolute cap", work_allowance: "work allowance", duration: "duration",
  input_charge: "input charge", input_records: "input records", held_charge: "held charge", output_charge: "output charge", output_records: "output records", aggregate_charge: "aggregate held charge",
  record_work: "per-record work", record_charge: "per-record charge", record_outputs: "per-record outputs",
};
export const dimensionWords = (name: string): string => DIMENSIONS[name] ?? name;
/** The failure message is the engine's bounded text; the summary bounds it again for its rows. */
const MESSAGE_CHARS = 512;

class Mismatch extends Error {}
const no = (): never => { throw new Mismatch(); };
const object = (value: unknown): Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value) && !isExactNumber(value) ? value as Record<string, unknown> : no();
const exactKeys = (value: Record<string, unknown>, keys: readonly string[]) => {
  if (Object.keys(value).length !== keys.length || keys.some(key => !(key in value))) no();
};
/** Every receipt count is a native `Int`: non-negative and within i64, never a wider transport counter. */
const int = (value: unknown): string => nativeIntText(value) ?? no();
const text = (value: unknown): string => typeof value === "string" ? value : no();
/** A canonical `sha256:` digest, the only spelling the engine writes for a bounds receipt. */
const digest = (value: unknown): string => typeof value === "string" && /^sha256:[0-9a-f]{64}$/.test(value) ? value : no();
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
    const attempt = option(raw.attempt, text);
    const budgetDigest = option(raw.budgetDigest, digest), budgetIssuedAttempt = option(raw.budgetIssuedAttempt, text);
    const budgetPrevious = option(raw.budgetPrevious, digest);
    const authorizedWork = int(raw.authorizedWork), workGrant = int(raw.workGrant);
    // The engine's own receipt laws: a bounds receipt exists exactly with a durable attempt, a first
    // receipt carries no explicit credit, and the latest grant is part of the cumulative credit.
    const durable = attempt !== undefined;
    if ((budgetDigest !== undefined) !== durable || (budgetIssuedAttempt !== undefined) !== durable) no();
    if (budgetPrevious !== undefined && !durable) no();
    if (budgetPrevious === undefined && (authorizedWork !== "0" || workGrant !== "0")) no();
    if (BigInt(workGrant) > BigInt(authorizedWork)) no();
    return {
      status: oneOf(raw.status, ["complete", "stopped", "cancelled"] as const),
      position: int(raw.position), readPosition: int(raw.readPosition), extent: int(raw.extent),
      positionUnit: oneOf(raw.positionUnit, ["bytes", "records"] as const),
      inputChargeUnit: oneOf(raw.inputChargeUnit, ["raw_bytes", "logical_charge"] as const),
      inputCharge: int(raw.inputCharge), inputRecords: int(raw.inputRecords),
      outputCharge: int(raw.outputCharge), outputRecords: int(raw.outputRecords),
      work: int(raw.work), measuredWork: int(raw.measuredWork), workAllowance: int(raw.workAllowance),
      outstandingWork: option(raw.outstandingWork, int),
      durationChargedMs: int(raw.durationChargedMs), durationOutstandingMs: option(raw.durationOutstandingMs, int),
      attempt, previousAttempt: option(raw.previousAttempt, text),
      budgetDigest, budgetIssuedAttempt, budgetPrevious, authorizedWork, workGrant,
      durationOverrunMs: option(raw.durationOverrunMs, int),
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
/** An exact count the engine may not have: its none is said as none, never as zero. */
const optional = (digits: string | undefined) => digits === undefined ? ink("none") : n(digits);

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
  rows.push([dim("work charged "), n(receipt.work), SEP, dim("measured "), n(receipt.measuredWork), SEP,
    dim("prepaid reservation "), optional(receipt.outstandingWork)]);
  // Once a continuation has authorized work explicitly, the allowance is no longer all input-earned.
  // The two parts are not separated by subtraction: the engine clips the allowance to the work cap.
  rows.push(receipt.authorizedWork === "0"
    ? [dim("earned allowance "), n(receipt.workAllowance), SEP, dim("absolute cap "), n(receipt.limits.work)]
    : [dim("work allowance "), n(receipt.workAllowance), dim(" (earned and explicitly authorized)"), SEP, dim("absolute cap "), n(receipt.limits.work)]);
  if (receipt.authorizedWork !== "0" || receipt.budgetPrevious !== undefined)
    rows.push([dim("explicitly authorized work "), n(receipt.authorizedWork), dim(" in all"), SEP, dim("latest grant "), n(receipt.workGrant)]);
  rows.push([dim("duration charged "), n(receipt.durationChargedMs), dim(" ms elapsed"), SEP, dim("prepaid reservation "),
    ...(receipt.durationOutstandingMs === undefined ? [ink("none")] : [n(receipt.durationOutstandingMs), dim(" ms")]), SEP,
    dim("limit "), n(receipt.limits.durationMs), dim(" ms")]);
  rows.push([dim("past the duration limit "), ...(receipt.durationOverrunMs === undefined ? [ink("none")]
    : [n(receipt.durationOverrunMs), dim(" ms measured")]), dim(" · a cooperative limit, not preemption")]);
  rows.push([dim("logical held charge "), n(receipt.heldCharge), SEP, dim("high-water "), n(receipt.highWaterCharge), SEP,
    dim("cap "), n(receipt.limits.heldCharge), dim(" · not memory use or stored bytes")]);
  if (receipt.failureCode !== undefined || receipt.failureMessage !== undefined) {
    const message = receipt.failureMessage ?? "";
    rows.push([dim("failure "), ...(receipt.failureCode ? [ink(receipt.failureCode, "mono-bad"), SEP] : []),
      ink(message.length > MESSAGE_CHARS ? `${message.slice(0, MESSAGE_CHARS)}…` : message, "mono-bad"),
      ...(message.length > MESSAGE_CHARS ? [dim(" (shortened here; the whole text is in the value)")] : [])]);
  }
  if (receipt.exhausted !== undefined) rows.push([dim("limit reached "), ink(dimensionWords(receipt.exhausted), "mono-warn")]);
  if (receipt.rejectedStart !== undefined || receipt.rejectedEnd !== undefined) {
    rows.push([dim("rejected original bytes "), ink(receipt.rejectedStart === undefined ? "?" : grouped(receipt.rejectedStart)),
      dim("–"), ink(receipt.rejectedEnd === undefined ? "?" : grouped(receipt.rejectedEnd)), dim(" (half-open)")]);
  }
  rows.push([dim("analysis "), ink(receipt.analysisId, "mono-ref")]);
  // Each identity as recorded; a missing predecessor is said as none, never inferred from the attempt.
  rows.push([dim("checkpoint attempt "), receipt.attempt === undefined ? ink("none") : ink(receipt.attempt, "mono-dim"), SEP,
    dim("previous attempt "), receipt.previousAttempt === undefined ? ink("none") : ink(receipt.previousAttempt, "mono-dim")]);
  // The bounds receipt identifies limits only; holding its digest grants nothing.
  rows.push([dim("bounds receipt "), receipt.budgetDigest === undefined ? ink("none") : ink(receipt.budgetDigest, "mono-dim"), SEP,
    dim("issued by attempt "), receipt.budgetIssuedAttempt === undefined ? ink("none") : ink(receipt.budgetIssuedAttempt, "mono-dim"), SEP,
    dim("previous bounds "), receipt.budgetPrevious === undefined ? ink("none") : ink(receipt.budgetPrevious, "mono-dim")]);
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
    dim("state "), n(l.stateCharge), SEP, dim("context "), n(l.contextCharge)]);
  // Whether a durable attempt exists, not whether resuming is offered: a deterministic stop keeps one.
  rows.push([dim("durable checkpoint "), ink(receipt.attempt !== undefined ? "yes" : "none")]);
  return rows;
}
