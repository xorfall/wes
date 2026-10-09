import { withValidMeta } from "./value-meta";
import { validDisplayMetadata, DisplayReadError, liveReader, type DisplayRead, type DisplayMetadata } from "./live-view-reader";
import {ValuePackages} from "./value-views/external/discovery";
import {bindValueViews,builtinValueViews} from "./value-views/registry";
import {loadFramePackages} from "./value-views/external/load";
import type {ViewAsset} from "./value-views/external/document";
import { readExactJson, parseExactJson, stringifyExactJson } from "./exact-json";
import { mergeViewFrame } from "./value-views/instances";
import { ViewMounts } from "./value-views/mounts";
import { max_active_view_roots, max_view_frame_instances } from "./value-views/limits";
import type { InputPatches } from "./value-views/instances";
import type { SharedState, SharedCommit } from "./value-views/shared-state";
import { ViewFrameReader, type ViewFrame, type FrameSample, type ViewInstance } from "./value-views/instances";
import { freshPinName, pinCommand } from "./value-views/pin";
import { WorkspaceEvents } from "./workspace-events";
import { decodeEvent } from "./protocol-decode";
import { diagnosticsSession, clientDiagnostic, timeClientSubmit } from "./local-telemetry";
import { TerminalUnavailable, TerminalRequestError } from "./terminal-errors";
import { EngineRefusal, SUBMISSION_OUTCOME_HEADER, recordEngineFailure, recordExecutionFailure, requestOperation } from "./engine-diagnostics";
import { StorageError, storageError } from "./storage-error";
import { AssistantUi } from "./assistant-ui";
import { ResultReader, ResultWithdrawnError } from "./result-reader";
import { datasetWithdrawals } from "./surface/render/dataset-source";
import { DatasetReadError, datasetFailure, datasetQuery, decodeDatasetHead, decodeDatasetRead, extendsReference, type DatasetPosition, type DatasetRead, type DatasetStream } from "./dataset-read";
import type { DatasetReference } from "./presentation/dataset";
import type { ViewDatasetBinding } from "./value-views/view-datasets";
import { workspaceHeaders, workspaceName } from "./workspace-binding";
import type { Event, Request, StoredValue, Suggested, SavedHistoryPage } from "./protocol";

export type Connection = "connecting" | "connected" | "reconnecting";
/** Stored handles remembered per node for access notices: current plus a few recent runs. */
const HANDLES_PER_NODE = 4;
/** Largest dataset reply read, as the server bounds it. */
const DATASET_REPLY_BYTES = 1024 * 1024;

export interface SandboxObservation {
  readonly name: string; readonly generation: string; readonly reference: string | null;
  readonly inspect: boolean | null; readonly value: StoredValue;
  readonly state: "active" | "stopped" | "not run" | "removed";
}
export interface EnvironmentDocument {
  readonly name: string;
  readonly environments: readonly string[];
  readonly source: string;
  readonly origin: string;
}

/**
 * The engine, from this side of the boundary.
 *
 * Everything this client can do is here, and it is short because the protocol is. There is no way to
 * reach past it — no shared object, no second channel — which is the whole point of the engine running
 * in its own process.
 */
export class Engine {

  private sandboxListeners = new Set<(cell: string, result: SandboxObservation) => void>();
  onSandbox(listener: (cell: string, result: SandboxObservation) => void): () => void {
    this.sandboxListeners.add(listener);
    return () => { this.sandboxListeners.delete(listener); };
  }
  async readSandbox(reference: string, inspect: boolean, generation: string, signal: AbortSignal): Promise<SandboxObservation> {
    if (generation !== this.generation) throw new Error("Workspace changed; reopen the sandbox.");
    const response = await fetch(`/sandbox?${new URLSearchParams({ reference, inspect: String(inspect) })}`, {
      headers: this.headers({ "X-Wes-Session": generation }), cache: "no-store", signal,
    });
    if (!response.ok) throw new Error(await response.text() || "Sandbox observation is unavailable.");
    const result = (await readExactJson<{sandbox:SandboxObservation}>(response)).sandbox as SandboxObservation;
    if (generation !== this.generation || result.generation !== generation) throw new Error("Workspace changed; sandbox observation discarded.");
    return result;
  }
  readonly assistantUi = new AssistantUi();
  private uiGeneration?: string;
  /** The live workspace an unbound engine was greeted with; never guessed. */
  private observedWorkspace?: string;
  private workspaceListeners = new Set<() => void>();
  /**
   * The workspace this engine's views belong to: the explicit binding, otherwise the name announced
   * by the current session. Undefined while unknown, closed or disconnected.
   */
  viewWorkspaceName(): string | undefined { return this.binding ?? this.observedWorkspace; }
  /** Notifies only when {@link viewWorkspaceName} actually changes. */
  onViewWorkspace(listener: () => void): () => void {
    this.workspaceListeners.add(listener);
    return () => { this.workspaceListeners.delete(listener); };
  }
  private observeWorkspace(name: string | undefined): void {
    const before = this.viewWorkspaceName();
    this.observedWorkspace = name;
    if (this.viewWorkspaceName() !== before) this.workspaceListeners.forEach(listener => listener());
  }
  // A server refusal of a withdrawn handle reaches the same shared gate as an access notice, so
  // headers, peeks and windows holding the value drop it even without a notice.
  private readonly results = new ResultReader(() => this.generation, () => this.timeout, () => this.binding,
    (handle, generation) => { if (generation) datasetWithdrawals.withdrawMany([handle], generation); });
  constructor(readonly binding?: string, client?: string) {
    if (binding !== undefined && !workspaceName(binding)) throw new Error("Invalid workspace binding");
    if (client !== undefined) this.client = client;
  }
  private headers(extra: Record<string, string> = {}): Record<string, string> { return { ...workspaceHeaders(this.binding), ...extra }; }
  async environmentDocuments(signal?: AbortSignal): Promise<readonly EnvironmentDocument[]> {
    const generation = this.generation;
    if (!generation) throw new Error("Wait for the workspace connection before opening environment documents.");
    const response = await fetch("/environment-documents", { headers: this.headers(), signal });
    if (!response.ok) throw new Error((await response.text()) || "Could not load environment documents.");
    const body = await readExactJson(response) as { generation?: string; documents?: EnvironmentDocument[] };
    if (generation !== this.generation || body.generation !== generation) throw new Error("Workspace changed. Reopen /edit env.");
    if (!Array.isArray(body.documents) || body.documents.some(d => typeof d.name !== "string" || typeof d.source !== "string" || typeof d.origin !== "string" || !Array.isArray(d.environments))) throw new Error("Invalid environment document response.");
    return body.documents;
  }
  readonly client: string = crypto.randomUUID();
  async trace(node: string, signal?: AbortSignal): Promise<StoredValue | undefined> {
    const generation = this.generation;
    if (!generation) throw new Error("Wait for the workspace connection.");
    const response = await fetch(`/traces/${encodeURIComponent(node)}`, { headers: this.headers({ "X-Wes-Session": generation }), signal, cache: "no-store" });
    if (generation !== this.generation) throw new Error("Workspace changed; inspection was discarded.");
    if (response.status === 404) return undefined;
    if (!response.ok) throw new Error("HTTP inspection is unavailable.");
    return withValidMeta(await readExactJson(response) as StoredValue);
  }
  async terminal<T = Record<string, unknown>>(request: Record<string, unknown>, signal?: AbortSignal): Promise<T> {
    if (!this.generation) throw new Error("Wait for the workspace connection.");
    const generation = this.generation;
    const deadline = AbortSignal.timeout(15_000);
    const operation = String(request.action ?? "request");
    try {
      const response = await fetch("/terminals", {
        method: "POST", headers: this.headers({ "Content-Type": "application/json", "X-Wes-Session": this.generation }),
        body: stringifyExactJson({ ...request, client: this.client }), signal: signal ? AbortSignal.any([signal, deadline]) : deadline,
      });
      if (generation !== this.generation) throw new Error("Workspace changed; terminal reply was discarded.");
      if (response.status === 410) throw new TerminalUnavailable();
      if (!response.ok) throw new TerminalRequestError(`TERM_HTTP_${response.status}`, "Terminal server", operation, `Terminal server refused ${operation} (HTTP ${response.status}).`, (await response.text()) || undefined);
      const result = await readExactJson(response) as T;
      if (generation !== this.generation) throw new Error("Workspace changed; terminal reply was discarded.");
      return result;
    } catch (error) {
      // An owning component's disposal is handled by its lifetime guard. Browser timeout text
      // varies (WebKit can report AbortError), so use the deadline's actual state first.
      if (signal?.aborted) throw error;
      if (deadline.aborted) throw new TerminalRequestError("TERM_TIMEOUT", "Browser → terminal server", operation, "Terminal request timed out after 15 seconds.", "No complete response arrived before the request deadline.");
      if (error instanceof DOMException && error.name === "AbortError") throw new TerminalRequestError("TERM_ABORTED", "Browser → terminal server", operation, "Terminal connection was interrupted.", "The browser aborted the request; the underlying reason was not provided.");
      if (error instanceof TypeError) throw new TerminalRequestError("TERM_NETWORK", "Browser → terminal server", operation, "Could not complete the terminal request.", error.message);
      throw error;
    }
  }
  async submitConsole(cell: string, text: string): Promise<void> {
    await this.send({ request: "submit", cell, text, client: this.client, environments: this.environmentContext(), console: true });
  }
  private environments?: Extract<Event, { event: "environments" }>;
  private vocabulary?: Extract<Event, { event: "vocabulary" }>;
  private sourceAttempts = new Map<string, Extract<Request, { request: "submit" }>>();
  private documentWaiters = new Map<string, { generation: string; resolve: (event: Extract<Event, { event: "planned" }>) => void; reject: (error: Error) => void }>();
  environmentContext(): import("./protocol").EnvironmentContext | undefined {
    if (!this.environments || (!this.environments.managed && !this.environments.clients[this.client])) return undefined;
    return this.environments.clients[this.client] ?? { selected: this.environments.default ?? null, revisions: this.environments.revisions };
  }
  private generation: string | undefined;
  private compositions = new Map<string, { generation: string | undefined; context: import("./protocol").EnvironmentContext | undefined }>();
  compose(text: string, scope = "default"): void {
    if (!text) this.compositions.delete(scope);
    else if (!this.compositions.has(scope)) this.compositions.set(scope, { generation: this.generation, context: this.environmentContext() });
  }
  captureComposition() { const context=this.environmentContext(); return { generation: this.generation, context: context && {...context,revisions:{...context.revisions}} }; }
  adoptComposition(captured: ReturnType<Engine["captureComposition"]>, scope = "prompt"): void {
    if (!captured.generation || captured.generation !== this.generation || JSON.stringify(captured.context) !== JSON.stringify(this.environmentContext())) throw new Error("Session or environment changed; prepare the command again.");
    this.compositions.set(scope, captured);
  }
  async prepareViewCommand(binding: import("./value-views/view-bindings").ViewBinding, template: string, arguments_: Readonly<Record<string, unknown>>, captured: ReturnType<Engine["captureComposition"]>, signal: AbortSignal): Promise<string> {
    if (binding.generation !== this.generation || captured.generation !== this.generation) throw new Error("View changed");
    const response = await fetch("/view-commands", { method: "POST", headers: this.headers({"X-Wes-Session": binding.generation, "Content-Type": "application/json"}),
      body: stringifyExactJson({root: binding.root, instance: binding.rootInstance, member: binding.member, revision: binding.revision, inputRevision: binding.inputRevision, template, arguments: arguments_, environments: captured.context}),
      signal: AbortSignal.any([signal, AbortSignal.timeout(2000)]), cache: "no-store" });
    if (!response.ok || binding.generation !== this.generation || signal.aborted) throw new Error("View command changed or is unavailable");
    const draft = await readExactJson(response) as {source?: unknown};
    if (typeof draft.source !== "string" || draft.source.length > 16 * 1024) throw new Error("Invalid command draft");
    return draft.source;
  }
  /** A retained editor and its returned prompt start with the same captured context. */
  copyComposition(from: string, to: string): void {
    const captured = this.compositions.get(from);
    this.compositions.delete(to);
    if (captured) this.compositions.set(to, captured);
  }
  moveComposition(from: string, to: string): void {
    this.copyComposition(from, to);
    this.compositions.delete(from);
  }
  compositionInfo(scope = "default") {
    const captured = this.compositions.get(scope);
    return captured && { ...captured, sessionChanged: captured.generation !== this.generation };
  }
  /** Explicit user review, never triggered by selection changes or a reconnect. */
  rebaseComposition(scope = "default"): void {
    if (!this.generation || !this.environments) throw new Error("Wait for the session and environment definitions before reviewing the draft.");
    const context = this.environmentContext();
    if (context && Object.entries(this.environments.revisions).some(([name, revision]) => context.revisions[name] !== revision)) {
      throw new Error("Select the current environment revision before reviewing the draft.");
    }
    if (this.compositions.has(scope)) this.compositions.set(scope, { generation: this.generation, context });
  }
  checkComposition(scope = "default"): void {
    const captured = this.compositions.get(scope);
    if (captured && captured.generation !== this.generation) throw new Error("Session changed while composing; explicitly review the draft context before submitting.");
    if (captured && !captured.context && this.environmentContext()) throw new Error("Environment definitions were installed while composing; explicitly review the draft context.");
  }
  async submitComposed(cell: string, text: string, scope = "default", retainDraft = false): Promise<void> {
    this.checkComposition(scope);
    const captured = this.compositions.get(scope);
    if (!retainDraft) this.compositions.delete(scope);
    await this.submit(cell, text, captured?.context ?? this.environmentContext());
  }
  /** A lost reply is retried with the exact original intent and attempt id. */
  async retrySubmission(cell: string): Promise<void> {
    const request = this.sourceAttempts.get(cell);
    if (!request) throw new Error("This client no longer holds the original submission; inspect the work before running it again.");
    await this.send(request);
  }
  private attempts = new Map<string, string>();
  private readonly referable = new Map<string, readonly string[]>();
  /**
   * Listens for what the engine says, whether or not it was asked.
   *
   * Results arriving, nodes going stale because something upstream changed, a long call finishing —
   * none of these answer a question, so the client is told rather than polling.
   */
  listen(onEvent: (event: Event) => void, onTrouble: (reason: string) => void, onConnection?: (state: Connection) => void): () => void {
    onConnection?.("connecting");
    const source = new WorkspaceEvents(this.binding === undefined ? "/events" : `/events?${new URLSearchParams({ workspace: this.binding })}`);
    source.onmessage = (message) => {
      try {
        const event = decodeEvent(parseExactJson(message.data));
        const previousGeneration = this.generation;
        if (event.event === "session") {
          for (const waiter of this.documentWaiters.values()) if (waiter.generation !== event.generation) waiter.reject(new Error("Workspace changed while submitting the document; inspect its original workspace before retrying."));
        }
        if (event.event === "planned") {
          const waiter = this.documentWaiters.get(event.cell);
          if (waiter) {
            const problems = event.diagnostics?.filter(d => d.severity === "error");
            if (event.failure || problems?.length) waiter.reject(new Error(event.failure || problems!.map(d => `${d.code}: ${d.message}`).join("\n")));
            else waiter.resolve(event);
          }
        }
        if (event.event === "session") {
          diagnosticsSession(event.generation);
          if (this.uiGeneration !== event.generation) { this.assistantUi.reset(); this.uiGeneration = event.generation; }
          this.generation = event.generation; this.environments = undefined; this.vocabulary = undefined;
          // The greeting carries its identity atomically; a null or invalid name leaves it unknown.
          this.observeWorkspace(workspaceName(event.workspace) ? event.workspace : undefined);
          onConnection?.("connected");
        }
        if (event.event === "workspace-context" && this.generation !== undefined && workspaceName(event.name)) this.observeWorkspace(event.name);
        if (event.event === "workspace-closed") {
          this.generation=undefined;this.environments=undefined;this.vocabulary=undefined;this.observeWorkspace(undefined);
          if(event.workspace && typeof window !== "undefined") window.dispatchEvent?.(new CustomEvent("wes-workspace-closed",{detail:event.workspace}));
          if(this.binding!==undefined)source.close();
        }
        // Bounded like the engine's own node identities: each live node's latest announced names. An
        // older binding this misses is refused by the engine (NAM003), never silently moved.
        if (event.event === "session" || event.event === "workspace-closed") this.referable.clear();
        if (event.event === "created") this.referable.set(event.node, [event.node, event.name, ...(event.errorNames ?? [])].filter(Boolean));
        if (event.event === "dropped") for (const node of event.nodes) this.referable.delete(node);
        if (event.event === "environments") this.environments = event;
        if (event.event === "vocabulary") this.vocabulary = event;
        if (["session","planned","ready","evidence","workspace-closed"].includes(event.event)) this.valuePackages.invalidate();
        const logContext = { workspace: this.viewWorkspaceName() ?? "Current workspace", generation: this.generation };
        if (event.event === "failed") recordExecutionFailure(event, logContext);
        // An incomplete analysis is a failed run with a partial value; its failure is logged the same way.
        if (event.event === "evidence" && event.kind === "incomplete" && event.error)
          recordExecutionFailure({ event: "failed", node: event.node, reason: event.reason ?? event.error.message, error: event.error }, logContext);
        if (["session","ready","evidence","failed","cancelled","dropped","work-retired","workspace-closed"].includes(event.event)) this.viewFrames.invalidate();
        // Access first: every cache is cleared before any consumer hears of the event or the stale
        // state that follows it, so nothing can draw or resurrect the withdrawn value in between.
        if (event.event === "session" && event.generation !== previousGeneration) this.nodeHandles.clear();
        if (event.event === "session") datasetWithdrawals.withdrawMany(this.results.withdrawnHandles(), event.generation);
        if (event.event === "ready" || event.event === "evidence") this.rememberHandle(event.node, event.handle);
        if (event.event === "dropped") for (const node of event.nodes) this.nodeHandles.delete(node);
        if (event.event === "result-access") this.withdrawAccess(event.node);
        if (event.event !== "vocabulary") onEvent(event);
        if ((event.event === "environments" || event.event === "vocabulary") && this.vocabulary) {
          const context = this.environmentContext();
          onEvent({ ...this.vocabulary, providers: context ? (context.selected ? this.environments?.providers[context.selected] ?? [] : []) : this.vocabulary.providers });
        }
      } catch {
        onTrouble("the engine said something this client could not read");
      }
    };
    source.onerror = () => {
      for (const waiter of this.documentWaiters.values()) waiter.reject(new Error("Connection lost while submitting the document; inspect the submitted cell before retrying."));
      clientDiagnostic("reconnect"); this.generation = undefined; this.observeWorkspace(undefined); onConnection?.("reconnecting"); onTrouble("lost the engine");
    };
    return () => source.close();
  }

  /**
   * Runs a command, naming the attempt so a retry is not a second command.
   *
   * The name identifies this attempt, not the cell. A cell re-run on purpose sends a new one and runs
   * again; a cell re-sent after a timeout sends the same one and is answered rather than run twice.
   */
  async submit(cell: string, text: string, context = this.environmentContext()): Promise<void> {
    let request = this.sourceAttempts.get(cell);
    if (request && request.text !== text) throw new Error("this attempt already names different source");
    if (!request) {
      if (this.sourceAttempts.size >= 10_000) throw new Error("submission history is full; reload the client");
      request = { request: "submit", cell, text, client: this.client, environments: context };
      this.sourceAttempts.set(cell, request);
    }
    await this.send(request);
  }

  /** Raw document bytes use the same identity/admission path as normal source work. */
  async submitDocument(cell: string, text: string, source: string, generation: string,
    context?: import("./protocol").EnvironmentContext): Promise<Extract<Event, { event: "planned" }>> {
    if (generation !== this.generation) throw new Error("Workspace changed before document submission.");
    let request = this.sourceAttempts.get(cell);
    if (request && (request.text !== text || request.document?.source !== source)) throw new Error("this attempt already names different document input");
    if (!request) {
      if (this.sourceAttempts.size >= 10_000) throw new Error("submission history is full; reload the client");
      request = { request: "submit", cell, text, document: { source }, client: this.client, environments: context };
      this.sourceAttempts.set(cell, request);
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    const completed = new Promise<Extract<Event, { event: "planned" }>>((resolve, reject) => {
      this.documentWaiters.set(cell, { generation, resolve, reject });
      timer = setTimeout(() => reject(new Error("Document outcome is not confirmed; inspect the submitted cell before retrying.")), Math.max(this.timeout, 300_000));
    });
    try {
      const [, result] = await Promise.all([this.send(request), completed]);
      if (generation !== this.generation) throw new Error("Workspace changed while submitting the document.");
      return result;
    } finally {
      clearTimeout(timer);
      this.documentWaiters.delete(cell);
    }
  }

  /** Explicit repeat of captured work; retries retain the first intent and context exactly. */
  async rerun(cell: string, text: string, origin: string, acknowledgeEffects = false, from?: string, document?: { readonly source: string }): Promise<void> {
    let request = this.sourceAttempts.get(cell);
    if (request && (request.text !== text || request.repeat !== origin || request.acknowledge_effects !== acknowledgeEffects || request.from !== from || request.document?.source !== document?.source)) {
      throw new Error("this attempt already names different repeat intent");
    }
    if (!request) {
      if (this.sourceAttempts.size >= 10_000) throw new Error("submission history is full; reload the client");
      request = { request: "submit", cell, text, document, client: this.client, environments: this.environmentContext(), repeat: origin, acknowledge_effects: acknowledgeEffects, from };
      this.sourceAttempts.set(cell, request);
    }
    await this.send(request);
  }

  async revise(cell: string, text: string, origin: string): Promise<void> {
    let request = this.sourceAttempts.get(cell);
    if (request && (request.text !== text || request.revision_of !== origin)) throw new Error("this attempt already names different revision intent");
    if (!request) {
      if (this.sourceAttempts.size >= 10_000) throw new Error("submission history is full; reload the client");
      request = { request: "submit", cell, text, client: this.client, environments: this.environmentContext(), revision_of: origin };
      this.sourceAttempts.set(cell, request);
    }
    await this.send(request);
  }

  async cancelWork(origin: string): Promise<void> { await this.send({ request: "cancel-work", origin }); }

  /** Stops a node's work. */
  async cancel(node: string): Promise<void> {
    await this.send({ request: "cancel", node });
  }

  /**
   * Hands the engine a credential, by the name a description gave it.
   *
   * Not `submit`, deliberately. A command is echoed to every client and kept on the node that ran it,
   * so a credential typed as one would end up in three places it has no business being. This goes
   * nowhere else, and nothing comes back but the vocabulary saying the provider is now ready.
   */
  async supply(name: string, value: string): Promise<void> {
    const context = this.environmentContext();
    if (context && (!this.environments?.default || context.selected !== this.environments.default)) throw new Error("Managed credentials require scoped environment controls, not the default provider credential store.");
    await this.send({ request: "secret", name, value });
  }
  async environmentAuthority(request: Exclude<Request, { request: "submit" }>): Promise<void> { await this.send(request); }

  /**
   * Says this result should still be here after the engine restarts.
   *
   * Every result is stored so it can be looked at; only the ones somebody keeps are archived. For a
   * large result this is the only chance — nothing else will offer it again.
   */
  async keep(handle: string): Promise<void> {
    await this.send({ request: "keep", handle });
  }

  /** Sets what this session keeps without being asked, and up to what size. */
  /**
   * Asks what could go inside a value written in a language of its own.
   *
   * <p>The only question here that is not a command and not a stored result. It goes to its own path
   * rather than through `/submit` for the reason the data plane exists: requests are serialised on
   * purpose, and a suggestion queued behind a running command is a suggestion nobody waits for.
   *
   * @param language what the parameter said the value is written in
   * @param written the value so far
   * @param caret where the caret is within it
   */
  async suggest(language: string, written: string, caret: number): Promise<Suggested> {
    const asked = new URLSearchParams({
      in: language,
      line: written,
      caret: String(caret),
    });
    const response = await window.fetch(`/complete?${asked.toString()}`);
    if (!response.ok) {
      throw new Error(`the engine could not answer: ${response.status}`);
    }
    return (await readExactJson(response)) as Suggested;
  }

  /**
   * Answers a command that is waiting for one.
   *
   * <p>Goes nowhere else. The engine does not journal it, does not echo it back, and no other client
   * sees it — a prompt asking for a password is an ordinary use of this rather than an edge case.
   */
  async answer(node: string, run: string, text: string): Promise<void> {
    if (new TextEncoder().encode(text).byteLength > 64 * 1024) throw new Error("Answer is too long (maximum 64 KiB).");
    await this.send({ request: "input", node, run, text });
  }
  async eof(node: string, run: string): Promise<void> { await this.send({ request: "eof", node, run }); }

  /** Asks where the engine keeps things. The answer arrives on the event stream, like everything else. */
  async storage(): Promise<void> {
    await this.send({ request: "storage" });
  }

  async keeping(automatic: boolean, under: number): Promise<void> {
    await this.send({ request: "keeping", automatic, under });
  }

  async workHistory(cell: string): Promise<import("./protocol").WorkHistory> {
    return this.storageManagement({ request: "work-history", cell, client: this.client });
  }
  async workRun(cell: string, run: string): Promise<import("./protocol").WorkRunDetail> {
    const key = this.protectionKey(run);
    const detail = await this.storageManagement<import("./protocol").WorkRunDetail>({ request: "work-history", cell, run, client: this.client });
    // Absence of a journal receipt does not establish that retaining bytes failed.
    if (detail.run.protected && this.protection.get(key) !== "pending") this.setProtection(key, undefined);
    return detail;
  }
  private protection = new Map<string, "pending" | "uncertain">();
  private protectionListeners = new Set<() => void>();
  private protectionKey(run: string) { return stringifyExactJson([this.generation, run]); }
  runProtection(run: string) { return this.protection.get(this.protectionKey(run)); }
  onRunProtection(listener: () => void) {
    this.protectionListeners.add(listener);
    return () => { this.protectionListeners.delete(listener); };
  }
  private setProtection(key: string, state: "pending" | "uncertain" | undefined) {
    if (state) this.protection.set(key, state); else this.protection.delete(key);
    this.protectionListeners.forEach(listener => listener());
  }
  async protectRun(cell: string, run: string): Promise<import("./protocol").WorkRunDetail> {
    const key = this.protectionKey(run);
    if (this.protection.has(key)) throw new StorageError("Protection is pending or unconfirmed. Check this run's protection before requesting it again.", [], undefined, true);
    this.setProtection(key, "pending");
    try {
      const detail = await this.storageManagement<import("./protocol").WorkRunDetail>({ request: "protect-run", cell, run, client: this.client });
      if (!detail.run.protected) throw new StorageError("Protection could not be confirmed.", [], undefined, true);
      this.setProtection(key, undefined);
      return detail;
    } catch (error) {
      this.setProtection(key, error instanceof StorageError && error.mayHaveApplied === false ? undefined : "uncertain");
      throw error;
    }
  }
  async previewRelease(handle: string): Promise<import("./protocol").ReleasePreview> {
    return this.storageManagement({ request: "release-preview", handle, client: this.client });
  }
  async previewDeleteWork(cell: string): Promise<import("./protocol").WorkPreview> {
    return this.storageManagement({ request: "delete-work-preview", cell, client: this.client });
  }
  async deleteWork(token: string, dependents: boolean, protectedContent: boolean): Promise<void> {
    await this.storageManagement({ request: "delete-work", token, client: this.client, dependents, protected: protectedContent });
  }
  /** Consumes backend-issued authority, never a bare handle or automatic retry. */
  async release(token: string): Promise<void> {
    await this.storageManagement({ request: "release", token, client: this.client });
  }
  private async storageManagement<T>(request: Request): Promise<T> {
    const generation = this.generation;
    try { return await this.storageRequest<T>(request); }
    catch (error) {
      recordEngineFailure(request, error, { workspace: this.binding ?? "Current workspace", generation });
      throw error;
    }
  }
  private async storageRequest<T>(request: Request): Promise<T> {
    const generation = this.generation;
    if (!generation) throw new Error("Wait for the workspace connection.");
    let response: Response;
    let text: string;
    try {
      response = await fetch("/submit", {
        method: "POST", headers: this.headers({ "Content-Type": "application/json", "X-Wes-Session": generation }),
        body: stringifyExactJson(request), signal: AbortSignal.timeout(this.timeout),
      });
      text = await response.text();
    } catch {
      if (request.request === "protect-run") throw new Error("Run protection completion is unknown. Its result may have been retained; inspect this run before retrying. No execution was started.");
      if (request.request === "work-history") throw new Error("Run history could not be read. No execution was requested.");
      throw new Error(request.request === "release" || request.request === "delete-work"
        ? "Deletion completion is unknown because the connection ended. Stored copies may have changed; inspect before requesting a fresh preview. No retry was made."
        : "Deletion preview could not be read. No deletion was requested.");
    }
    // Retirement updates presentation through authoritative events; a concurrent real
    // session transition still cannot erase the confirmed deletion receipt.
    if (generation !== this.generation && request.request !== "delete-work") throw new Error(request.request === "protect-run"
      ? "Workspace changed; the reply was discarded. The admitted run protection may still have completed."
      : request.request === "work-history" ? "Workspace changed; the historical evidence reply was discarded."
      : "Workspace changed; storage reply was discarded. An admitted deletion may still have completed.");
    if (!response.ok) {
      throw storageError(text);
    }
    return parseExactJson(text) as T;
  }

  /**
   * Reads a result — the data plane, and the only call here that moves anything.
   *
   * Deliberately separate from hearing that a result exists. A node announces itself as "Bars, 4.1 MB,
   * adjusted" for a couple of hundred bytes; this is what it costs to actually look.
   */
  async fetch(handle: string): Promise<StoredValue> {
    const generation=this.generation;
    const value=await this.results.read(handle);
    let modules=builtinValueViews;
    try { modules=await this.valuePackages.modules(value); } catch { /* Renderer failure leaves the stored data readable. */ }
    if(generation!==this.generation)throw new Error("Workspace changed; the result read was discarded.");
    if(this.results.isWithdrawn(handle))throw new ResultWithdrawnError();
    bindValueViews(value,modules);
    return value;
  }
  /**
   * Reads one bounded page (or, without a position, only the lifecycle) of the Dataset at `select`
   * inside the stored result `handle`, for the exact session `generation`. A busy refusal the
   * server marks retryable is retried as the same read; nothing else is. A reply that arrives after
   * the session changed is dropped, never returned. `stream` selects the result's own snapshot's
   * outputs or its skipped records; the reply must be of that stream, under the same authority.
   */
  async readDataset(handle: string, generation: string, expected: DatasetReference, select: string, position: DatasetPosition | undefined, limit: number, signal: AbortSignal, stream: DatasetStream = "outputs"): Promise<DatasetRead> {
    return this.datasetGet(`/datasets/${encodeURIComponent(handle)}`, {}, generation, datasetQuery(select, position, position ? limit : undefined, undefined, stream),
      raw => decodeDatasetRead(raw, expected, position ? limit : undefined, stream), signal);
  }
  /**
   * Inspects the current committed head of the Dataset at `select` inside the stored result
   * `handle`: the same EventLog epoch or analysis attempt as the result's own snapshot `original`,
   * never another attempt. The reply must extend both `original` and `shown`, the newest head this
   * reader already drew. It starts no work; a reader asks for it only after an explicit Follow.
   */
  async readDatasetHead(handle: string, generation: string, original: DatasetReference, shown: DatasetReference, select: string, signal: AbortSignal): Promise<DatasetRead> {
    return this.datasetGet(`/datasets/${encodeURIComponent(handle)}`, {}, generation, datasetQuery(select, undefined, undefined, { kind: "head" }),
      raw => decodeDatasetHead(raw, original, shown), signal);
  }
  /**
   * Reads one page of `extent`, a head this reader already inspected for the stored result
   * `handle`. The server checks it is still a committed extension of the result's own snapshot under
   * current authority; the reply must name exactly that extent.
   */
  async readDatasetExtent(handle: string, generation: string, original: DatasetReference, extent: DatasetReference, select: string, position: DatasetPosition, limit: number, signal: AbortSignal): Promise<DatasetRead> {
    if (!extendsReference(extent, original)) throw new DatasetReadError("invalid", 0, "DATASET_EXTENT_INVALID", "The shown snapshot is not an extension of this result; nothing was read.", false);
    return this.datasetGet(`/datasets/${encodeURIComponent(handle)}`, {}, generation, datasetQuery(select, position, limit, { kind: "extent", reference: extent }),
      raw => decodeDatasetRead(raw, extent, limit), signal);
  }
  /**
   * Reads the Dataset at `select` inside one drawn View member's input, for exactly the frame the
   * host drew: its root and instance, the member, and that member's render and input revisions.
   * The server resolves the member's own input; nothing here names a dataset, path or URL of the
   * View's choosing, and nothing observes, refreshes or runs a source.
   */
  async readViewDataset(binding: ViewDatasetBinding, expected: DatasetReference, select: string, position: DatasetPosition | undefined, limit: number, signal: AbortSignal): Promise<DatasetRead> {
    const path = `/view-datasets/${[binding.root, binding.rootInstance, binding.member].map(encodeURIComponent).join("/")}`;
    // View reads are always the frozen input's outputs: the View route refuses head and extent reads.
    return this.datasetGet(path, { "X-Wes-View-Revision": binding.revision, "X-Wes-Input-Revision": binding.inputRevision },
      binding.generation, datasetQuery(select, position, position ? limit : undefined), raw => decodeDatasetRead(raw, expected, position ? limit : undefined), signal);
  }
  /** One bounded dataset GET: exact session, at most 1 MiB of reply, retry only a retryable busy. */
  private async datasetGet(path: string, extra: Record<string, string>, generation: string, query: string, decode: (raw: unknown) => DatasetRead | undefined, signal: AbortSignal): Promise<DatasetRead> {
    const changed = () => new DatasetReadError("session", 409, "DATASET_SESSION_CHANGED", "Workspace changed; the dataset read was discarded.", false);
    const delays = [150, 400, 900];
    for (let attempt = 0; ; attempt++) {
      if (generation !== this.generation) throw changed();
      const response = await fetch(`${path}?${query}`, {
        headers: this.headers({ ...extra, "X-Wes-Session": generation }), cache: "no-store", signal: AbortSignal.any([signal, AbortSignal.timeout(this.timeout)]),
      });
      if (generation !== this.generation) throw changed();
      if (response.ok) {
        const text = await response.text();
        if (new TextEncoder().encode(text).length > DATASET_REPLY_BYTES) throw new DatasetReadError("limit", response.status, "DATASET_REPLY_LIMIT", "The dataset reply exceeds its 1 MiB budget; nothing from it is drawn.", false);
        const decoded = decode(parseExactJson(text));
        if (generation !== this.generation) throw changed();
        if (!decoded) throw new DatasetReadError("invalid", response.status, "DATASET_REPLY_INVALID", "The dataset reply does not match the shown snapshot; nothing from it is drawn.", false);
        return decoded;
      }
      const failure = await datasetFailure(response);
      if (generation !== this.generation) throw changed();
      if (failure.kind !== "busy" || !failure.retryable || attempt >= delays.length) throw failure;
      await new Promise((resolve, reject) => {
        const timer = setTimeout(resolve, delays[attempt]);
        signal.addEventListener("abort", () => { clearTimeout(timer); reject(signal.reason); }, { once: true });
      });
    }
  }
  /**
   * The stored handles this client was told each node published, newest last and a few per node:
   * the only association between a node-scoped access notice and the values already read for it.
   */
  private readonly nodeHandles = new Map<string, string[]>();
  private rememberHandle(node: string, handle: string) {
    const known = (this.nodeHandles.get(node) ?? []).filter(it => it !== handle);
    this.nodeHandles.set(node, [...known, handle].slice(-HANDLES_PER_NODE));
  }
  /**
   * Withdraws `node`'s result here: its known handles are refused from now on (a reply in flight is
   * dropped), marked withdrawn for every surface drawing a stored identity, its held displays are
   * dropped and every open view frame is read afresh. Nothing reruns and nothing is read again for
   * the node itself; a later run's new handle is unaffected.
   */
  private withdrawAccess(node: string) {
    const handles = this.nodeHandles.get(node) ?? [];
    for (const handle of handles) this.results.withdraw(handle);
    if (this.generation) datasetWithdrawals.withdrawMany(handles, this.generation);
    this.nodeHandles.delete(node);
    liveReader(this).withdraw(node);
    this.valuePackages.invalidate();
    this.viewFrames.purge(); this.viewInputs.purge(); this.viewStates.purge();
  }
  private readonly valuePackages=new ValuePackages(async()=>{
    const generation=this.generation;
    if(!generation)throw new Error("Wait for the workspace connection.");
    const response=await fetch("/language/views",{headers:this.headers({"X-Wes-Session":generation}),cache:"no-store",signal:AbortSignal.timeout(this.timeout)});
    if(!response.ok)throw new Error("View catalogue unavailable.");
    const text=await response.text();
    if(text.length>8*1024*1024)throw new Error("View catalogue budget exceeded.");
    if(generation!==this.generation)throw new Error("Workspace changed; catalogue discarded.");
    return parseExactJson(text);
  },digest=>this.readViewAsset(digest));
  private readonly assetReads=new Map<string,Promise<ViewAsset>>();
  private readViewAsset(digest:string):Promise<ViewAsset>{
    const generation=this.generation,key=`${generation}/${digest}`;
    const pending=this.assetReads.get(key);if(pending)return pending;
    // An asset read is shared immutable work, independent of any single display's lifetime.
    const reading=(async()=>{
      if(!generation)throw new Error("Wait for the workspace connection.");
      const response=await fetch(`/view-packages/${encodeURIComponent(digest)}`,{headers:this.headers({"X-Wes-Session":generation}),cache:"no-store",signal:AbortSignal.timeout(this.timeout)});
      if(!response.ok)throw new Error(`View assets unavailable (HTTP ${response.status}).`);
      const text=await response.text();if(text.length>8*1024*1024)throw new Error("View asset budget exceeded.");
      if(generation!==this.generation)throw new Error("Workspace changed; assets discarded.");
      return parseExactJson(text) as ViewAsset;
    })().finally(()=>this.assetReads.delete(key));
    this.assetReads.set(key,reading);return reading;
  }
  private readonly viewFrames = new ViewFrameReader(async (node, generation, signal, etag, previous) => {
    if(generation!==this.generation)throw new Error("Workspace changed; reopen the view.");
    const [id,instance,mount]=node.split("/");
    const response=await fetch(`/view-instances/${encodeURIComponent(id!)}/${encodeURIComponent(instance!)}`, {
      headers:this.headers({"X-Wes-Session":generation,"X-Wes-View-Mount":mount!,...(previous?{"X-Wes-View-Revisions":stringifyExactJson(previous.instances.map(i=>[i.id,i.instance,i.revision,i.inputRevision]))}:{}),...(etag?{"If-None-Match":etag}:{})}),signal,cache:"no-store",
    });
    if(generation!==this.generation)throw new Error("Workspace changed; view discarded.");
    if(response.status===304)return undefined;
    if(response.status===503)return {retry:true as const};
    if(!response.ok)throw new Error(await response.text()||`View unavailable (HTTP ${response.status}).`);
    const raw=await readExactJson(response) as ViewFrame;
    const frame=mergeViewFrame(raw,previous);
    await loadFramePackages(frame,digest=>this.readViewAsset(digest));
    if(generation!==this.generation)throw new Error("Workspace changed; view discarded.");
    return {frame,etag:response.headers.get("ETag")??""};
  },undefined,frame=>frame.instances.some(i=>i.query?.running || i.observing && (i.inputReference.kind==="current" || !!i.query)));
  private readonly viewInputs = new ViewFrameReader<InputPatches>(async(node,generation,signal,etag)=>{
    if(generation!==this.generation)throw new Error("Workspace changed; reopen the view.");
    const response=await fetch(`/view-inputs/${node.split("/").map(encodeURIComponent).join("/")}`,{
      headers:this.headers({"X-Wes-Session":generation,...(etag?{"If-None-Match":etag}:{})}),signal,cache:"no-store",
    });
    if(generation!==this.generation)throw new Error("Workspace changed; input discarded.");
    if(response.status===304)return undefined;
    if(response.status===503)return {retry:true as const};
    if(!response.ok)throw new Error("View input bindings are unavailable.");
    return {frame:await readExactJson(response) as InputPatches,etag:response.headers.get("ETag")??""};
  },250);
  watchViewInputs(node:string,instance:string,generation:string,changed:(sample:FrameSample<InputPatches>)=>void):()=>void {
    return this.viewInputs.watch(`${node}/${instance}`,generation,changed);
  }
  // State subscriptions cover the bounded members of admitted roots, rather than counting each as a root.
  private readonly viewStates = new ViewFrameReader<SharedState>(async (node,generation,signal,etag)=>{
    if(generation!==this.generation)throw new Error("Workspace changed; reopen the view.");
    const response=await fetch(`/view-interaction/${node.split("/").map(encodeURIComponent).join("/")}`,{
      headers:this.headers({"X-Wes-Session":generation,...(etag?{"If-None-Match":etag}:{})}),signal,cache:"no-store",
    });
    if(generation!==this.generation)throw new Error("Workspace changed; selection discarded.");
    if(response.status===304)return undefined;
    if(response.status===503)return {retry:true as const};
    if(!response.ok)throw new Error("Shared selection is unavailable.");
    return {frame:await readExactJson(response) as SharedState,etag:response.headers.get("ETag")??""};
  },250,undefined,max_active_view_roots()*max_view_frame_instances());
  watchViewState(node:string,instance:string,generation:string,changed:(sample:FrameSample<SharedState>)=>void):()=>void {
    return this.viewStates.watch(`${node}/${instance}`,generation,changed);
  }
  async commitViewState(node:string,instance:string,generation:string,edit:SharedCommit,signal:AbortSignal):Promise<{state:SharedState;conflict:boolean;problem?:string}> {
    if(generation!==this.generation)throw new Error("Workspace changed");
    const {owner,identity,definitionRevision,revision,fields,outputs,events=[]}=edit;
    const deadline=AbortSignal.any([signal,AbortSignal.timeout(2000)]);
    const request={method:"PUT",headers:this.headers({"X-Wes-Session":generation,"Content-Type":"application/json"}),
      body:stringifyExactJson({owner,identity,definitionRevision,revision,fields,outputs,events}),signal:deadline};
    let response:Response;
    for(let attempt=0;;attempt++) {
      deadline.throwIfAborted();
      response=await fetch(`/view-interaction/${encodeURIComponent(node)}/${encodeURIComponent(instance)}`,request);
      // This route's 503 is admission refusal before decoding or mutation. Unknown outcomes,
      // conflicts and all other failures are never replayed.
      if(response.status!==503 || attempt===3)break;
      await response.body?.cancel();
      await new Promise<void>((resolve,reject)=>{
        const finish=()=>{deadline.removeEventListener("abort",cancel);resolve();};
        const timer=setTimeout(finish,50);
        const cancel=()=>{clearTimeout(timer);deadline.removeEventListener("abort",cancel);reject(deadline.reason);};
        deadline.addEventListener("abort",cancel,{once:true});if(deadline.aborted)cancel();
      });
      if(generation!==this.generation)throw new Error("Workspace changed");
    }
    if(generation!==this.generation||signal.aborted)throw new Error("Workspace changed");
    if(!response.ok&&response.status!==409)throw new Error("Selection was not confirmed");
    const state=await readExactJson(response) as SharedState & {problem?:string};
    return {state,conflict:response.status===409,problem:state.problem};
  }
  private async viewMountAction(node:string,identity:string,generation:string,action:string,token?:string):Promise<string|undefined>{
    if(generation!==this.generation)throw new Error("Workspace changed; reopen the view.");
    const deadline=AbortSignal.timeout(2000);
    try {
      const response=await fetch(`/view-mounts/${encodeURIComponent(node)}/${encodeURIComponent(identity)}`,{
        method:"POST",headers:this.headers({"X-Wes-Session":generation,"Content-Type":"application/json"}),
        body:stringifyExactJson({action,...(token?{token}:{})}),signal:deadline,
      });
      if(generation!==this.generation)throw new Error("Workspace changed; reopen the view.");
      if(!response.ok)throw new Error(await response.text()||"View display session is unavailable; reopen the view and press Start.");
      const value=await readExactJson(response) as {token?:string};
      if(action!=="close" && action!=="touch")this.viewFrames.invalidate();
      return value.token;
    } catch(error) {
      if(generation!==this.generation)throw new Error("Workspace changed; reopen the view.");
      if(deadline.aborted)throw new Error("View session request exceeded its 2-second budget. Its outcome is unconfirmed; reopen the view to reconnect. No command was rerun.");
      if(error instanceof DOMException && error.name==="AbortError")throw new Error("View session connection was interrupted. Its outcome is unconfirmed; reopen the view to reconnect. No command was rerun.");
      throw error;
    }
  }
  private readonly viewMounts=new ViewMounts((node,identity,generation,action,token)=>this.viewMountAction(node,identity,generation,action,token));
  async viewObservation(node:string,identity:string,generation:string,active:boolean):Promise<void>{
    await this.viewMountAction(node,identity,generation,active?"start":"stop");
  }
  async applyViewQuery(node:string,generation:string):Promise<void>{
    if(generation!==this.generation || !/^[\p{L}\p{N}_]+$/u.test(node))throw new Error("View reference changed; reopen it before applying the query.");
    await this.submit(crypto.randomUUID(), `:view apply $${node}`);
    this.viewFrames.invalidate();
  }
  async captureViewResult(node:string,generation:string,port?:string):Promise<void>{
    if(generation!==this.generation || !/^[\p{L}\p{N}_]+$/u.test(node))throw new Error("View reference changed; reopen it before capturing a result.");
    await this.submit(crypto.randomUUID(),port===undefined?`:view capture $${node}`:`:view output $${node} port:${stringifyExactJson(port)}`);
  }
  /**
   * One Pin of exactly the displayed frame. The result is a new cell whose publication reports
   * retention and binding separately; a lost reply is never resubmitted here.
   */
  async pinViewInput(entry:Pick<ViewInstance,"id"|"instance"|"revision"|"inputRevision">,generation:string):Promise<string>{
    if(generation!==this.generation)throw new Error("View reference changed; reopen it before pinning its input.");
    const name=freshPinName(entry.id,new Set([...this.referable.values()].flat()));
    await this.submit(crypto.randomUUID(),pinCommand(entry,name,stringifyExactJson));
    // An admitted Pin stays admitted in its workspace; only this feedback would be stale here.
    if(generation!==this.generation)throw new Error("Workspace changed while the Pin was submitted; its outcome is reported in the original workspace. It was not repeated.");
    return name;
  }
  viewGeneration():string|undefined { return this.generation; }
  watchViewFrame(node:string, instance:string, changed:(sample:FrameSample)=>void):()=>void {
    if(!this.generation){changed({problem:"Wait for the workspace connection before opening this view."});return ()=>{};}
    const generation=this.generation;
    let closed=false,failed=false,stop:(()=>void)|undefined;
    const mount=this.viewMounts.mount(node,instance,generation,error=>{
      if(closed)return;
      failed=true;stop?.();stop=undefined;changed({problem:error.message});
    });
    if(mount.waiting)changed({paused:true});
    void mount.ready.then(token=>{if(!closed&&!failed)stop=this.viewFrames.watch(`${node}/${instance}/${token}`,generation,changed);}).catch(error=>{if(!closed)changed({problem:error instanceof Error?error.message:"View unavailable"});});
    return ()=>{closed=true;stop?.();mount.close();};
  }
  async liveView(node: string, generation: string, signal: AbortSignal): Promise<DisplayRead | undefined> {
    if (generation !== this.generation) throw new DisplayReadError("Workspace changed; discard its display samples.", "withdrawn");
    const response = await fetch(`/live-view/${encodeURIComponent(node)}`, {
      headers: this.headers({ "X-Wes-Session": generation }), signal, cache: "no-store",
    });
    if (generation !== this.generation) throw new DisplayReadError("Workspace changed; sample discarded.", "withdrawn");
    const encoded=response.headers.get("X-Wes-Display");
    let metadata:DisplayMetadata|undefined;
    if(encoded){try{const parsed=JSON.parse(encoded);if(!validDisplayMetadata(parsed))throw new Error();metadata=parsed;}catch{throw new DisplayReadError("Invalid display metadata.","read");}}
    if (!response.ok && response.status!==204) {
      const body=await response.json().catch(()=>({}));
      const code=response.status===403 || response.status===409 || response.status===410 ? "withdrawn" : response.status===413 ? "budget" : response.status===503 ? "busy" : response.status===429 ? "capacity" : "read";
      throw new DisplayReadError(body.message || `Live view unavailable (HTTP ${response.status}).`,code,metadata);
    }
    if(!metadata || typeof metadata.revision!=="string" || !Array.isArray(metadata.epochs) || !Array.isArray(metadata.sources))throw new DisplayReadError("Invalid display metadata.","read");
    const value=response.status===204 ? undefined : withValidMeta(await readExactJson(response) as StoredValue);
    if (generation !== this.generation) throw new DisplayReadError("Workspace changed; sample discarded.","withdrawn");
    return {value,metadata};
  }


  /** A saved Log page is read-only and belongs to the selected session, never applied as live events. */
  async history(cursor?: string, signal?: AbortSignal): Promise<SavedHistoryPage> {
    const generation = this.generation;
    if (generation === undefined) throw new Error("connect to the engine before reading saved history");
    const controller = new AbortController();
    const cancel = () => controller.abort();
    if (signal?.aborted) cancel();
    else signal?.addEventListener("abort", cancel, { once: true });
    const timer = setTimeout(cancel, this.timeout);
    try {
      const query = cursor === undefined ? "" : `?${new URLSearchParams({ cursor })}`;
      const response = await window.fetch(`/history${query}`, { headers: this.headers({ "X-Wes-Session": generation }), signal: controller.signal });
      if (!response.ok) throw new Error(`saved history could not be read (${response.status}): ${await response.text()}`);
      const page = await readExactJson(response) as SavedHistoryPage;
      if (this.generation !== generation || page.generation !== generation) throw new Error("session changed while reading saved history; start again");
      return page;
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener("abort", cancel);
    }
  }

  /**
   * How long to wait before saying the engine did not answer. Source submissions have a five-minute
   * minimum because their HTTP reply waits for preparation (including documentation conversion).
   *
   * A timeout is a decision, not a discovery: a slow engine and a dead one look identical from here.
   * So this gives up waiting and says so, and never decides on its own to ask again — asking again is
   * the person's call, because for some commands it means asking twice.
   */
  timeout = 15_000;

  private async send(request: Request): Promise<void> {
    const selected = this.generation;
    if (selected === undefined) throw new Error("connect to the engine before sending a command");
    let generation = selected;
    if (request.request === "submit") {
      const original = this.attempts.get(request.cell);
      if (original !== undefined) generation = original;
      else {
        if (this.attempts.size >= 10_000) throw new Error("submission history is full; reload the client");
        this.attempts.set(request.cell, selected);
      }
    }
    // Source preparation may download documentation and await describe before the HTTP reply.
    // Keep source semantics in the engine: scripts and compound commands can import too.
    const wait = request.request === "submit" ? Math.max(this.timeout, 300_000) : this.timeout;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), wait);
    const measured = timeClientSubmit();
    let outcome: "ok" | "error" = "error";
    try {
      const response = await window.fetch("/submit", {
        method: "POST",
        headers: this.headers({ "Content-Type": "application/json", "X-Wes-Session": generation }),
        body: stringifyExactJson(request),
        signal: controller.signal,
      });
      if (!response.ok) {
        const notStarted = request.request === "submit" && response.headers?.get(SUBMISSION_OUTCOME_HEADER) === "not-started";
        throw new EngineRefusal(response.status, await response.text(), requestOperation(request), notStarted ? "not-started" : undefined);
      }
      if (request.request === "submit" && response.headers?.get("content-type")?.includes("application/json")) {
        const reply = await readExactJson(response) as { sandbox?: SandboxObservation };

        if (reply.sandbox && generation === this.generation && reply.sandbox.generation === generation) {
          for (const listener of this.sandboxListeners) listener(request.cell, reply.sandbox);
        }
      }
      outcome = "ok";
    } catch (error) {
      recordEngineFailure(request, error, { workspace: this.binding ?? "Current workspace", generation }, controller.signal.aborted);
      throw error;
    } finally {
      measured(outcome);
      clearTimeout(timer);
    }
  }
}
