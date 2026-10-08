/**
 * The engine's read-only continuation review of one owned analysis (`:scan continuation`), read
 * exactly as the engine typed it, and the two commands a person may prepare from it.
 *
 * Read only from a value whose type is the native `ScanContinuationPreview` record, with its
 * `ScanContinuationBound` rows and `ScanContinuationFrozen` configuration, and — when metadata
 * travelled with it — whose contract is `ScanContinuationPreview`. Every field must be present with
 * its declared encoding; any mismatch yields no review at all and the generic value view stays. The
 * engine's `canResume`/`canContinue` and their closed reasons are the only availability facts: the
 * client never derives either from the counters.
 *
 * The review grants nothing. Its basis binds the manifest, the checkpoint attempt, the bounds receipt,
 * the active settings and all six requested totals, and only guards a later `:scan continue` of exactly
 * those totals, which the engine checks again against the latest checkpoint, the active ceilings, the
 * source and ownership. Changed totals need a new review and its new basis. A copy of this value is
 * not authority, and nothing here runs, reserves, reconciles or refreshes anything.
 *
 * Ordinary resume (`canResume`) runs under the analysis's latest granted bounds — the current totals,
 * which a successful continuation may have raised — never necessarily those of its first attempt.
 */
import { isExactNumber } from "../exact-json";
import { nativeIntText } from "../protocol-decode";
import type { StoredValue, TypeShape } from "../protocol";
import { REFERABLE_NODE } from "./dataset-management";

/** The engine's six literal cumulative totals, in its order. Also the command's parameter names. */
export const TOTALS = ["work", "input", "records", "output", "outputs", "duration"] as const;
export type TotalName = typeof TOTALS[number];
export type Totals = Readonly<Record<TotalName, string>>;

export type BoundStatus = "above_ceiling" | "lowered" | "unchanged" | "raised";
export interface ContinuationBound {
  readonly key: TotalName;
  /** The analysis's current total, from its latest bounds receipt; ordinary resume keeps it. */
  readonly current: string;
  /** The ceiling captured when the current total was issued: history, not today's policy. */
  readonly issuanceCeiling: string;
  /** The ceiling of the operating policy read for this review. */
  readonly activeCeiling: string;
  readonly requested: string;
  readonly status: BoundStatus;
}
export interface ContinuationFrozen {
  readonly stepRevision: string; readonly finishRevision?: string; readonly profileRevision: string; readonly sourceDigest: string;
  readonly memory: string; readonly recordWork: string; readonly scratch: string; readonly recordOutputs: string;
  readonly startup: string; readonly rate: string;
}
/** The engine's closed refusal names, in its order. A stop at a cumulative total is not a deterministic stop. */
export const REASONS = ["not_latest", "active_writer", "finished", "deterministic_stop", "cumulative_stop", "incomplete_source", "lowered",
  "above_ceiling", "frozen_above_ceiling", "unchanged", "ineffective_raise", "no_headroom"] as const;
export type ContinuationReason = typeof REASONS[number];
export const LIFECYCLES = ["open", "sealed", "cancelled", "interrupted", "incomplete", "prefix"] as const;

export interface ContinuationPreview {
  /** The graph node of the original owned analysis, as the engine named it; never parsed from command text. */
  readonly node: string;
  readonly analysis: string; readonly run: string; readonly attempt: string;
  readonly basis: string; readonly budgetDigest: string; readonly budgetIssuedAttempt: string;
  readonly latest: boolean; readonly activeWriter: boolean;
  readonly lifecycle: typeof LIFECYCLES[number];
  readonly stop?: string;
  readonly canResume: boolean; readonly resumeReason?: ContinuationReason;
  readonly canContinue: boolean; readonly continueReason?: ContinuationReason;
  readonly measuredWork: string; readonly chargedWork: string; readonly outstandingWork: string;
  /** What a resumed or continued attempt is charged first: the whole unconfirmed interval, once. */
  readonly chargedAfterInterruption: string;
  readonly durationChargedMs: string; readonly durationOutstandingMs: string; readonly durationAfterInterruptionMs: string;
  readonly allowanceBefore: string; readonly allowanceAfter: string;
  readonly authorizedWork: string; readonly newWorkGrant: string;
  readonly position: string; readonly outputCount: string;
  readonly bounds: readonly ContinuationBound[];
  readonly frozen: ContinuationFrozen;
}

const PREVIEW_FIELDS = ["version", "node", "analysis", "run", "attempt", "basis", "budgetDigest", "budgetIssuedAttempt", "latest", "activeWriter",
  "lifecycle", "stop", "canResume", "resumeReason", "canContinue", "continueReason", "measuredWork", "chargedWork", "outstandingWork",
  "chargedAfterInterruption", "durationChargedMs", "durationOutstandingMs", "durationAfterInterruptionMs", "allowanceBefore", "allowanceAfter",
  "authorizedWork", "newWorkGrant", "position", "outputCount", "bounds", "frozen"] as const;
const BOUND_FIELDS = ["key", "current", "issuanceCeiling", "activeCeiling", "requested", "status"] as const;
const FROZEN_FIELDS = ["stepRevision", "finishRevision", "profileRevision", "sourceDigest", "memory", "recordWork", "scratch",
  "recordOutputs", "startup", "rate"] as const;
const SHA256 = /^sha256:[0-9a-f]{64}$/;
const DECIMAL = /^(?:0|[1-9][0-9]{0,19})$/;
const U64_MAX = 18446744073709551615n;
/** The language's Int literal is signed 64-bit. */
const I64_MAX = 9223372036854775807n;

class Mismatch extends Error {}
const no = (): never => { throw new Mismatch(); };
const object = (value: unknown): Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value) && !isExactNumber(value) ? value as Record<string, unknown> : no();
const exactKeys = (value: Record<string, unknown>, keys: readonly string[]) => {
  if (Object.keys(value).length !== keys.length || keys.some(key => !Object.hasOwn(value, key))) no();
};
/** A native `Int` field: non-negative and within i64. `position`/`outputCount` are u64 text instead. */
const int = (value: unknown): string => nativeIntText(value) ?? no();
const text = (value: unknown): string => typeof value === "string" ? value : no();
const bool = (value: unknown): boolean => typeof value === "boolean" ? value : no();
const digest = (value: unknown): string => typeof value === "string" && SHA256.test(value) ? value : no();
/** Exact unsigned decimal text, as the engine writes `position` and `outputCount`. */
const decimal = (value: unknown): string => typeof value === "string" && DECIMAL.test(value) && BigInt(value) <= U64_MAX ? value : no();
const oneOf = <T extends string>(value: unknown, allowed: readonly T[]): T => (allowed as readonly unknown[]).includes(value) ? value as T : no();
function option<T>(value: unknown, read: (value: unknown) => T): T | undefined {
  const wrapped = object(value);
  if (wrapped.kind === "none" && Object.keys(wrapped).length === 1) return undefined;
  if (wrapped.kind === "some" && Object.keys(wrapped).length === 2 && "value" in wrapped) return read(wrapped.value);
  return no();
}
const field = (shape: TypeShape, name: string) => shape.kind === "record" ? shape.fields.find(it => it.name === name)?.type : undefined;
const named = (shape: TypeShape | undefined, name: string) => shape?.kind === "record" && shape.name === name;

/** The review a `ScanContinuationPreview` value projects, or nothing when it is not exactly one. */
export function continuationPreviewOf(value: StoredValue | undefined): ContinuationPreview | undefined {
  if (!value || !named(value.type, "ScanContinuationPreview")) return undefined;
  const rows = field(value.type, "bounds");
  if (rows?.kind !== "list" || !named(rows.element, "ScanContinuationBound")) return undefined;
  if (!named(field(value.type, "frozen"), "ScanContinuationFrozen")) return undefined;
  if (value.meta && value.meta.contract.name !== "ScanContinuationPreview") return undefined;
  try {
    const raw = object(value.data);
    exactKeys(raw, PREVIEW_FIELDS);
    if (int(raw.version) !== "1") no();
    if (!Array.isArray(raw.bounds) || raw.bounds.length !== TOTALS.length) no();
    const bounds = (raw.bounds as unknown[]).map((item, at): ContinuationBound => {
      const row = object(item);
      exactKeys(row, BOUND_FIELDS);
      if (row.key !== TOTALS[at]) no();
      return { key: TOTALS[at]!, current: int(row.current), issuanceCeiling: int(row.issuanceCeiling), activeCeiling: int(row.activeCeiling),
        requested: int(row.requested), status: oneOf(row.status, ["above_ceiling", "lowered", "unchanged", "raised"] as const) };
    });
    const rawFrozen = object(raw.frozen);
    exactKeys(rawFrozen, FROZEN_FIELDS);
    const frozenFinish = option(rawFrozen.finishRevision, text);
    const frozen: ContinuationFrozen = {
      stepRevision: text(rawFrozen.stepRevision), ...(frozenFinish !== undefined ? { finishRevision: frozenFinish } : {}),
      profileRevision: text(rawFrozen.profileRevision), sourceDigest: text(rawFrozen.sourceDigest),
      memory: int(rawFrozen.memory), recordWork: int(rawFrozen.recordWork), scratch: int(rawFrozen.scratch),
      recordOutputs: int(rawFrozen.recordOutputs), startup: int(rawFrozen.startup), rate: int(rawFrozen.rate),
    };
    const canResume = bool(raw.canResume), canContinue = bool(raw.canContinue);
    const resumeReason = option(raw.resumeReason, reason => oneOf(reason, REASONS));
    const continueReason = option(raw.continueReason, reason => oneOf(reason, REASONS));
    // Each offer is exactly the absence of its reason; a projection saying otherwise is not the engine's.
    if (canResume !== (resumeReason === undefined) || canContinue !== (continueReason === undefined)) no();
    const stop = option(raw.stop, text);
    return {
      node: text(raw.node), analysis: text(raw.analysis), run: text(raw.run), attempt: text(raw.attempt),
      basis: digest(raw.basis), budgetDigest: digest(raw.budgetDigest), budgetIssuedAttempt: text(raw.budgetIssuedAttempt),
      latest: bool(raw.latest), activeWriter: bool(raw.activeWriter), lifecycle: oneOf(raw.lifecycle, LIFECYCLES),
      ...(stop !== undefined ? { stop } : {}),
      canResume, ...(resumeReason ? { resumeReason } : {}), canContinue, ...(continueReason ? { continueReason } : {}),
      measuredWork: int(raw.measuredWork), chargedWork: int(raw.chargedWork), outstandingWork: int(raw.outstandingWork),
      chargedAfterInterruption: int(raw.chargedAfterInterruption),
      durationChargedMs: int(raw.durationChargedMs), durationOutstandingMs: int(raw.durationOutstandingMs),
      durationAfterInterruptionMs: int(raw.durationAfterInterruptionMs),
      allowanceBefore: int(raw.allowanceBefore), allowanceAfter: int(raw.allowanceAfter),
      authorizedWork: int(raw.authorizedWork), newWorkGrant: int(raw.newWorkGrant),
      position: decimal(raw.position), outputCount: decimal(raw.outputCount),
      bounds, frozen,
    };
  } catch (error) {
    if (error instanceof Mismatch) return undefined;
    throw error;
  }
}

/** The totals the engine reviewed, in its order. */
export const requestedTotals = (preview: ContinuationPreview): Totals =>
  Object.fromEntries(preview.bounds.map(bound => [bound.key, bound.requested])) as Totals;

/**
 * Whether a total can be written as the positive Int literal the command's grammar takes: canonical
 * decimal, no larger than i64. Grammar only — the engine alone decides admission (its own total caps
 * and the active ceilings), so a spellable total may still be refused by the new review.
 */
export function literalTotal(value: string): boolean {
  return /^[1-9][0-9]{0,18}$/.test(value) && BigInt(value) <= I64_MAX;
}

const literals = (totals: Totals) => TOTALS.map(key => `${key}:${totals[key]}`).join(" ");

/**
 * A new read-only review of the original analysis with these totals. `node` is the engine's own node
 * id; undefined when it is not one a command can name or a total cannot be written as a literal.
 */
export function continuationCommand(node: string, totals: Totals, result: string): string | undefined {
  if (!REFERABLE_NODE.test(node) || TOTALS.some(key => !literalTotal(totals[key]))) return undefined;
  return `:scan continuation $${node} ${literals(totals)} > ${result}`;
}

/**
 * The guarded new attempt for exactly a reviewed basis and its reviewed totals. Undefined unless the
 * basis is canonical, the node is nameable and every total can be written as a literal.
 */
export function continueCommand(node: string, basis: string, totals: Totals, result: string): string | undefined {
  if (!SHA256.test(basis) || !REFERABLE_NODE.test(node) || TOTALS.some(key => !literalTotal(totals[key]))) return undefined;
  return `:scan continue $${node} basis:"${basis}" ${literals(totals)} > ${result}`;
}

/** The review command for a receipt's own analysis, with the totals left as they are. */
export function reviewCommand(node: string, result: string): string | undefined {
  return REFERABLE_NODE.test(node) ? `:scan continuation $${node} > ${result}` : undefined;
}
