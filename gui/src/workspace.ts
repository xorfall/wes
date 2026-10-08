import type { Event, NodeState } from "./protocol";
import { appendTranscript } from "./transcripts";
import { emptyCatalogue, type Catalogue } from "./vocabulary";

/**
 * What the client knows, built only from what the engine said.
 *
 * The engine owns workspace state. This is a projection of the event
 * stream and nothing more — which is why reconnecting has to replay or refetch rather than patch, and
 * why nothing here ever guesses at a state it was not told about.
 */

export interface WorkspaceNode {
  readonly waiting?:readonly import("./protocol").WaitingInput[];
  readonly errorNames?:readonly string[];
  readonly streamOutput?: boolean;
  /** The engine's statement that this node's own call streams; absent means not stated, never inferred. */
  readonly streamSource?: boolean;
  readonly staleReason?: import("./protocol").StaleReason;
  /** `creation` input edges order one construction; absent until the engine announces the node. */
  readonly dependencyLifetime?: import("./protocol").DependencyLifetime;
  /** The engine's own statement that a captured node's current run took its inputs; see `inputsCaptured`. */
  readonly inputsCaptured?: true;
  /** The engine's statement that a creation-lifetime node finished constructing. */
  readonly constructionComplete?: boolean;
  /** The engine's statement that newer committed input waits for this calculation; never inferred. */
  readonly updatePending?: boolean;
  /**
   * The engine's display-only value of a terminal node: a stopped stream's last value, or the
   * committed partial result of an analysis that did not complete. The graph state is unchanged.
   */
  readonly evidence?: Evidence;
  /** The latest lossy progress of this node's current run only; never carried across runs. */
  readonly progress?: { readonly run: string; readonly value: import("./protocol").ExecutionProgress };
  readonly retention?: import("./protocol").Retention;
  readonly repeatable?: boolean;
  readonly publication?: import("./protocol").ResultPublication;
  readonly traced?: boolean;
  readonly private?: boolean;
  readonly confidential?: boolean;
  readonly residence?: "memory" | "temporary" | "retainable";
  readonly environment?: Extract<Event, { event: "node-environment" }>;
  readonly id: string;
  readonly startedAt?: string;
  readonly name?: string;
  readonly command: string;
  readonly currentDefinition?: string;
  readonly dependsOn: readonly string[];
  readonly state: NodeState;
  readonly type?: string;
  readonly handle?: string;
  readonly bytes?: number;
  readonly provenance: Record<string, string>;
  /** What was overridden to produce this. A result nobody can see a warning on is unwarned. */
  readonly cautions: readonly string[];
  /** Whether this survives the engine restarting. Undefined until it has a result at all. */
  readonly kept: boolean;
  readonly failure?: string;
  readonly failureRecord?: import("./protocol").ErrorRecord;
  readonly cancellation?: { readonly code: string; readonly reason: string };
  /** Whether somebody can type at it while it runs. */
  readonly interactive?: boolean;
  /** What it has written so far, for a command that asks before it finishes. */
  readonly wrote?: string;
  readonly run?: string;
  readonly conversationActive?: boolean;
  readonly outputLost?: boolean;
  readonly wroteTrimmed?: boolean;
  /**
   * A call that was in flight when the engine stopped, if this node was making one.
   *
   * Not a failure and not staleness. The node's value is gone, which staleness says; what staleness
   * cannot say is that asking again might be asking twice.
   */
  readonly doubt?: Doubt;
  /**
   * The engine withdrew access to this node's result. Its value, handle and every value-derived
   * fact are gone; only a later run's own new result is readable again.
   */
  readonly accessWithdrawn?: true;
  /**
   * The recording commands the engine said this node's admitted writer accepts, bound to the run it
   * said it for. It offers a reviewed command only; any other run, or a withdrawal, has none.
   */
  readonly recordingControl?: import("./protocol").RecordingControl & { readonly run: string };
  /**
   * The local reconciliation the engine said this node's original run accepts, bound to that run.
   * It offers a reviewed command only; another run, a later announcement without it, or a
   * withdrawal has none.
   */
  readonly reconciliationControl?: import("./protocol").ReconciliationControl & { readonly run: string };
  /**
   * Present only while the engine says this node's owned run (a recording or a followed scan) is
   * still open, bound to that run. Its value stays an immutable acknowledged prefix; nothing here
   * reads it again or grants any action. Withdrawal, a terminal state or another run clears it.
   */
  readonly openLifetime?: { readonly run: string };
}

/** A recording control as the engine stated it for one exact run, or nothing. */
const recordingControlOf = (event: Extract<import("./protocol").Event, { event: "created" }>) =>
  event.recordingControl && typeof event.run === "string" && event.run !== "" ? { ...event.recordingControl, run: event.run } : undefined;
/** A reconciliation control as the engine stated it for one exact original run, or nothing. */
const reconciliationControlOf = (event: Extract<import("./protocol").Event, { event: "created" }>) =>
  event.reconciliationControl && typeof event.run === "string" && event.run !== "" ? { ...event.reconciliationControl, run: event.run } : undefined;
/** An open lifetime as the engine stated it for one exact run; false, null or no run is none. */
const openLifetimeOf = (event: Extract<import("./protocol").Event, { event: "created" }>) =>
  event.lifetimeActive === true && typeof event.run === "string" && event.run !== "" ? { run: event.run } : undefined;

export interface Evidence {
  readonly kind: import("./protocol").EvidenceKind;
  readonly source: string;
  readonly run: string;
}
/** A stopped stream's last value. Only this kind is ever offered a restart. */
export const stoppedStream = (node: WorkspaceNode | undefined): Evidence | undefined =>
  node?.evidence?.kind === "stopped_stream" ? node.evidence : undefined;
/** An analysis that stopped before completing, with its committed partial result. */
export const incompleteResult = (node: WorkspaceNode | undefined): Evidence | undefined =>
  node?.evidence?.kind === "incomplete" ? node.evidence : undefined;
/** The progress of the node's current run, when the engine reported it for that exact run. */
export const currentProgress = (node: WorkspaceNode | undefined): import("./protocol").ExecutionProgress | undefined =>
  node?.progress && node.run !== undefined && node.progress.run === node.run ? node.progress.value : undefined;

/** What is known about a call nobody can answer for. */
export interface Doubt {
  readonly capability: string;
  readonly safe: boolean;
  readonly when: string;
}

/** What this session keeps without being asked, as the engine last said it. */
export interface Keeping {
  readonly automatic: boolean;
  readonly under: number;
}

export interface Workspace {
  readonly capacity?: import("./protocol").ExecutionCapacity;
  readonly identity?: Extract<Event, { event: "workspace-context" }>;
  readonly displayProblem?: string;
  readonly logStatus?: import("./protocol").LogStatus;
  readonly startupWarnings: readonly import("./protocol").StartupWarning[];
  readonly history: readonly import("./protocol").HistoryEvent[];
  readonly nodes: readonly WorkspaceNode[];
  /** What can be said now. Replaced wholesale when the engine says it changed. */
  readonly catalogue: Catalogue;
  readonly keeping: Keeping;
  /** Where the engine keeps things, once it has been asked. Absent until then. */
  readonly storage?: Extract<Event, { event: "storage" }>;
  /** Which nodes each attempt produced, so a cell can find what it made. */
  readonly cells: Readonly<Record<string, readonly string[]>>;
  readonly repeatedRuns?: Readonly<Record<string, string>>;
  readonly attemptFailures?: Readonly<Record<string, string>>;
  /**
   * Nodes that went stale since anybody last looked.
   *
   * Kept separately from the nodes themselves because staleness is two different things at once: a
   * property of a node, which the node carries, and something that just happened, which nothing else
   * announces. A cell scrolled off the screen can be marked perfectly and still tell nobody.
   */
  readonly wentStale: readonly string[];
}

export const emptyWorkspace: Workspace = {
  startupWarnings: [],
  history: [],
  nodes: [],
  catalogue: emptyCatalogue,
  /** The engine's own default, so the panel is not wrong for the moment before the greeting lands. */
  keeping: { automatic: true, under: 10 * 1024 * 1024 },
  cells: {},
  wentStale: [],
};

/**
 * Folds one event into what is known.
 *
 * A node is only created by a `created` event. A state change for an unknown node is ignored rather
 * than inventing a node with no command and no edges — a half-node on screen would be worse than a
 * missing one, and its absence is a visible symptom if the engine ever stops announcing.
 */
export function apply(workspace: Workspace, event: Event): Workspace {
  switch (event.event) {
    case "workspace-closed": return emptyWorkspace;
    case "workspace-context": return { ...workspace, identity: event };
    case "work-retired": {
      const gone = new Set(event.cells);
      return { ...workspace, cells: Object.fromEntries(Object.entries(workspace.cells).filter(([cell]) => !gone.has(cell))) };
    }
    case "session":
      return emptyWorkspace;
    case "environments":
    case "storage-warning": return workspace;
    case "node-environment": return { ...workspace, nodes: workspace.nodes.map(node => node.id === event.node ? { ...node, environment: event } : node) };
    case "projection-unavailable":
      return { ...emptyWorkspace, displayProblem: event.message };
    case "log-delta": {
      const removed = new Set(event.removed);
      const entries = new Map(event.entries.map(entry => [entry.record.id, entry]));
      const history = event.reset ? [] : workspace.history.flatMap(entry => {
        if (removed.has(entry.record.id)) return [];
        const replacement = entries.get(entry.record.id);
        entries.delete(entry.record.id);
        return [replacement ?? entry];
      });
      history.push(...entries.values());
      return { ...workspace, history };
    }
    case "execution-capacity": return { ...workspace, capacity: event };
    case "log-status":
      return { ...workspace, logStatus: event };
    case "log":
    case "log-notice":
    case "log-diagnostic":
      return { ...workspace, history: workspace.history.some(entry => entry.record.id === event.record.id)
        ? workspace.history.map(entry => entry.record.id === event.record.id ? event : entry)
        : [...workspace.history, event] };
    case "startup-warning":
      return { ...workspace, startupWarnings: [...workspace.startupWarnings.filter(warning => warning.id !== event.id), event] };
    /*
     * Nothing to keep. A diagnostic belongs to the cell whose command it is about, and that is where it
     * is drawn. Do not accumulate a second, unbounded copy in the workspace projection.
     */
    case "reported":
      return workspace;

    case "storage":
      return { ...workspace, storage: event };

    /*
     * Appended, not replaced. What a command wrote is a transcript — the question, then the answer's
     * echo, then whatever came next — and a client that kept only the last chunk would show the tail of
     * a conversation with the question missing.
     */
    case "output":
      return {
        ...workspace,
        nodes: appendTranscript(workspace.nodes, event),
      };
    case "conversation":
      return change(workspace, event.node, node => ({ ...node, run: event.run || undefined,
        conversationActive: event.active,
        ...(node.run !== (event.run || undefined) ? { wrote: undefined, outputLost: false, wroteTrimmed: false } : {}),
      }));
    case "output-gap":
      return { ...workspace, nodes: workspace.nodes.map(node => node.interactive && node.run
        ? { ...node, outputLost: true } : node) };

    case "vocabulary":
      return {
        ...workspace,
        catalogue: {
          commands: event.commands,
          calculation: event.calculation,
          annotations: event.annotations,
          providers: event.providers,
          templates: event.templates ?? [],
        },
      };

    case "created":
      if (workspace.nodes.some(node => node.id === event.node)) {
        return change(workspace, event.node, node => ({ ...node, name: event.name || undefined, errorNames:event.errorNames,
          // Progress belongs to one run; a new run never inherits an older run's counters.
          ...(node.progress && node.progress.run !== (event.run ?? undefined) ? { progress: undefined } : {}),
          command: event.command, currentDefinition: event.currentDefinition ?? undefined, run: event.run ?? undefined, dependsOn: event.dependsOn, dependencyLifetime: event.dependencyLifetime, inputsCaptured: event.inputsCaptured === true || undefined, streamOutput: event.streamOutput, streamSource: event.streamSource === true, traced: event.traced, interactive: event.interactive,
          repeatable: event.repeatable, startedAt: event.startedAt ?? undefined,
          // Replaced by every announcement: absent or null now means no controls, whatever was said before.
          recordingControl: recordingControlOf(event), reconciliationControl: reconciliationControlOf(event), openLifetime: openLifetimeOf(event) }));
      }
      return {
        ...workspace,
        nodes: [
          ...workspace.nodes,
          {
            id: event.node,
            errorNames:event.errorNames,
            repeatable: event.repeatable,
            startedAt: event.startedAt ?? undefined,
            name: event.name === "" ? undefined : event.name,
            command: event.command,
            currentDefinition: event.currentDefinition ?? undefined,
            run: event.run ?? undefined,
            streamOutput: event.streamOutput,
            streamSource: event.streamSource === true,
            dependsOn: event.dependsOn,
            dependencyLifetime: event.dependencyLifetime,
            ...(event.inputsCaptured === true ? { inputsCaptured: true as const } : {}),
            state: "pending",
            traced: event.traced, interactive: event.interactive,
            provenance: {},
            cautions: [],
            kept: false,
            ...(recordingControlOf(event) ? { recordingControl: recordingControlOf(event) } : {}),
            ...(reconciliationControlOf(event) ? { reconciliationControl: reconciliationControlOf(event) } : {}),
            ...(openLifetimeOf(event) ? { openLifetime: openLifetimeOf(event) } : {}),
          },
        ],
      };

    case "node":
      if (event.state !== "stale") workspace = { ...workspace, wentStale: workspace.wentStale.filter(id => id !== event.node) };
      if (event.state === "stale" && !workspace.wentStale.includes(event.node)) {
        workspace = { ...workspace, wentStale: [...workspace.wentStale, event.node] };
      }
      return change(workspace, event.node, (node) => ({ ...node, state: event.state, waiting:event.waiting,
        staleReason: event.state === "stale" ? event.staleReason : undefined,
        // A new run's own state ends the withdrawal notice; it brings no old value back.
        accessWithdrawn: event.state === "stale" ? node.accessWithdrawn : undefined,
        updatePending: event.updatePending === true || undefined,
        constructionComplete: event.constructionComplete || undefined,
        ...(node.evidence ? { handle: undefined, bytes: undefined, kept: false } : {}),
        evidence: undefined,
        publication: event.publication,
        failure: undefined, failureRecord: undefined, cancellation: undefined,
        ...(event.state === "ready" && event.publication?.state !== "available"
          ? { handle: undefined, bytes: undefined, kept: false } : {}),
        ...(event.state === "skipped" ? { handle: undefined, bytes: undefined, kept: false } : {}),
      }));

    case "result-access":
      // Arrives before the node's stale state. Drop what the value told us; keep the node, its
      // command and its actions. Nothing reruns.
      return change(workspace, event.node, node => ({ ...node, accessWithdrawn: true, handle: undefined, bytes: undefined,
        type: undefined, provenance: {}, cautions: [], kept: false, retention: undefined, publication: undefined, evidence: undefined, progress: undefined,
        recordingControl: undefined, reconciliationControl: undefined, openLifetime: undefined }));

    case "node-progress":
      // Lossy status for the exact current run only: a late report from an older run is not this run's,
      // and a withdrawn result's run reports nothing until a new run's own state ends the withdrawal.
      return change(workspace, event.node, node => event.run !== null && node.run === event.run && !node.accessWithdrawn
        ? { ...node, progress: { run: event.run, value: event.progress } } : node);

    case "evidence":
    case "ready":
      workspace = { ...workspace, wentStale: workspace.wentStale.filter((id) => id !== event.node) };
      return change(workspace, event.node, (node) => ({
        ...node,
        // Evidence keeps the engine's terminal state; it is never promoted to ready.
        state: event.event === "evidence" ? event.state : "ready",
        staleReason: undefined, waiting:undefined, updatePending: undefined,
        constructionComplete: event.constructionComplete === true || undefined,
        evidence: event.event === "evidence" ? { kind: event.kind, source: event.source, run: event.run } : undefined,
        publication: event.publication,
        type: event.type,
        handle: event.handle,
        bytes: event.bytes,
        provenance: event.provenance,
        cautions: event.cautions,
        kept: event.kept,
        retention: event.retention,
        accessWithdrawn: undefined,
        private: event.private,
        confidential: event.confidential,
        residence: event.residence,
        // An incomplete analysis is still a failure; its record is the engine's, beside the partial value.
        failure: event.event === "evidence" && event.kind === "incomplete" ? event.reason ?? event.error?.message : undefined,
        failureRecord: event.event === "evidence" && event.kind === "incomplete" ? event.error : undefined,
        cancellation: undefined,
        doubt: undefined,
        // Ready keeps an open lifetime (a usable prefix of a run still open); evidence is terminal.
        ...(event.event === "evidence" ? { openLifetime: undefined } : {}),
      }));

    case "keeping":
      return { ...workspace, keeping: { automatic: event.automatic, under: event.under } };

    case "planned":
      return {
        ...workspace,
        cells: { ...workspace.cells, [event.cell]: event.nodes },
        repeatedRuns: event.repeatedRun ? { ...workspace.repeatedRuns, [event.cell]: event.repeatedRun } : workspace.repeatedRuns,
        attemptFailures: event.failure ? { ...workspace.attemptFailures, [event.cell]: event.failure } : workspace.attemptFailures,
      };

    case "interrupted":
      return change(workspace, event.node, (node) => ({
        ...node,
        doubt: { capability: event.capability, safe: event.safe, when: event.when },
      }));

    case "failed":
      workspace = { ...workspace, wentStale: workspace.wentStale.filter(id => id !== event.node) };
      return change(workspace, event.node, (node) => ({
        ...node,
        state: "failed",
        staleReason: undefined, waiting:undefined, updatePending: undefined, constructionComplete: undefined,
        evidence: undefined,
        publication: undefined,
        failure: event.reason,
        failureRecord: event.error,
        cancellation: undefined,
        handle: undefined, bytes: undefined, kept: false,
        // A terminal state ends the run's lifetime whatever the last announcement said.
        openLifetime: undefined,
      }));

    case "cancelled":
      workspace = { ...workspace, wentStale: workspace.wentStale.filter(id => id !== event.node) };
      return change(workspace, event.node, (node) => ({
        ...node, state: "cancelled", staleReason: undefined, updatePending: undefined, constructionComplete: undefined, failure: undefined, failureRecord: undefined, evidence: undefined,
        publication: undefined,
        cancellation: { code: event.code, reason: event.reason },
        handle: undefined, bytes: undefined, kept: false,
        openLifetime: undefined,
      }));

    case "dropped": {
      const gone = new Set(event.nodes);
      return { ...workspace, nodes: workspace.nodes.filter((node) => !gone.has(node.id)), wentStale: workspace.wentStale.filter(id => !gone.has(id)) };
    }
  }
}

function change(
  workspace: Workspace,
  id: string,
  update: (node: WorkspaceNode) => WorkspaceNode,
): Workspace {
  if (!workspace.nodes.some((node) => node.id === id)) {
    return workspace;
  }
  return {
    ...workspace,
    nodes: workspace.nodes.map((node) => (node.id === id ? update(node) : node)),
  };
}

/** Whether anything about this result was overridden on the way. */
export function isCautioned(node: WorkspaceNode): boolean {
  return node.cautions.length > 0;
}

/**
 * Every way a result can be referred to.
 *
 * Both a result's user-assigned name and its stable node id resolve to that result, so completion
 * must offer both.
 */
export function referables(nodes: readonly WorkspaceNode[]): readonly string[] {
  const names: string[] = [];
  nodes.forEach((node) => {
    if (node.name !== undefined) {
      names.push(node.name);
    }
    names.push(node.id);
  });
  return names;
}

/** What to call a node on screen: the name someone gave it, or the id it always had. */
export function label(node: WorkspaceNode): string {
  return node.name ?? node.id;
}

/** Says the person has seen what went stale, so the announcement stops and the marks stay. */
export function seen(workspace: Workspace): Workspace {
  return workspace.wentStale.length === 0 ? workspace : { ...workspace, wentStale: [] };
}

/** Which cell produced a node, for an announcement that can take somebody to it. */
export function cellOf(workspace: Workspace, node: string): string | undefined {
  for (const [cell, nodes] of Object.entries(workspace.cells)) {
    if (nodes.includes(node)) {
      return cell;
    }
  }
  return undefined;
}

/** Stale because newer committed input superseded a bounded observation, not because a definition or restore changed. */
const OBSERVATION_STALE = new Set(["stream_updated", "input_behind"]);

export function observationStale(node: WorkspaceNode): boolean {
  return node.state === "stale" && OBSERVATION_STALE.has(node.staleReason?.code ?? "");
}

export const UPDATE_PENDING_STATUS = "Newer input waiting; finishing current calculation";

/** Only the engine's bit; no count, since the engine keeps one pending update, not a backlog. */
export function updatePendingStatus(node: WorkspaceNode): string | undefined {
  return node.updatePending ? UPDATE_PENDING_STATUS : undefined;
}

/** A node whose input edges only ordered its one construction, which has succeeded. */
export function constructed(node: WorkspaceNode): boolean {
  return node.dependencyLifetime === "creation" && node.constructionComplete === true;
}

/**
 * A captured-input node (an analysis) whose current run has actually taken its inputs at guarded
 * entry, as the engine states it: later producer updates then no longer make it out of date. A run
 * identity alone proves nothing — a spawned run that has not entered has one.
 */
export function inputsCaptured(node: WorkspaceNode): boolean {
  return node.dependencyLifetime === "captured" && node.inputsCaptured === true;
}

/** No inference from retry history or private result metadata. */
export function staleMessage(node: WorkspaceNode): string | undefined {
  return node.state === "stale" ? node.staleReason?.message ?? "The reason this result became stale was not recorded." : undefined;
}
