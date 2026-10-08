export interface ExecutionCapacity {
  readonly operations: { readonly used: number; readonly limit: number };
  readonly streams: { readonly used: number; readonly limit: number };
}

/**
 * The engine's messages, as this language sees them.
 *
 * The wire format is the single source of truth — these are a transcription of it, not a second design.
 * When the engine's `Wire` changes, this file is what has to change with it, and nothing else does.
 */

export interface WaitingInput {readonly source:string;readonly port:"data"|"error"|"cancel";readonly state:"pending"|"closed";readonly run:string|null;readonly message:string}
export type NodeState = "pending" | "running" | "ready" | "stale" | "failed" | "cancelled" | "skipped";

export interface EnvironmentContext { readonly selected: string | null; readonly revisions: Record<string, string> }
export type Severity = "error" | "warning" | "info";

export interface StaleReason { readonly code: string; readonly message: string }

export interface SourceLocation {
  readonly source: string; readonly start: number; readonly end: number;
  readonly line: number; readonly column: number; readonly endLine: number; readonly endColumn: number;
}
export interface ErrorRecord {
  readonly locations?: readonly SourceLocation[];
  readonly id: string; readonly code: string; readonly message: string;
  readonly causeId: string; readonly issues: readonly { path: string; code: string; message: string }[];
}

/** Engine publication evidence, separate from execution state and HTTP read failures. */
/** A Pin's view binding, separate from whether its result was kept. Refusal never un-keeps the result. */
export type PinBinding = { readonly state: "pending" } | { readonly state: "bound" } | { readonly state: "refused"; readonly problem: string };
export type DependencyLifetime = "continuous" | "creation" | "captured";
export const DEPENDENCY_LIFETIMES: readonly DependencyLifetime[] = ["continuous", "creation", "captured"];
export type ResultPublication = {
  readonly run: string | null;
  readonly uncertainHandle: string | null;
  readonly problem: ErrorRecord | null;
  readonly message: string;
  readonly pinBinding?: PinBinding | null;
} & ({ readonly state: "available"; readonly handle: string }
  | { readonly state: "pending"; readonly handle: null }
  | { readonly state: "unavailable"; readonly handle: null });

export interface ResultDescriptor {
  readonly publication?: ResultPublication;
  readonly private?: boolean;
  readonly node: string;
  readonly type: string;
  readonly handle: string;
  readonly bytes: number;
  readonly provenance: Record<string, string>;
  /** What was overridden to produce this, if anything. */
  readonly cautions: string[];
  /** Whether this survives the engine restarting. Every result is stored; only some are kept. */
  readonly kept: boolean;
  readonly retention?: Retention;
}

export type EvidenceKind = "stopped_stream" | "incomplete";
/** `committing` means outputs are being made durable; it is a working phase, never success. */
export type RecordPhase = "reading" | "processing" | "finishing" | "committing" | "complete" | "stopped" | "cancelled";
/**
 * Committed and read positions share `unit`. Charges are conservative logical charges, never RSS or
 * encoded bytes. `workAllowance` is the work allowed so far (input-earned, plus any explicitly authorized
 * continuation credit, which this projection does not separate), under the fixed `workLimit`. Integers are
 * decimal strings after decoding, because the engine's u64 counters exceed a JavaScript number.
 */
export interface RecordCounters {
  readonly committedPosition: string; readonly readPosition: string; readonly extent: string; readonly unit: "bytes" | "records";
  readonly inputRecords: string; readonly outputRecords: string;
  readonly work: string; readonly workAllowance: string; readonly workLimit: string;
  readonly heldCharge: string; readonly highWaterCharge: string; readonly heldLimit: string;
  readonly outputCharge: string; readonly outputLimit: string;
}
/**
 * What the engine says a node's current recording run accepts: an attached writer, or a recording
 * setup prepared before its source runs. A prepared setup may be discarded; an attached writer is
 * stopped. Neither is ever offered for a run that has finished, and never both at once.
 */
export interface RecordingControl {
  /** The run (setup or physical writer) has not finished; it stays true while a stop drains. */
  readonly active: boolean;
  readonly statusAvailable: boolean;
  /** Never true once the run has finished. */
  readonly stopAvailable: boolean;
  /** Only an unused prepared setup can be discarded; never true once the run has finished. */
  readonly discardAvailable: boolean;
}
/**
 * Which local reconciliation command the engine accepts for a node's original owned run: `scan` for
 * a bound scan or resume, `dataset` for the recording that starts a lifetime. It concerns the joined
 * local store only; it never reruns a producer or resumes analysis.
 */
export interface ReconciliationControl {
  readonly command: "scan" | "dataset";
  /** True only after the original run has physically joined; never while it is active, open or waiting. */
  readonly available: boolean;
}
/** A recording writer's own state; each maps to exactly one shared phase. */
export type RecordingWriterState = "prepared" | "recording" | "draining" | "stopped" | "incomplete" | "unconfirmed";
/**
 * The latest status a recording writer acknowledged, as exact decimal strings. Lossy and at most a
 * few times a second: not a stored receipt, not the source's state, not a live queue count and not
 * a permission. Charges are conservative value/work charges against the run's captured limits.
 */
export interface RecordingCounters {
  readonly state: RecordingWriterState;
  readonly first: string; readonly acceptedThrough: string; readonly committedThrough: string;
  /** Accepted but not yet committed, or null when unknown. */
  readonly pending: string | null;
  readonly rejected: string;
  readonly termination: import("./dataset-read").RecordingTermination | null;
  readonly chargedBytes: string; readonly chargedWork: string; readonly bytesLimit: string; readonly workLimit: string;
}
/**
 * One run's progress. A finite analysis reports positions and charges; a recording reports its
 * writer status. For a recording, an absent `recording` means the counters are withheld because the
 * source is not public, never zero.
 */
export type ExecutionProgress =
  | { readonly kind: "records"; readonly phase: RecordPhase; readonly counters: RecordCounters | null }
  | { readonly kind: "recording"; readonly phase: RecordPhase; readonly counters: null; readonly recording?: RecordingCounters };

export interface ExecutionRecord {
  readonly id: string;
  readonly node: string;
  readonly run: string;
  readonly at: string;
  readonly state: Uppercase<NodeState>;
  readonly error: ErrorRecord | readonly [];
}

export interface LogEvent {
  readonly event: "log";
  readonly record: ExecutionRecord;
  readonly durable: boolean;
  readonly persistenceProblem: string;
}

export interface DiagnosticLogEvent {
  readonly event: "log-diagnostic";
  readonly record: { readonly id: string; readonly at: string; readonly cell: string;
    readonly source: string; readonly diagnostic: Diagnostic };
  readonly durable: boolean;
  readonly persistenceProblem: string;
}

export interface NoticeLogEvent {
  readonly event: "log-notice";
  readonly record: { readonly id: string; readonly at: string; readonly error: ErrorRecord;
    readonly context: { readonly kind: "execution" | "publication" | "keep" | "release" | "eviction" | "storage-worker" | "workspace-shutdown";
      readonly node: string | null; readonly run: string | null; readonly handle: string | null;
      readonly mayHaveApplied?: boolean } };
  readonly durable: boolean;
  readonly persistenceProblem: string;
}
export interface StartupWarning {
  readonly event: "startup-warning"; readonly id: string; readonly message: string;
  readonly node: string | null; readonly handle: string | null;
}
export interface LogStatus {
  readonly event: "log-status"; readonly pending: number; readonly omitted: string;
  readonly omittedNotDurable: string; readonly unconfirmed: string; readonly captureFailures: string;
}
export type HistoryEvent = LogEvent | DiagnosticLogEvent | NoticeLogEvent;
export interface SavedHistoryPage {
  readonly generation: string;
  readonly entries: readonly HistoryEvent[];
  readonly next: string | null;
  readonly through: string;
  readonly unconfirmedWrites: string;
}

export interface Diagnostic {
  /** The stable identifier, like `CHK009`. Not our enum — a string, deliberately. */
  readonly code: string;
  readonly severity: Severity;
  readonly message: string;
  /** Where it points, as offsets into the submitted text. */
  readonly start: number;
  readonly end: number;
  /** What to try. Some errors have more than one honest answer; that is why this is a list. */
  readonly hints: readonly string[];
}

export type Event =
  | { readonly event: "workspace-closed"; readonly workspace: string | null }
  | { readonly event: "workspace-context"; readonly name: string; readonly saved: readonly string[] }
  | { readonly event: "environments"; readonly managed: boolean; readonly default?: string | null; readonly enabled: Record<string, boolean>; readonly credentials: Record<string, Record<string, Record<string, string>>>; readonly revisions: Record<string, string>; readonly providers: Record<string, ProviderWire[]>; readonly clients: Record<string, EnvironmentContext> }
  | { readonly event: "node-environment"; readonly node: string; readonly environment: string; readonly revision: string; readonly target: string; readonly endpoint: string | null; readonly origin: string }
  /** The greeting names its workspace and generation together; null while no workspace is open. */
  | { readonly event: "session"; readonly workspace: string | null; readonly generation: string; readonly cells?: readonly string[] }
  | ({ readonly event: "execution-capacity" } & ExecutionCapacity)
  | { readonly event: "storage-warning"; readonly message: string }
  | { readonly event: "projection-unavailable"; readonly message: string }
  | HistoryEvent
  | StartupWarning
  | LogStatus
  | { readonly event: "log-delta"; readonly reset: boolean; readonly removed: readonly string[]; readonly entries: readonly HistoryEvent[] }
  | {
      readonly event: "reported";
      readonly cell: string;
      readonly source: string;
      readonly diagnostics: Diagnostic[];
    }
  | {
      readonly event: "vocabulary";
      readonly calculation?: import("./vocabulary").CalculationVocabulary;
      readonly commands: MetaCommandWire[];
      readonly annotations: string[];
      readonly providers: ProviderWire[];
      readonly templates?: readonly import("./vocabulary").Template[];
    }
  | {
      /**
       * What a submission turned into. Sent once per accepted submission, even when the answer is
       * nothing — `:import` makes no nodes, so a cell waiting for one would wait forever on exactly
       * the commands that worked.
       */
      /**
       * What a running command has written. Only an interactive one writes while it runs; everything
       * else reports once, at the end, through a value.
       */
      readonly event: "output";
      readonly node: string;
      readonly run: string;
      readonly text: string;
      readonly omittedBytes: string;
    }
  | { readonly event: "conversation"; readonly node: string; readonly run: string; readonly active: boolean }
  | { readonly event: "output-gap" }
  | {
      readonly event: "storage";
      readonly workspace: string;
      readonly retention?: {
        readonly classes: readonly { kind: Retention; count: number; bytes: number }[];
        readonly liveBytes: number; readonly archiveBytes: number;
        readonly privateCount: number; readonly privateBytes: number;
      };
      readonly places: {
        readonly name: string;
        readonly where: string;
        readonly holds: string;
        readonly durable: boolean;
        readonly files: number;
        readonly bytes: number;
      }[];
    }
  | {
      readonly event: "planned";
      readonly receipts?: readonly { readonly summary: string }[] | null;
      readonly restored?: boolean;
      readonly workOf?: string | null;
      readonly revisionOf?: string | null;
      readonly revisionAccepted?: boolean;
      readonly repeatOf?: string | null;
      readonly repeatFrom?: string | null;
      readonly repeatedRun?: string | null;
      readonly acknowledgeEffects?: boolean;
      readonly failure?: string | null;
      readonly diagnostics?: readonly Diagnostic[];
      readonly document?: { readonly source: string } | null;
      readonly cell: string;
      /**
       * What was submitted. Redundant while the client that typed it is still listening, and the only
       * source there is for one attaching later — a submission that made no nodes has nothing else to
       * say what it was.
       */
      readonly text: string;
      readonly nodes: string[];
    }
  | {
      /**
       * A call that was in flight when the engine stopped. Not a failure: a failure says the call did
       * not happen, and the whole point is that nobody knows.
       */
      readonly event: "interrupted";
      readonly node: string;
      readonly cell: string;
      readonly capability: string;
      readonly safe: boolean;
      readonly when: string;
    }
  | {
      readonly event: "created";
      /**
       * How the node's input edges govern it. `creation` edges order its one construction; once that
       * succeeds, upstream refreshes no longer reach it (a view then follows its Current input).
       * `captured` (an analysis) takes its inputs once when its run starts: later producer updates
       * neither cancel nor recompute that run, and an explicit refresh captures the new inputs. It is
       * not a construction and is never "constructed".
       */
      readonly dependencyLifetime: DependencyLifetime;
      /**
       * Whether a `captured` node's current run has actually taken its inputs at guarded entry. False
       * before entry (a spawned run that has not entered), true afterwards and after restoration.
       * Never true for another lifetime. Absent is read as false.
       */
      readonly inputsCaptured?: boolean;
      readonly currentDefinition?: string | null;
      readonly run?: string | null;
      readonly errorNames?:readonly string[];
      readonly repeatable?: boolean;
      /** Engine-recorded RUNNING time for this node's current run; absent without retained evidence. */
      readonly startedAt?: string | null;
      readonly traced?: boolean;
      readonly node: string;
      readonly dependsOn: string[];
      readonly name: string;
      readonly command: string;
      /** Whether somebody can type at it while it runs. */
      readonly interactive: boolean;
      /** Output can change with an upstream stream; independent of current run state. */
      readonly streamOutput?: boolean;
      /** The node's own bound call opens a stream: it is a source, not a consumer of one. */
      readonly streamSource?: boolean;
      /**
       * Which recording commands this exact run's admitted writer accepts now, or null/absent for none.
       * Metadata for offering a reviewed command only, never a permission: commands are checked again.
       */
      readonly recordingControl?: RecordingControl | null;
      /**
       * The local reconciliation this exact original run accepts, or null/absent for none (copies,
       * selections and other commands have none). Metadata for a reviewed command only, never a permission.
       */
      readonly reconciliationControl?: ReconciliationControl | null;
      /**
       * For work with an owned lifetime (a recording, or a scan following a committed EventLog):
       * whether this run is still open. Null/absent for other work. A ready node may be lifetime
       * active: its value is an acknowledged immutable prefix, not a stream, and is not re-read per
       * event. Restored or reopened work is never active.
       */
      readonly lifetimeActive?: boolean | null;
    }
  | { readonly event: "node"; readonly node: string; readonly state: NodeState; readonly waiting?:readonly WaitingInput[]; readonly staleReason?: StaleReason; readonly publication?: ResultPublication;
      /** Newer committed input arrived while this captured calculation runs; it is computed after this one completes. */
      readonly updatePending?: boolean;
      /** A creation-lifetime node finished constructing; its input edges no longer propagate refreshes. */
      readonly constructionComplete: boolean }
  | ({ readonly event: "ready"; readonly constructionComplete?: boolean } & ResultDescriptor)
  /**
   * A display-only value of a node whose graph state stays terminal: a stopped stream's last value or
   * an analysis's committed partial result. Never a successful output; `state` is the engine's own.
   */
  | ({ readonly event: "evidence"; readonly state: NodeState; readonly kind: EvidenceKind; readonly source: string; readonly run: string;
      readonly error?: ErrorRecord; readonly reason?: string; readonly constructionComplete?: boolean } & ResultDescriptor)
  /** Lossy status of one run; counters are null when they are not public. Never a value or history entry. */
  | { readonly event: "node-progress"; readonly node: string; readonly run: string | null; readonly progress: ExecutionProgress }
  /**
   * Access to a node's current result was withdrawn. Carries no value, type, count or reason: every
   * cached value, value-derived fact and display of that node is dropped. Never a request to rerun.
   */
  | { readonly event: "result-access"; readonly node: string; readonly readable: false }
  | {
      /**
       * The rule in force: what this session keeps without being asked, and up to what size.
       *
       * Said on connect and again whenever it changes. The rule lives in the engine because that is
       * the only thing present when a result becomes ready — a client trusting its own copy would
       * show a confident wrong answer after a reload.
       */
      readonly event: "keeping";
      readonly automatic: boolean;
      readonly under: number;
    }
  | { readonly event: "failed"; readonly node: string; readonly reason: string; readonly error: ErrorRecord }
  | { readonly event: "cancelled"; readonly node: string; readonly code: string; readonly reason: string }
  | { readonly event: "work-retired"; readonly cells: string[] }
  | { readonly event: "dropped"; readonly nodes: string[] };

/** A subcommand word and what follows it. Leaves carry empty `takes` and `variants`. */
export interface CommandVariantWire {
  readonly word: string;
  readonly parameters: ParameterWire[];
  readonly takes: string[];
  readonly variants: readonly CommandVariantWire[];
}

/** The vocabulary as it crosses. Mirrored in `vocabulary.ts`, which is where the client keeps it. */
export interface MetaCommandWire {
  readonly name: string;
  readonly implemented: boolean;
  readonly summary: string;
  /** The words that may follow, when only certain ones may. Empty when the command takes any. */
  readonly takes: string[];
  /** One entry per word in `takes` that has its own signature; nested words follow the same shape. */
  readonly variants: readonly CommandVariantWire[];
  readonly open: boolean;
  readonly parameters: ParameterWire[];
}

export interface ProviderWire {
  readonly kind?: string;
  readonly target?: string;
  readonly endpoint?: string | null;
  readonly name: string;
  readonly capabilities: CapabilityWire[];
  /** The credential it needs, by name. Empty when it needs none. The value never crosses. */
  readonly credentials: { readonly name: string; readonly supplied: boolean }[];
  readonly ready: boolean;
}

export interface CapabilityWire {
  readonly path: string[];
  readonly summary: string;
  readonly result: string;
  readonly safe: boolean;
  readonly parameters: ParameterWire[];
}

export interface ParameterWire {
  readonly name: string;
  readonly type: string;
  readonly required: boolean;
  readonly allowed: string[];
  /** The contract's declared values, when the parameter has a finite domain. */
  readonly choices?: ChoicesWire;
  /** What language the value is written in, or empty when it is only a value. */
  readonly content: string;
}

export interface ChoicesWire {
  readonly kind: "text" | "int" | "decimal" | "bool";
  readonly members: string[];
  readonly total: number;
  readonly complete: boolean;
}

/** What could go inside a value written in a language of its own. Asked for, not pushed. */
export interface Suggested {
  readonly from: number;
  readonly items: SuggestedItem[];
}

export interface SuggestedItem {
  readonly text: string;
  readonly kind: string;
  readonly detail: string;
}

export type Request =
  | { readonly request: "work-history"; readonly cell: string; readonly client: string; readonly run?: string }
  | { readonly request: "protect-run"; readonly cell: string; readonly client: string; readonly run: string }
  | { readonly request: "delete-work-preview"; readonly cell: string; readonly client: string }
  | { readonly request: "delete-work"; readonly token: string; readonly client: string; readonly dependents: boolean; readonly protected: boolean }
  | { readonly request: "environmentsecret"; readonly reference: string; readonly value: string }
  | { readonly request: "environmentforget"; readonly reference: string }
  | { readonly request: "environmentgrant"; readonly environment: string; readonly revision: string; readonly provider: string; readonly seconds: number }
  | { readonly request: "environmentrevoke"; readonly environment: string; readonly revision: string; readonly provider: string }
  | { readonly request: "environmenttransfer"; readonly origin: string; readonly destination: string; readonly revision: string; readonly seconds: number }
  /** `cell` names one attempt at running a cell, not the cell — re-running it is a new attempt. */
  | { readonly request: "submit"; readonly console?: boolean; readonly cell: string; readonly text: string; readonly document?: { readonly source: string }; readonly client: string; readonly environments?: EnvironmentContext; readonly revision_of?: string; readonly repeat?: string; readonly acknowledge_effects?: boolean; readonly from?: string }
  | { readonly request: "release-preview"; readonly handle: string; readonly client: string }
  | { readonly request: "release"; readonly token: string; readonly client: string }
  | { readonly request: "cancel-work"; readonly origin: string }
  | { readonly request: "cancel"; readonly node: string }
  /**
   * A credential, and the one request that does not go through the command line.
   *
   * Everything typed into the console is echoed back to every client in `created`, kept in this
   * client's history and stored on the node that ran it. A credential must be in none of those, so it
   * gets its own door — and travels one way only: the engine never writes it back.
   */
  | { readonly request: "secret"; readonly name: string; readonly value: string }
  /** Keep this result past the session. By handle, because a node that ran again has other bytes. */
  | { readonly request: "keep"; readonly handle: string }
  | { readonly request: "keeping"; readonly automatic: boolean; readonly under: number }
  /** Where the engine keeps things. Answered with a `storage` event, which every client sees. */
  | { readonly request: "storage" }
  /**
   * Answers a command that is waiting for one. Never written down anywhere — a prompt asking for a
   * password is an ordinary use of this, not an edge case.
   */
  | { readonly request: "input"; readonly node: string; readonly run: string; readonly text: string }
  | { readonly request: "eof"; readonly node: string; readonly run: string };

/**
 * A value as the data plane stores it: what it is, how it was obtained, and what it holds.
 *
 * The type travels structurally rather than as a name, because a client that wants to lay a record out
 * as a table needs to know its fields — a label would send it back to the engine to ask.
 */
export interface StoredValue {
  readonly type: TypeShape;
  readonly provenance: Record<string, string>;
  readonly data: unknown;
  /** Contract metadata beside the value; absent means unknown. Decoded by `withValidMeta`. */
  readonly meta?: import("./value-meta").ValueMeta;
}
export type Retention = "temporary" | "automatic" | "protected" | "unknown";
export interface ReleasePreview {
  readonly token: string; readonly handle: string; readonly retention: Retention;
  readonly nodes: readonly string[]; readonly downstream: readonly string[];
  readonly workspaces: readonly string[]; readonly expiresInSeconds: number;
}

export interface WorkBlocker {
  readonly node: string | null;
  readonly cells: readonly string[];
  readonly state: string;
  readonly reason: string;
}

export interface WorkPreview {
  readonly token: string;
  readonly cells: readonly string[];
  readonly nodes: readonly string[];
  readonly dependents: readonly string[];
  readonly labels: Readonly<Record<string, string>>;
  readonly payloads: readonly string[];
  readonly protected: readonly string[];
  readonly sharedWorkspaces: readonly string[];
  readonly expiresInSeconds: number;
}

export type TypeShape =
  | { readonly kind: "meta"; readonly name: string }
  | { readonly kind: "primitive"; readonly name: string }
  | { readonly kind: "list"; readonly element: TypeShape }
  | { readonly kind: "option"; readonly element: TypeShape }
  | { readonly kind: "iter"; readonly element: TypeShape; readonly contract?: string }
  /** A committed dataset extent: its data is a descriptor, never the records, and it is not a List. */
  | { readonly kind: "dataset"; readonly element: TypeShape }
  | { readonly kind: "record"; readonly name: string; readonly fields: TypeField[] }
  | { readonly kind: "unknown" };

export interface TypeField {
  readonly name: string;
  readonly type: TypeShape;
}

/** How a type reads on screen. The engine has the same rendering; this is the client's copy of it. */
export function describeType(shape: TypeShape | undefined): string {
  // A value is read off the wire with a cast, so a shape the client has never seen is not a bug in
  // the sender — it is a newer engine, or a field that was simply not sent. Both read as unknown.
  switch (shape?.kind) {
    case "primitive":
      return shape.name.charAt(0) + shape.name.slice(1).toLowerCase();
    case "iter":
      return `Iter<${describeType(shape.element)}>`;
    case "dataset":
      return `Dataset<${describeType(shape.element)}>`;
    case "option":
      return `Option<${describeType(shape.element)}>`;
    case "list":
      return `List<${describeType(shape.element)}>`;
    case "record":
      return shape.name === "" ? "{…}" : shape.name;
    case "unknown":
    default:
      return "Unknown";
  }
}

/** Bytes, said the way a person reads them. */
export function describeSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export interface WorkRun {
  readonly node: string; readonly run: string; readonly state: string; readonly at: string;
  readonly handle: string | null; readonly protected: boolean; readonly trace: boolean;
}
export interface WorkHistory {
  readonly attempts: readonly { readonly id: string; readonly source: string; readonly revisionOf: string | null;
    readonly repeatOf: string | null; readonly nodes: readonly string[]; readonly failure: string | null; readonly diagnostics: readonly string[] }[];
  readonly runs: readonly WorkRun[]; readonly unconfirmedWrites: string;
}
export interface WorkRunDetail {
  readonly canProtect: boolean;
  readonly error: Pick<ErrorRecord, "code" | "message" | "issues" | "locations"> | null;
  readonly run: WorkRun; readonly definition: { readonly text: string; readonly [field: string]: unknown };
  readonly trace: StoredValue | null; readonly contextNote: string; readonly traceNote: string;
}
