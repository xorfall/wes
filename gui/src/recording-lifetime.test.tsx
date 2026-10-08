import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { decodeDatasetRead, type DatasetRead } from "./dataset-read";
import type { Engine } from "./engine";
import { decodeDatasetReference, type DatasetReference } from "./presentation/dataset";
import type { RecordingControl, StoredValue } from "./protocol";
import { decodeProgress } from "./protocol-decode";
import { newCell, type Cell } from "./cells";
import type { WorkspaceNode } from "./workspace";
import { lineText } from "./surface/MonoLine";
import { progressRows, lifetimeActive } from "./surface/record-progress";
import { recordingActions } from "./surface/RecordingControls";
import { joined, verdictOf } from "./surface/session-model";
import { stateOf as graphState } from "./surface/graph-model";
import { ValueBlock } from "./surface/render/ValueBlock";

/*
 * Independently authored synthetic recording lifetimes: invented node ids, runs, sequences and
 * dataset identities. They describe no real source, writer or store.
 */
vi.mock("./surface/Cell", async (original) => ({ ...(await original<typeof import("./surface/Cell")>()), useBlockReport: () => () => undefined }));
afterEach(() => { vi.restoreAllMocks(); });

const RUN = "00000000-0000-4000-8000-0000000000e1";
const LATER = "00000000-0000-4000-8000-0000000000e2";
const ACTIVE: RecordingControl = { active: true, statusAvailable: true, stopAvailable: true, discardAvailable: false };
const DRAINING: RecordingControl = { active: true, statusAvailable: true, stopAvailable: false, discardAvailable: false };
const FINISHED: RecordingControl = { active: false, statusAvailable: true, stopAvailable: false, discardAvailable: false };

const writer = (state: string, over: Record<string, unknown> = {}) => ({
  state, first: "1", acceptedThrough: "24", committedThrough: "20", pending: "4", rejected: "0", termination: null,
  chargedBytes: "2048", chargedWork: "24", bytesLimit: "65536", workLimit: "1000", ...over,
});
const PHASE: Record<string, string> = { prepared: "reading", recording: "processing", draining: "committing", stopped: "complete", incomplete: "stopped" };
const report = (state: string, over: Record<string, unknown> = {}) => ({ kind: "recording", phase: PHASE[state], counters: null, recording: writer(state, over) });

/**
 * A recording whose first prefix is usable: the engine says the node is ready while its run goes on.
 * As the engine announces it, an active writer's run also carries `lifetimeActive: true`.
 */
const recording = (control: RecordingControl | undefined, progress: unknown = report("recording"), over: Partial<WorkspaceNode> = {}): WorkspaceNode => ({
  id: "n1", name: "capture", command: ":synthetic", dependsOn: [], state: "ready", provenance: {}, cautions: [], kept: false, run: RUN,
  progress: { run: RUN, value: decodeProgress(progress) },
  ...(control ? { recordingControl: { ...control, run: RUN } } : {}),
  ...(control?.active ? { openLifetime: { run: RUN } } : {}), ...over,
});
const first = (node: WorkspaceNode) => lineText(progressRows(node)![0]!);
const cell = (ids: readonly string[]): Cell => ({ ...newCell(":synthetic"), state: "answered", nodes: [...ids] });
const NOW = new Date("2026-01-01T00:00:00Z");
const verdict = (...nodes: WorkspaceNode[]) => lineText(joined(verdictOf("default", cell(nodes.map(node => node.id)), nodes, NOW)));
const stateWord = (...nodes: WorkspaceNode[]) => verdict(...nodes).split(" · ")[0];

describe("a ready recording whose writer is still active", () => {
  it("reads its working state as current, offers both controls and does not say the run is ok", () => {
    const node = recording(ACTIVE);
    expect(lifetimeActive(node)).toBe(true);
    expect(first(node)).toBe("recording · committed 1–20 · accepted through 24");
    expect(recordingActions(node)).toEqual({ status: true, stop: true, discard: false, run: RUN });
    expect(stateWord(node)).toBe("recording");
  });

  it("while a stop drains: still active, the drain is current, and status is the only control", () => {
    const node = recording(DRAINING, report("draining"));
    expect(lifetimeActive(node)).toBe(true);
    expect(first(node)).toMatch(/^draining · /);
    expect(first(node)).not.toMatch(/last reported/);
    expect(recordingActions(node)).toEqual({ status: true, stop: false, discard: false, run: RUN });
    expect(stateWord(node)).toBe("recording");
  });

  it("never lets a terminal report speak for a writer the engine still calls active", () => {
    const stopped = report("stopped", { acceptedThrough: "20", pending: "0", termination: "manual" });
    expect(first(recording(ACTIVE, stopped))).toMatch(/^last reported stopped · /);
    expect(first(recording(DRAINING, stopped))).toMatch(/^last reported stopped · /);
    // Once the engine says the writer finished, the same report is the current end.
    expect(first(recording(FINISHED, stopped))).toMatch(/^stopped · /);
    expect(stateWord(recording(FINISHED, stopped))).toBe("ok");
  });

  it("keeps the usable value: the node stays ready and its glyph and type are not withheld", () => {
    const node = recording(ACTIVE, report("recording"), { type: "Recording" });
    expect(node.state).toBe("ready");
    expect(verdict(node)).toContain("Recording");
  });
});

describe("a ready recording without an active writer signal", () => {
  it("reads a working state as last reported once the writer has finished", () => {
    const node = recording(FINISHED);
    expect(lifetimeActive(node)).toBe(false);
    expect(first(node)).toMatch(/^last reported recording · /);
    expect(first(recording(FINISHED, report("draining")))).toMatch(/^last reported draining · /);
    expect(stateWord(node)).toBe("ok");
  });

  it("treats copies, restored work, another run's control and a withdrawal as no signal", () => {
    for (const node of [
      recording(undefined),
      recording(ACTIVE, report("recording"), { run: LATER, progress: { run: LATER, value: decodeProgress(report("recording")) } }),
      recording(ACTIVE, report("recording"), { accessWithdrawn: true }),
    ]) {
      expect(lifetimeActive(node)).toBe(false);
      expect(first(node)).toMatch(/^last reported recording · /);
      expect(stateWord(node)).toBe("ok");
    }
  });

  it("does not infer an active writer from the progress itself or from the command text", () => {
    const node = recording(undefined, report("recording"), { command: ":dataset record $synthetic > capture" });
    expect(lifetimeActive(node)).toBe(false);
    expect(first(node)).toMatch(/^last reported recording/);
  });
});

describe("what the active signal leaves alone", () => {
  it("keeps finite analysis progress exactly as before without an open lifetime, even beside a control", () => {
    const finite = { kind: "records", phase: "processing", counters: null };
    expect(first(recording(ACTIVE, finite, { openLifetime: undefined }))).toMatch(/^last reported processing/);
    expect(first(recording(undefined, finite))).toMatch(/^last reported processing/);
  });

  it("decides currency from the lifetime signal alone, never from the recording control", () => {
    // An active writer control without an open lifetime is not an open run; the controls stay offered.
    const controlOnly = recording(ACTIVE, report("recording"), { openLifetime: undefined });
    expect(lifetimeActive(controlOnly)).toBe(false);
    expect(first(controlOnly)).toMatch(/^last reported recording/);
    expect(recordingActions(controlOnly)).toEqual({ status: true, stop: true, discard: false, run: RUN });
    // An open lifetime without any control is an open run that offers no recording command.
    const openOnly = recording(undefined, report("recording"), { openLifetime: { run: RUN } });
    expect(lifetimeActive(openOnly)).toBe(true);
    expect(first(openOnly)).toMatch(/^recording · /);
    expect(recordingActions(openOnly)).toBeUndefined();
  });

  it("keeps a running recording current and a failed one's working report old", () => {
    expect(first(recording(ACTIVE, report("recording"), { state: "running" }))).toMatch(/^recording · /);
    expect(first(recording(ACTIVE, report("recording"), { state: "failed" }))).toMatch(/^last reported recording/);
  });

  it("draws an active recording as running in the graph, and a finished or unsignalled one as ok", () => {
    expect(graphState(recording(ACTIVE), undefined)).toBe("running");
    expect(graphState(recording(DRAINING, report("draining")), undefined)).toBe("running");
    expect(graphState(recording(FINISHED), undefined)).toBe("ok");
    expect(graphState(recording(undefined), undefined)).toBe("ok");
  });

  it("counts an active recording apart from completed results among several nodes", () => {
    const said = verdict(recording(ACTIVE, report("recording"), { id: "n1" }), recording(FINISHED, report("recording"), { id: "n2" }));
    expect(said).toContain("1 ok");
    expect(said).toContain("1 recording");
  });
});

describe("the recording's dataset reader", () => {
  const reference = (records: string, generation = "1"): DatasetReference => decodeDatasetReference({
    store: "00000000-0000-4000-8000-0000000000a7", dataset: "00000000-0000-4000-8000-0000000000b7", generation, manifest: "00000000-0000-4000-8000-0000000000c7",
    manifestDigest: `sha256:${"7".repeat(64)}`, manifestBytes: "512", schemaDigest: `sha256:${"8".repeat(64)}`, records, authorizationGeneration: "1",
  })!;
  const reply = (at: DatasetReference) => ({ reference: { ...at }, lifecycle: "open", protected: false, persistence: "Durable", segmentBytes: "4096" });

  it("refuses any reply for a later prefix than the frozen descriptor names", () => {
    const frozen = reference("20");
    expect(decodeDatasetRead(reply(frozen), frozen)).toMatchObject({ reference: frozen });
    expect(decodeDatasetRead(reply(reference("24")), frozen)).toBeUndefined();
    expect(decodeDatasetRead(reply(reference("20", "2")), frozen)).toBeUndefined();
  });

  it("asks for exactly the descriptor it was given, whatever the writer has done since", async () => {
    const frozen = reference("20");
    const value: StoredValue = { type: { kind: "dataset", element: { kind: "primitive", name: "TEXT" } }, provenance: {}, data: { kind: "dataset", reference: { ...frozen } } };
    const readDataset = vi.fn((): Promise<DatasetRead> => new Promise(() => undefined));
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ValueBlock engine={{ readDataset } as unknown as Engine} value={value} stored={{ handle: "h-lifetime", generation: "g-lifetime" }} cacheKey="lifetime" mode="window" inCell={false} />); });
    expect(readDataset).toHaveBeenCalled();
    for (const call of readDataset.mock.calls as unknown[][]) expect(call[2]).toEqual(frozen);
    act(() => tree.unmount());
  });
});
