/**
 * The strict boundary for engine events whose shape decides what a person is told about a run.
 *
 * Progress and evidence are read here once, field by field, for every surface: a malformed counter
 * or an unknown evidence kind is refused rather than drawn as a guess. Other events still pass as the
 * engine wrote them; they are transcribed in `protocol.ts`.
 */
import { isExactNumber } from "./exact-json";
import { DEPENDENCY_LIFETIMES } from "./protocol";
import type { Event, EvidenceKind, ExecutionProgress, NodeState, ReconciliationControl, RecordCounters, RecordingCounters, RecordingWriterState, RecordPhase } from "./protocol";
import { RECORDING_TERMINATIONS } from "./dataset-read";

const NODE_STATES: readonly NodeState[] = ["pending", "running", "ready", "stale", "failed", "cancelled", "skipped"];
const EVIDENCE_KINDS: readonly EvidenceKind[] = ["stopped_stream", "incomplete"];
const PHASES: readonly RecordPhase[] = ["reading", "processing", "finishing", "committing", "complete", "stopped", "cancelled"];
const RETENTIONS = ["temporary", "automatic", "protected", "unknown"];
const COUNTERS = ["committedPosition", "readPosition", "extent", "unit", "inputRecords", "outputRecords", "work", "workAllowance",
  "workLimit", "heldCharge", "highWaterCharge", "heldLimit", "outputCharge", "outputLimit"] as const;

class Malformed extends Error {}
const refuse = (what: string): never => { throw new Malformed(`Malformed engine event: ${what}`); };
const record = (value: unknown, what: string): Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value) && !isExactNumber(value) ? value as Record<string, unknown> : refuse(what);
const text = (value: unknown, what: string): string => typeof value === "string" ? value : refuse(what);
const named = (value: unknown, what: string): string => text(value, what) !== "" ? value as string : refuse(what);
const oneOf = <T extends string>(value: unknown, allowed: readonly T[], what: string): T =>
  typeof value === "string" && (allowed as readonly string[]).includes(value) ? value as T : refuse(what);
const optional = <T>(value: unknown, read: (value: unknown) => T): T | undefined => value === undefined || value === null ? undefined : read(value);

/** A non-negative integer as canonical decimal text, whether it arrived as a number or an exact lexeme. */
export function counter(value: unknown, what: string): string {
  return counterText(value) ?? refuse(what);
}
/** The same reading without throwing, for values whose shape is checked rather than required. */
export function counterText(value: unknown): string | undefined {
  const lexeme = typeof value === "number" ? (Number.isSafeInteger(value) ? String(value) : "") : isExactNumber(value) ? value.text : "";
  return /^(0|[1-9]\d{0,19})$/.test(lexeme) && BigInt(lexeme) <= 18446744073709551615n ? lexeme : undefined;
}

/** Each writer state belongs to exactly one shared phase. */
const WRITER_PHASES: Readonly<Record<RecordingWriterState, RecordPhase>> = {
  prepared: "reading", recording: "processing", draining: "committing", stopped: "complete", incomplete: "stopped", unconfirmed: "stopped",
};
const WRITER_STATES = Object.keys(WRITER_PHASES) as RecordingWriterState[];
const RECORDING = ["state", "first", "acceptedThrough", "committedThrough", "pending", "rejected", "termination", "chargedBytes", "chargedWork", "bytesLimit", "workLimit"] as const;
const DECIMAL = /^(0|[1-9][0-9]{0,19})$/;
/** A recording count: canonical u64 decimal text only, never a JSON number. */
const decimal = (value: unknown, what: string): string =>
  typeof value === "string" && DECIMAL.test(value) && BigInt(value) <= 18446744073709551615n ? value : refuse(what);

/**
 * A recording writer's status: exactly its fields, a state that agrees with the phase, and counts
 * that agree with each other (committed never beyond accepted, a known pending count equal to the
 * difference). Anything else is refused whole.
 */
function decodeRecording(raw: Record<string, unknown>, phase: RecordPhase): RecordingCounters {
  if (Object.keys(raw).length !== RECORDING.length || RECORDING.some(key => !Object.hasOwn(raw, key))) refuse("recording fields");
  const state = oneOf(raw.state, WRITER_STATES, "recording state");
  if (WRITER_PHASES[state] !== phase) refuse("recording state and phase");
  const first = decimal(raw.first, "recording first"), acceptedThrough = decimal(raw.acceptedThrough, "recording accepted");
  const committedThrough = decimal(raw.committedThrough, "recording committed"), rejected = decimal(raw.rejected, "recording rejected");
  const pending = raw.pending === null ? null : decimal(raw.pending, "recording pending");
  const termination = raw.termination === null ? null : oneOf(raw.termination, RECORDING_TERMINATIONS, "recording termination");
  const before = BigInt(first) - 1n, accepted = BigInt(acceptedThrough), committed = BigInt(committedThrough);
  if (first === "0" || committed < before || accepted < committed) refuse("recording sequence");
  if (pending !== null && BigInt(pending) !== accepted - committed) refuse("recording pending");
  return { state, first, acceptedThrough, committedThrough, pending, rejected, termination,
    chargedBytes: decimal(raw.chargedBytes, "recording chargedBytes"), chargedWork: decimal(raw.chargedWork, "recording chargedWork"),
    bytesLimit: decimal(raw.bytesLimit, "recording bytesLimit"), workLimit: decimal(raw.workLimit, "recording workLimit") };
}

export function decodeProgress(value: unknown): ExecutionProgress {
  const progress = record(value, "progress");
  if (progress.kind === "recording") {
    if (Object.keys(progress).some(key => !["kind", "phase", "counters", "recording"].includes(key))) refuse("progress field");
    const phase = oneOf(progress.phase, PHASES, "progress phase");
    if (progress.counters !== null) refuse("recording progress counters");
    // Absent: withheld because the source is not public. Present: one exact writer status.
    if (!Object.hasOwn(progress, "recording")) return { kind: "recording", phase, counters: null };
    return { kind: "recording", phase, counters: null, recording: decodeRecording(record(progress.recording, "recording"), phase) };
  }
  if (Object.keys(progress).some(key => !["kind", "phase", "counters"].includes(key))) refuse("progress field");
  if (progress.kind !== "records") refuse("progress kind");
  const phase = oneOf(progress.phase, PHASES, "progress phase");
  if (progress.counters === null) return { kind: "records", phase, counters: null };
  const raw = record(progress.counters, "progress counters");
  if (Object.keys(raw).length !== COUNTERS.length || COUNTERS.some(key => !(key in raw))) refuse("progress counters");
  const read = (key: Exclude<typeof COUNTERS[number], "unit">) => counter(raw[key], `progress ${key}`);
  const counters: RecordCounters = {
    committedPosition: read("committedPosition"), readPosition: read("readPosition"), extent: read("extent"),
    unit: oneOf(raw.unit, ["bytes", "records"] as const, "progress unit"),
    inputRecords: read("inputRecords"), outputRecords: read("outputRecords"),
    work: read("work"), workAllowance: read("workAllowance"), workLimit: read("workLimit"),
    heldCharge: read("heldCharge"), highWaterCharge: read("highWaterCharge"), heldLimit: read("heldLimit"),
    outputCharge: read("outputCharge"), outputLimit: read("outputLimit"),
  };
  return { kind: "records", phase, counters };
}

function decodeEvidence(event: Record<string, unknown>): Event {
  const provenance = record(event.provenance, "evidence provenance");
  if (Object.values(provenance).some(value => typeof value !== "string")) refuse("evidence provenance");
  if (!Array.isArray(event.cautions) || event.cautions.some(caution => typeof caution !== "string")) refuse("evidence cautions");
  if (typeof event.kept !== "boolean") refuse("evidence kept");
  if (typeof event.bytes !== "number" || !Number.isSafeInteger(event.bytes) || event.bytes < 0) refuse("evidence bytes");
  const error = optional(event.error, value => {
    const error = record(value, "evidence error");
    text(error.id, "evidence error id"); text(error.code, "evidence error code"); text(error.message, "evidence error message");
    if (!Array.isArray(error.issues)) refuse("evidence error issues");
    return error as unknown as Extract<Event, { event: "evidence" }>["error"];
  });
  return {
    ...(event as object),
    event: "evidence",
    node: named(event.node, "evidence node"),
    state: oneOf(event.state, NODE_STATES, "evidence state"),
    kind: oneOf(event.kind, EVIDENCE_KINDS, "evidence kind"),
    source: named(event.source, "evidence source"),
    run: named(event.run, "evidence run"),
    type: text(event.type, "evidence type"),
    handle: named(event.handle, "evidence handle"),
    bytes: event.bytes as number,
    provenance: provenance as Record<string, string>,
    cautions: event.cautions as string[],
    kept: event.kept as boolean,
    private: optional(event.private, value => typeof value === "boolean" ? value : refuse("evidence private")),
    retention: optional(event.retention, value => oneOf(value, RETENTIONS, "evidence retention") as never),
    error,
    reason: optional(event.reason, value => text(value, "evidence reason")),
    constructionComplete: optional(event.constructionComplete, value => typeof value === "boolean" ? value : refuse("evidence constructionComplete")),
  } as Event;
}

const RECORDING_CONTROL_KEYS = ["active", "statusAvailable", "stopAvailable", "discardAvailable"] as const;
const RECONCILIATION_CONTROL_KEYS = ["command", "available"] as const;
const RECONCILIATION_COMMANDS: readonly ReconciliationControl["command"][] = ["scan", "dataset"];

/**
 * The lifecycle and control fields of `created`, strictly; the rest passes as the engine wrote it.
 * A malformed field is refused rather than read as an open run or a grant.
 */
function decodeCreated(event: Record<string, unknown>): Event {
  let decoded: Record<string, unknown> = event;
  // An unknown spelling is refused, never read as the continuous default.
  const lifetime = oneOf(event.dependencyLifetime, DEPENDENCY_LIFETIMES, "dependencyLifetime");
  // Absent means not captured; a present flag is a boolean, and only a captured lifetime can be true.
  if (Object.hasOwn(event, "inputsCaptured") && typeof event.inputsCaptured !== "boolean") refuse("inputsCaptured");
  if (event.inputsCaptured === true && lifetime !== "captured") refuse("inputsCaptured without a captured lifetime");
  if (Object.hasOwn(event, "lifetimeActive") && event.lifetimeActive !== null && typeof event.lifetimeActive !== "boolean") refuse("lifetimeActive");
  if (Object.hasOwn(event, "recordingControl") && event.recordingControl !== null) {
    // Exactly four booleans, or null for none; any other key (a start, say) is refused, never ignored.
    const raw = record(event.recordingControl, "recording control");
    if (Object.keys(raw).length !== RECORDING_CONTROL_KEYS.length || RECORDING_CONTROL_KEYS.some(key => typeof raw[key] !== "boolean")) refuse("recording control");
    // A finished run can be neither stopped nor discarded; a setup is discarded, an attached writer stopped.
    if ((raw.stopAvailable === true || raw.discardAvailable === true) && raw.active === false) refuse("recording control without an active run");
    if (raw.stopAvailable === true && raw.discardAvailable === true) refuse("recording control offers both stop and discard");
    decoded = { ...event, recordingControl: { active: raw.active as boolean, statusAvailable: raw.statusAvailable as boolean,
      stopAvailable: raw.stopAvailable as boolean, discardAvailable: raw.discardAvailable as boolean } };
  }
  if (Object.hasOwn(event, "reconciliationControl") && event.reconciliationControl !== null) {
    // Exactly one known command and one boolean, or null for none; any other key or command is refused.
    const raw = record(event.reconciliationControl, "reconciliation control");
    if (Object.keys(raw).length !== RECONCILIATION_CONTROL_KEYS.length || RECONCILIATION_CONTROL_KEYS.some(key => !Object.hasOwn(raw, key))) refuse("reconciliation control");
    if (typeof raw.available !== "boolean") refuse("reconciliation control available");
    decoded = { ...decoded, reconciliationControl: { command: oneOf(raw.command, RECONCILIATION_COMMANDS, "reconciliation control command"),
      available: raw.available as boolean } };
  }
  return decoded as unknown as Event;
}

/** Reads one engine event; throws on a malformed progress or evidence event. */
export function decodeEvent(raw: unknown): Event {
  const event = record(raw, "event");
  if (event.event === "node-progress") {
    return {
      event: "node-progress",
      node: named(event.node, "progress node"),
      run: event.run === null ? null : named(event.run, "progress run"),
      progress: decodeProgress(event.progress),
    };
  }
  if (event.event === "result-access") {
    // Exactly the three keys the engine sends; only a withdrawal exists, never a grant.
    if (Object.keys(event).length !== 3 || !Object.hasOwn(event, "node") || !Object.hasOwn(event, "readable")) refuse("result-access keys");
    if (event.readable !== false) refuse("result-access readable");
    return { event: "result-access", node: named(event.node, "result-access node"), readable: false };
  }
  if (event.event === "evidence") return decodeEvidence(event);
  if (event.event === "created") return decodeCreated(event);
  // The retired event is refused, not silently re-read as evidence of an unknown kind.
  if (event.event === "stopped") refuse("retired stopped event");
  return event as unknown as Event;
}
