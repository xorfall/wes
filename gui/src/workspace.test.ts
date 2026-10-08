import { expect, it } from "vitest";
import { apply, emptyWorkspace, observationStale, staleMessage } from "./workspace";

const created = () => apply(emptyWorkspace, {
  event: "created", dependencyLifetime: "continuous", node: "id1", name: "request", command: "source read", dependsOn: [], interactive: false,
});

it("retains structured failure references and clears them on a new run", () => {
  const error = {id:"e",code:"DSC002",message:"Rejected",causeId:"",issues:[{path:"/describeReport",code:"DSC_REPORT",message:"report-id"}]};
  const failed = apply(created(),{event:"failed",node:"id1",reason:error.message,error});
  expect(failed.nodes[0]?.failureRecord).toEqual(error);
  expect(apply(failed,{event:"node",constructionComplete: false,node:"id1",state:"running"}).nodes[0]?.failureRecord).toBeUndefined();
  expect(apply(failed,{event:"cancelled",node:"id1",code:"RUN002",reason:"cancelled"}).nodes[0]?.failureRecord).toBeUndefined();
});

it("forwards publication evidence atomically and clears old authority on refresh and failed publication", () => {
  const publication = { state: "available" as const, run: "r1", handle: "stored", uncertainHandle: null, problem: null, message: "Published" };
  const ready = apply(created(), { event: "ready", node: "id1", publication, type: "Text", handle: "stored", bytes: 1, provenance: {}, cautions: [], kept: true });
  expect(ready.nodes[0]?.publication).toBe(publication);
  const running = apply(ready, { event: "node", constructionComplete: false, node: "id1", state: "running" });
  expect(running.nodes[0]?.publication).toBeUndefined();
  const pending = { ...publication, state: "pending" as const, run: "r2", handle: null };
  const publishing = apply(running, { event: "node", constructionComplete: false, node: "id1", state: "ready", publication: pending });
  expect(publishing.nodes[0]?.publication).toBe(pending);
  expect(publishing.nodes[0]?.handle).toBeUndefined();
  const failed = { ...pending, state: "unavailable" as const, uncertainHandle: "uncertain", problem: { id: "e1", code: "RUN005", message: "Not acknowledged", causeId: "cause", issues: [] } };
  const final = apply(publishing, { event: "node", constructionComplete: false, node: "id1", state: "ready", publication: failed });
  expect(final.nodes[0]?.state).toBe("ready");
  expect(final.nodes[0]?.failure).toBeUndefined();
  expect(final.nodes[0]?.publication).toBe(failed);
  expect(final.nodes[0]?.handle).toBeUndefined();
  expect(apply(final, { event: "session", workspace: null, generation: "g2" }).nodes).toHaveLength(0);
});

it("retains the engine's cancellation reason without treating it as failure", () => {
  const state = apply(created(), { event: "cancelled", node: "id1", code: "RUN002", reason: "Local timeout" });
  expect(state.nodes[0]?.state).toBe("cancelled");
  expect(state.nodes[0]?.cancellation).toEqual({ code: "RUN002", reason: "Local timeout" });
  expect(state.nodes[0]?.failure).toBeUndefined();
});

it("clears the cancellation notice when a new attempt starts", () => {
  const cancelled = apply(created(), { event: "cancelled", node: "id1", code: "RUN003", reason: "Local cancellation" });
  const running = apply(cancelled, { event: "node", constructionComplete: false, node: "id1", state: "running" });
  expect(running.nodes[0]?.cancellation).toBeUndefined();
  expect(running.nodes[0]?.state).toBe("running");
});

it("does not show a retained result as the output of a skipped branch", () => {
  const ready = apply(created(), { event: "ready", node: "id1", type: "Text", handle: "old", bytes: 10, provenance: {}, cautions: [], kept: true });
  const skipped = apply(ready, { event: "node", constructionComplete: false, node: "id1", state: "skipped" });
  expect(skipped.nodes[0]?.state).toBe("skipped");
  expect(skipped.nodes[0]?.handle).toBeUndefined();
  expect(skipped.nodes[0]?.failure).toBeUndefined();
});

it("replaces an existing node description without duplicating it or losing its result", () => {
  const ready = apply(created(), { event: "ready", node: "id1", type: "Text", handle: "saved", bytes: 4, provenance: {}, cautions: [], kept: true });
  const updated = apply(ready, { event: "created", dependencyLifetime: "continuous", node: "id1", name: "renamed", command: "source read", dependsOn: [], interactive: false });
  expect(updated.nodes).toHaveLength(1);
  expect(updated.nodes[0]?.name).toBe("renamed");
  expect(updated.nodes[0]?.handle).toBe("saved");
  expect(apply(updated, { event: "session", workspace: null, generation: "another" })).toEqual(emptyWorkspace);
});

it("updates a log receipt in place when its durable acknowledgement arrives", () => {
  const record = { id: "log1", node: "id1", run: "run1", at: "2026-09-11T00:00:00Z", state: "READY" as const, error: [] as const };
  const pending = apply(created(), { event: "log", record, durable: false, persistenceProblem: "" });
  const durable = apply(pending, { event: "log", record, durable: true, persistenceProblem: "" });
  expect(durable.history).toHaveLength(1);
  expect(durable.history[0]?.durable).toBe(true);
});

it("keeps subcommand metadata from the wire available to completion", async () => {
  const { complete } = await import("./complete");
  const state = apply(emptyWorkspace, {
    event: "vocabulary", annotations: [], providers: [], commands: [{
      name: "import", implemented: true, summary: "", takes: ["custom"], open: true, parameters: [],
      variants: [{ word: "custom", parameters: [{ name: "source", type: "Text", required: true, allowed: [], content: "" }], takes: [], variants: [] }],
    }],
  });
  const line = ":import custom so";
  expect(complete(line, line.length, state.catalogue, []).items.map(item => item.text)).toEqual(["source:"]);
});


it("keeps explicitly stopped results readable without changing graph validity and withdraws them on refresh", () => {
  const stopped = apply(created(), { event: "evidence", kind: "stopped_stream", state: "skipped", source: "source", run: "last-success", node: "id1", type: "Text", handle: "last", bytes: 4, provenance: {}, cautions: [], kept: false });
  expect(stopped.nodes[0]).toMatchObject({ state: "skipped", handle: "last", kept: false, evidence: { kind: "stopped_stream", source: "source", run: "last-success" } });
  expect(stopped.nodes[0]?.failure).toBeUndefined();
  const refreshing = apply(stopped, { event: "node", constructionComplete: false, node: "id1", state: "stale" });
  expect(refreshing.nodes[0]?.handle).toBeUndefined();
  expect(refreshing.nodes[0]?.evidence).toBeUndefined();
  const live = apply(stopped, { event: "ready", node: "id1", type: "Text", handle: "new", bytes: 4, provenance: {}, cautions: [], kept: false });
  expect(live.nodes[0]?.evidence).toBeUndefined();
  expect(live.nodes[0]?.state).toBe("ready");
  expect(apply(stopped, { event: "dropped", nodes: ["id1"] }).nodes).toHaveLength(0);
});

/*
 * Structural inputs shaped by the declared Rust contract (driver/progress.rs, web/projection.rs).
 * They test the reducer's rules only; they are not a captured engine payload.
 */
const scanCreated = (run: string) => apply(emptyWorkspace, {
  event: "created", dependencyLifetime: "continuous", node: "scan1", name: "normalized", command: ":scan source:$raw", dependsOn: [], interactive: false, run,
});
const progressOf = (phase: "processing" | "stopped", committed: string) => ({ kind: "records" as const, phase, counters: {
  committedPosition: committed, readPosition: "40", extent: "100", unit: "bytes" as const, inputRecords: "3", outputRecords: "2",
  work: "900", workAllowance: "1000", workLimit: "5000", heldCharge: "64", highWaterCharge: "96", heldLimit: "4096", outputCharge: "32", outputLimit: "1024",
} });
const scanError = { id: "e-scan", code: "CAL006", message: "scan work limit reached (5000)", causeId: "", issues: [] };

it("keeps an incomplete analysis failed while its committed partial result stays readable as evidence", () => {
  const running = apply(scanCreated("run-a"), { event: "node", constructionComplete: false, node: "scan1", state: "running" });
  const stopped = apply(running, { event: "evidence", kind: "incomplete", state: "failed", source: "scan1", run: "run-a", node: "scan1",
    type: "ScanResult", handle: "partial", bytes: 12, provenance: {}, cautions: [], kept: false, error: scanError, reason: scanError.message });
  expect(stopped.nodes[0]).toMatchObject({ state: "failed", handle: "partial", evidence: { kind: "incomplete", run: "run-a" },
    failure: scanError.message, failureRecord: scanError });
  // A later run withdraws the evidence and its handle; nothing carries it into the new run.
  const again = apply(stopped, { event: "node", constructionComplete: false, node: "scan1", state: "running" });
  expect(again.nodes[0]?.evidence).toBeUndefined();
  expect(again.nodes[0]?.handle).toBeUndefined();
  expect(again.nodes[0]?.failure).toBeUndefined();
});

it("applies progress only for the node's exact current run and forgets it when a new run is announced", () => {
  const first = scanCreated("run-a");
  const reported = apply(first, { event: "node-progress", node: "scan1", run: "run-a", progress: progressOf("processing", "10") });
  expect(reported.nodes[0]?.progress).toEqual({ run: "run-a", value: progressOf("processing", "10") });
  // A late report from another run, or one without a run, changes nothing.
  expect(apply(reported, { event: "node-progress", node: "scan1", run: "run-old", progress: progressOf("stopped", "99") })).toEqual(reported);
  expect(apply(reported, { event: "node-progress", node: "scan1", run: null, progress: progressOf("stopped", "99") })).toEqual(reported);
  expect(apply(reported, { event: "node-progress", node: "unknown", run: "run-a", progress: progressOf("stopped", "99") })).toEqual(reported);
  // The same run's next report replaces the slot; no history accumulates.
  const next = apply(reported, { event: "node-progress", node: "scan1", run: "run-a", progress: progressOf("processing", "20") });
  expect(next.nodes[0]?.progress?.value.counters?.committedPosition).toBe("20");
  const rerun = apply(next, { event: "created", dependencyLifetime: "continuous", node: "scan1", name: "normalized", command: ":scan source:$raw", dependsOn: [], interactive: false, run: "run-b" });
  expect(rerun.nodes[0]?.progress).toBeUndefined();
  expect(rerun.nodes[0]?.run).toBe("run-b");
});

it("retirement forgets only the removed cell-to-node binding", () => {
  const workspace = { ...created(), cells: { deleted: ["id2"], kept: ["id1"] } };
  const next = apply(workspace, { event: "work-retired", cells: ["deleted"] });
  expect(next.cells).toEqual({ kept: ["id1"] });
  expect(next.nodes).toBe(workspace.nodes);
});

it("carries stale evidence and clears both reason and announcement on every non-stale outcome", () => {
  const staleReason = { code: "restore_not_retained", message: "No retained result was available when the workspace reopened. The command was not rerun." };
  const stale = apply(created(), { event: "node", constructionComplete: false, node: "id1", state: "stale", staleReason });
  expect(stale.nodes[0]?.staleReason).toEqual(staleReason);
  expect(stale.wentStale).toEqual(["id1"]);
  for (const state of ["pending", "running", "ready", "skipped"] as const) {
    const next = apply(stale, { event: "node", constructionComplete: false, node: "id1", state });
    expect(next.nodes[0]?.staleReason).toBeUndefined();
    expect(next.wentStale).toEqual([]);
  }
  for (const event of [{ event: "failed", node: "id1", reason: "failed", error: { id: "failure-1", code: "SYN001", message: "failed", causeId: "", issues: [] } }, { event: "cancelled", node: "id1", code: "RUN003", reason: "cancelled" }] as const) {
    const next = apply(stale, event);
    expect(next.nodes[0]?.staleReason).toBeUndefined();
    expect(next.wentStale).toEqual([]);
  }
  expect(apply(stale, { event: "node", constructionComplete: false, node: "id1", state: "stale" }).nodes[0]?.staleReason).toBeUndefined();
});

it("should_ProjectTheEnginesUpdatePendingBit_When_AStateEventCarriesIt", () => {
  // Arrange
  const running = apply(created(), { event: "node", constructionComplete: false, node: "id1", state: "running", updatePending: true });
  const error = { id: "failure-1", code: "SYN001", message: "failed", causeId: "", issues: [] };
  // Act
  const replacements = [
    apply(running, { event: "node", constructionComplete: false, node: "id1", state: "running", updatePending: false }),
    apply(running, { event: "node", constructionComplete: false, node: "id1", state: "running" }),
    apply(running, { event: "node", constructionComplete: false, node: "id1", state: "stale", staleReason: { code: "input_behind", message: "Behind." } }),
    apply(running, { event: "ready", node: "id1", type: "Text", handle: "h", bytes: 1, provenance: {}, cautions: [], kept: false }),
    apply(running, { event: "evidence", kind: "stopped_stream", state: "skipped", source: "s", run: "r", node: "id1", type: "Text", handle: "h", bytes: 1, provenance: {}, cautions: [], kept: false }),
    apply(running, { event: "failed", node: "id1", reason: "failed", error }),
    apply(running, { event: "cancelled", node: "id1", code: "RUN003", reason: "cancelled" }),
  ];
  // Assert
  expect(running.nodes[0]?.updatePending).toBe(true);
  for (const next of replacements) expect(next.nodes[0]?.updatePending).toBeUndefined();
});

it("should_TreatInputBehindAsBoundedObservationStaleness_When_TheEngineReportsIt", () => {
  // Arrange
  const behind = apply(created(), { event: "node", constructionComplete: false, node: "id1", state: "stale",
    staleReason: { code: "input_behind", message: "The calculation completed an older input while newer input arrived." } });
  const changed = apply(created(), { event: "node", constructionComplete: false, node: "id1", state: "stale", staleReason: { code: "dependency_changed", message: "Changed." } });
  // Act
  const [observed, definitional] = [observationStale(behind.nodes[0]!), observationStale(changed.nodes[0]!)];
  // Assert
  expect(observed).toBe(true);
  expect(definitional).toBe(false);
  expect(staleMessage(behind.nodes[0]!)).toBe("The calculation completed an older input while newer input arrived.");
});

it("applies rolling history eviction and receipt changes atomically without replacing unchanged entries", () => {
  const entry = (id: string, durable = false) => ({ event: "log" as const, record: { id, node: "n", run: id, at: "0", state: "READY" as const, error: [] as const }, durable, persistenceProblem: "" });
  const entries = Array.from({ length: 10000 }, (_, n) => entry(String(n)));
  let workspace = apply(emptyWorkspace, { event: "log-delta", reset: true, removed: [], entries });
  workspace = apply(workspace, { event: "log-delta", reset: false, removed: ["0"], entries: [entry("1", true), entry("10000")] });
  expect(workspace.history).toHaveLength(10000);
  expect(workspace.history[0]!.record.id).toBe("1");
  expect(workspace.history[0]!.durable).toBe(true);
  expect(workspace.history[1]).toBe(entries[2]);
  expect(workspace.history.at(-1)!.record.id).toBe("10000");
  expect(apply(workspace, { event: "log-delta", reset: true, removed: [], entries: [] }).history).toEqual([]);
});
it("retains authoritative selected-port explanations and clears them when a run progresses",()=>{
  const waiting=[{source:"source",port:"error" as const,state:"closed" as const,run:"r1",message:"error output was not produced by $source in this run"}];
  const state=apply(created(),{event:"node",constructionComplete: false,node:"id1",state:"pending",waiting});expect(state.nodes[0]?.waiting).toEqual(waiting);
  expect(apply(state,{event:"node",constructionComplete: false,node:"id1",state:"running"}).nodes[0]?.waiting).toBeUndefined();
});

it("should_ProjectCreationLifetimeAndCompletion_When_TheEngineAnnouncesThem", () => {
  // Arrange
  const announced = apply(emptyWorkspace, { event: "created", dependencyLifetime: "creation", node: "chart", name: "chart", command: ":view create Metric", dependsOn: ["mapped"], interactive: false });
  // Act
  const built = apply(announced, { event: "node", constructionComplete: true, node: "chart", state: "ready" });
  const readyWithoutFlag = apply(built, { event: "ready", node: "chart", type: "ViewInstance", handle: "h", bytes: 1, provenance: {}, cautions: [], kept: false });
  const failed = apply(built, { event: "failed", node: "chart", reason: "x", error: { id: "e", code: "VIE001", message: "x", causeId: "", issues: [] } });
  // Assert
  expect(announced.nodes[0]?.dependencyLifetime).toBe("creation");
  expect(announced.nodes[0]?.constructionComplete).toBeUndefined();
  expect(built.nodes[0]?.constructionComplete).toBe(true);
  expect(readyWithoutFlag.nodes[0]?.constructionComplete).toBeUndefined();
  expect(failed.nodes[0]?.constructionComplete).toBeUndefined();
  expect(apply(built, { event: "node", constructionComplete: false, node: "chart", state: "pending" }).nodes[0]?.constructionComplete).toBeUndefined();
});
