import { readFileSync } from "node:fs";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, it, vi } from "vitest";
import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";
import { apply, emptyWorkspace, type WorkspaceNode } from "../workspace";
import { ResultDemand, ResultObservations, useResults } from "./results";
import { cellBlocks } from "./cell-output";
import { Cell as OutputCell } from "./Cell";
import { ObservationStatus } from "./ObservationStatus";
import { readSession } from "./session-model";
import { newCell } from "../cells";
import { ValueBlock } from "./render/ValueBlock";

const value = (n: number): StoredValue => ({ type: { kind: "primitive", name: "INT" }, data: n, provenance: {} } as StoredValue);
const node = (over: Partial<WorkspaceNode> = {}): WorkspaceNode => ({ id: "n", name: "sample", command: "synthetic", dependsOn: [], state: "ready", handle: "h1", provenance: {}, cautions: [], kept: false, ...over });
const flush = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };
function delayed() {
  const waiting = new Map<string, (value: StoredValue) => void>();
  const fetch = vi.fn((handle: string) => new Promise<StoredValue>(resolve => waiting.set(handle, resolve)));
  return { waiting, fetch };
}

it("bounds reads and skips thousands of obsolete requests while transport is busy", async () => {
  const { waiting, fetch } = delayed();
  const demand = new ResultDemand(fetch, () => {});
  demand.update(["a", "b"]);
  for (let i = 0; i < 2000; i++) demand.update([`latest-${i}`]);
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(demand.reads.size).toBe(0);
  waiting.get("a")!(value(1)); await flush();
  expect(fetch.mock.calls.map(([key]) => key)).toEqual(["a", "b", "latest-1999"]);
  waiting.get("b")!(value(2)); waiting.get("latest-1999")!(value(3)); await flush();
  expect([...demand.reads.keys()]).toEqual(["latest-1999"]);
  expect(demand.reads.get("latest-1999")?.value?.data).toBe(3);
  demand.dispose();
  expect(demand.reads.size).toBe(0);
});

it("keeps finite per-result read errors and retry on their own existing header", () => {
  const observations = new ResultObservations();
  const retryRead = vi.fn();
  const nodes = [node(), node({ id: "other", name: "other", handle: "other1" })];
  observations.select(nodes, new Map([["h1", value(1)], ["other1", value(2)]]), new Map());
  const client = { ...newCell("synthetic"), state: "answered" as const, nodes: ["n", "other"] };
  const current = [node({ handle: "h2" }), nodes[1]!];
  const workspace = { ...emptyWorkspace, nodes: current };
  const held = new Map([["other1", value(2)]]);
  const reads = new Map([["h2", { problem: "synthetic read failure" }]]);
  const cell = readSession({ workspace, cells: [client], held, context: { workspace: "qa", connection: "connected" } }).cells[0]!;
  const blocks = cellBlocks({ cell, workspace, held, reads, observations: observations.select(current, held, reads), retryRead, engine: {} as Engine, generation: "g" });
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<OutputCell theme="keys" state={cell.state} rows={cell.rows} verdict={cell.verdict} blocks={blocks} />); });
  const first = tree.root.findByProps({ "data-node": "n" });
  const heading = first.findByProps({ "data-zone":"data" });
  expect(JSON.stringify(heading.findByType(ObservationStatus).props)).toContain("synthetic read failure");
  expect(first.findByType(ValueBlock).props.value.data).toBe(1);
  const retry = heading.findAllByType("button").find(button => button.children.includes("retry reading result"))!;
  act(() => retry.props.onClick()); expect(retryRead).toHaveBeenCalledWith("h2");
  expect(retry.parent!.type).not.toBe("button");
  expect(tree.root.findByProps({ "data-node": "other" }).findByType(ObservationStatus).props.observation.state).toBe("current");
  act(() => tree.unmount());
});

it("retains only current demand after thousands of completed snapshots", async () => {
  const demand = new ResultDemand(async () => value(1), () => {});
  for (let i = 0; i < 2000; i++) {
    demand.update([`h${i}`]); await flush();
    expect(demand.reads.size).toBe(1);
  }
  demand.update([]);
  expect(demand.reads.size).toBe(0);
});

it("keeps exactly the previous public observation through publication and reads", () => {
  const observations = new ResultObservations();
  observations.select([node()], new Map([["h1", value(1)]]), new Map());
  const pending = node({ handle: undefined, publication: { state: "pending", handle: null, run: "r2", uncertainHandle: null, problem: null, message: "publishing" } });
  expect(observations.select([pending], new Map(), new Map()).get("n")).toMatchObject({ handle: "h1", state: "updating" });
  const replacement = node({ handle: "h2" });
  expect(observations.select([replacement], new Map(), new Map([["h2", { problem: "read failed" }]])).get("n")).toMatchObject({ state: "updating", problem: "read failed" });
  expect(observations.select([replacement], new Map([["h2", value(2)]]), new Map()).get("n")).toMatchObject({ handle: "h2", state: "current", value: { data: 2 } });
});

it.each([
  { state: "failed" }, { state: "cancelled" }, { state: "skipped" }, { private: true },
  { command: "changed" }, { publication: { state: "unavailable", handle: null, run: "r2", uncertainHandle: null, problem: null, message: "revoked" } },
] as Partial<WorkspaceNode>[])("withdraws previous observations at lifecycle/authority boundaries: %j", over => {
  const observations = new ResultObservations();
  observations.select([node()], new Map([["h1", value(1)]]), new Map());
  expect(observations.select([node({ handle: "h2", ...over })], new Map(), new Map()).size).toBe(0);
  expect(observations.select([node({ handle: "h3" })], new Map(), new Map()).size).toBe(0);
});

it("does not resurrect removed nodes or observations across generation changes", async () => {
  const { waiting, fetch } = delayed();
  let current!: ReturnType<typeof useResults>;
  function Probe({ generation, nodes }: { generation: string; nodes: WorkspaceNode[] }) {
    current = useResults(engine, generation, nodes.flatMap(n => n.handle ? [n.handle] : []), nodes); return null;
  }
  const engine = { fetch } as unknown as Engine;
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(<Probe generation="g1" nodes={[node()]} />); });
  await act(async () => { waiting.get("h1")!(value(1)); await flush(); });
  expect(current.observations.get("n")?.value.data).toBe(1);
  await act(async () => { tree.update(<Probe generation="g2" nodes={[node({ handle: "h2" })]} />); });
  expect(current.held.size).toBe(0); expect(current.observations.size).toBe(0);
  await act(async () => { tree.update(<Probe generation="g2" nodes={[]} />); waiting.get("h2")!(value(2)); await flush(); });
  expect(current.held.size).toBe(0); expect(current.observations.size).toBe(0);
  act(() => tree.unmount());
});

it.each([false])("keeps the value and status placement stable through replacement (stream: %s)", streamOutput => {
  const observations = new ResultObservations();
  const client = { ...newCell("synthetic"), state: "answered" as const, nodes: ["n"] };
  function Cell({ current, held }: { current: WorkspaceNode; held: Map<string, StoredValue> }) {
    current = { ...current, streamOutput };
    const workspace = { ...emptyWorkspace, nodes: [current] };
    const cell = readSession({ workspace, cells: [client], held, context: { workspace: "test", connection: "connected" } }).cells[0]!;
    const blocks = cellBlocks({ cell, workspace, held, reads: new Map(), observations: observations.select([current], held, new Map()), retryRead() {}, engine: {} as Engine, generation: "g" });
    return <OutputCell theme="keys" state={cell.state} rows={cell.rows} verdict={cell.verdict} streamOutput={cell.streamOutput} blocks={blocks} />;
  }
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<Cell current={node()} held={new Map([["h1", value(1)]])} />); });
  const mounted = tree.root.findByType(ValueBlock);
  const wrapperClass = streamOutput ? "result-observation" : "result-observation result-observation-inline";
  const wrapper = tree.root.findByProps({ className: wrapperClass });
  const status = tree.root.findByType(ObservationStatus);
  expect(status.props.inline).toBe(!streamOutput);
  expect(wrapper.findAllByType(ObservationStatus)).toHaveLength(streamOutput ? 1 : 0);
  // Finite status is its own flow row in the data zone, outside the header controls, and silent while current.
  const row = tree.root.findByProps({ className: "cell-data-status" });
  expect(row.findByType(ObservationStatus)).toBe(status);
  expect(tree.root.findByProps({ className: "result-controls" }).findAllByType(ObservationStatus)).toHaveLength(0);
  expect(status.findByProps({ role: "status" }).props["aria-hidden"]).toBe(true);
  // Actual reactive wire transition, observed in the finite Docker acceptance run.
  const staleReason = { code: "stream_updated", message: "An upstream stream changed its data or availability; this result is no longer current." };
  const stale = apply({ ...emptyWorkspace, nodes: [node()] }, { event: "node", constructionComplete: false, node: "n", state: "stale", staleReason });
  act(() => { tree.update(<Cell current={stale.nodes[0]!} held={new Map([["h1", value(1)]])} />); });
  expect(tree.root.findByType(ValueBlock)).toBe(mounted);
  expect(JSON.stringify(tree.toJSON())).toContain("Stale · previous result");
  expect(JSON.stringify(tree.toJSON())).toContain(staleReason.message);
  expect(tree.root.findByProps({ className: "cell-data-status" })).toBe(row);
  expect(status.findByProps({ role: "status" }).props["aria-hidden"]).toBe(false);
  // The last observation also survives if the old handle leaves current demand.
  act(() => { tree.update(<Cell current={{ ...stale.nodes[0]!, handle: undefined }} held={new Map()} />); });
  expect(tree.root.findByType(ValueBlock)).toBe(mounted);
  expect(JSON.stringify(tree.toJSON())).toContain("Stale · previous result");
  const publishing = apply(stale, { event: "node", constructionComplete: false, node: "n", state: "ready", publication: {
    state: "pending", run: "r2", handle: null, uncertainHandle: null, problem: null, message: "The result is being published.",
  } });
  expect(publishing.nodes[0]!.handle).toBeUndefined();
  act(() => { tree.update(<Cell current={publishing.nodes[0]!} held={new Map()} />); });
  expect(tree.root.findByType(ValueBlock)).toBe(mounted);
  expect(JSON.stringify(tree.toJSON())).toContain("Updating · previous result");
  expect(JSON.stringify(tree.toJSON())).not.toContain("no result");
  act(() => { tree.update(<Cell current={node({ handle: "h2" })} held={new Map()} />); });
  expect(tree.root.findByType(ValueBlock)).toBe(mounted);
  expect(JSON.stringify(tree.toJSON())).toContain("Updating · previous result");
  expect(JSON.stringify(tree.toJSON())).not.toContain("no result");
  act(() => { tree.update(<Cell current={node({ handle: "h2" })} held={new Map([["h2", value(2)]])} />); });
  expect(tree.root.findByType(ValueBlock)).toBe(mounted);
  expect(tree.root.findByType(ValueBlock).props.value.data).toBe(2);
  expect(tree.root.findByProps({ className: wrapperClass })).toBe(wrapper);
  expect(tree.root.findByType(ObservationStatus)).toBe(status);
  act(() => tree.unmount());
});

it("styles the finite status as an in-flow row that takes no height while current, keeping the live overlay", () => {
  const css = readFileSync(new URL("./surface.css", import.meta.url), "utf8");
  expect(css).toMatch(/\.result-observation-status \{[^}]*position: absolute;/);
  expect(css).toMatch(/\.result-observation-status-inline \{[^}]*position: static;[^}]*height: auto;/);
  expect(css).toMatch(/\.result-observation-status-inline\[aria-hidden=true\] \{ display: none; \}/);
});
