/**
 * The strict boundary for engine events whose shape decides what a person is told about a run.
 *
 * Progress and evidence are read here once, field by field, for every surface: a malformed counter
 * or an unknown evidence kind is refused rather than drawn as a guess. Other events still pass as the
 * engine wrote them; they are transcribed in `protocol.ts`.
 */
import { isExactNumber } from "./exact-json";
import type { Event, EvidenceKind, ExecutionProgress, NodeState, RecordCounters, RecordPhase } from "./protocol";

const NODE_STATES: readonly NodeState[] = ["pending", "running", "ready", "stale", "failed", "cancelled", "skipped"];
const EVIDENCE_KINDS: readonly EvidenceKind[] = ["stopped_stream", "incomplete"];
const PHASES: readonly RecordPhase[] = ["reading", "processing", "finishing", "complete", "stopped", "cancelled"];
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

export function decodeProgress(value: unknown): ExecutionProgress {
  const progress = record(value, "progress");
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
  if (event.event === "evidence") return decodeEvidence(event);
  // The retired event is refused, not silently re-read as evidence of an unknown kind.
  if (event.event === "stopped") refuse("retired stopped event");
  return event as unknown as Event;
}
