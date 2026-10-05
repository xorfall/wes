import { expect, it } from "vitest";
import { durationsOf, readCell } from "./session-model";
import { lineText } from "./MonoLine";
import { newCell } from "../cells";
import { emptyWorkspace, type WorkspaceNode } from "../workspace";
import type { HistoryEvent } from "../protocol";
import { sessionContext } from "./session-fixture";
const node: WorkspaceNode = { id: "n", name: "producer", command: 'sh run cmd:"old"', run: "new", state: "ready", dependsOn: [], provenance: {}, cautions: [], kept: false };
const record = (run: string, state: string, ms: number): HistoryEvent => ({ event: "log", record: { id: `${run}-${state}-${ms}`, node: "n", run, state, at: new Date(ms).toISOString() } }) as HistoryEvent;
it("uses only the current run and its first completion; missing starts never reuse an old span", () => {
  const workspace = { ...emptyWorkspace, nodes: [node], history: [record("old", "RUNNING", 0), record("old", "READY", 5000), record("new", "RUNNING", 10000), record("new", "READY", 10035), record("old", "READY", 19000), record("new", "READY", 22000), record("new", "RUNNING", 23000)] };
  expect(durationsOf(workspace).get("n")).toEqual({ start: 10000, end: 10035 });
  expect(durationsOf({ ...workspace, history: workspace.history.filter(e => e.event !== "log" || e.record.state !== "RUNNING" || e.record.run !== "new") }).has("n")).toBe(false);
});
it("shows the changed definition and backend receipt while preserving submitted source", () => {
  const cell = { ...newCell(node.command), state: "answered" as const, nodes: [node.id], receipts: [{ summary: "refresh $producer · 1 execution requested" }] };
  const current = { ...node, currentDefinition: 'sh run cmd:"new"' };
  const result = readCell({ workspace: { ...emptyWorkspace, nodes: [current] }, context: sessionContext, cells: [cell] }, cell, new Date(0));
  expect(result.rows.map(row => lineText(row.segments)).join("\n")).toContain('cmd:"new"');
  expect(result.source).toBe(node.command);
  expect(result.notices?.some(n => n.message.includes("refresh $producer"))).toBe(true);
});

it("marks a changed pipeline stage without repeating the original pipeline in its header", () => {
  const source = 'sh run cmd:"old" > producer\n:calc { return $producer; } > result';
  const cell = { ...newCell(source), state: "answered" as const, nodes: ["n", "result"] };
  const current = { ...node, command: source, currentDefinition: 'sh run cmd:"new"' };
  const resultNode = { ...node, id: "result", name: "result", command: source };
  const result = readCell({ workspace: { ...emptyWorkspace, nodes: [current, resultNode] }, context: sessionContext, cells: [cell] }, cell, new Date(0));
  expect(result.rows.map(row => lineText(row.segments)).join("\n")).toBe(source);
  expect(result.notices?.some(n => n.message.includes('superseded for $producer') && n.message.includes('cmd:"new"'))).toBe(true);
});

it("keeps nominal iterator metadata separate from its structural item shape", async () => {
  const { prepareSync } = await import("../presentation/prepare");
  const { typeLine } = await import("../presentation/type-shape");
  const shape = { kind: "iter" as const, element: { kind: "primitive" as const, name: "TEXT" } };
  const prepared = prepareSync({ type: shape, data: { itemType: "TEXT", itemContract: "RequestEntry" } });
  expect(typeLine(prepared.type)).toBe("Iter<RequestEntry>");
  expect(prepared.type).toMatchObject({ element: shape.element });
  expect(typeLine(prepareSync({ type: shape, data: { itemType: "TEXT" } }).type)).toBe("Iter<Text>");
});

it("stream deliveries do not end a live run; cancellation closes the source lifetime", () => {
  const history = [record("new", "RUNNING", 100), record("new", "READY", 200), record("new", "READY", 300)];
  const live = { ...node, state: "running" as const, streamOutput: true };
  expect(durationsOf({ ...emptyWorkspace, nodes: [live], history }).has("n")).toBe(false);
  const stopped = { ...live, state: "cancelled" as const, stopped: { source: "n", run: "new" } };
  expect(durationsOf({ ...emptyWorkspace, nodes: [stopped], history: [...history, record("new", "CANCELLED", 400)] }).get("n")).toEqual({ start: 100, end: 400 });
});

it("reports mixed retention without claiming every result was kept", () => {
  const nodes = [{ ...node, id:"small", kept:true }, { ...node, id:"large", kept:false }];
  const cell = { ...newCell(":calc {return 1;} > small\n:calc {return 2;} > large"), state:"answered" as const, nodes:["small","large"] };
  const model = readCell({ workspace:{...emptyWorkspace,nodes}, context:sessionContext,cells:[cell] }, cell, new Date(0));
  expect(JSON.stringify(model)).toContain("1 of 2 kept");
});

it("does not count a failed stage with no result as an unkept result", () => {
  const nodes = [{...node,id:"small",kept:true},{...node,id:"failed",state:"failed" as const,kept:false}];
  const cell = {...newCell(":help"),state:"answered" as const,nodes:["small","failed"]};
  const model = readCell({workspace:{...emptyWorkspace,nodes},context:sessionContext,cells:[cell]},cell,new Date(0));
  expect(JSON.stringify(model)).not.toContain("1 of 2 kept");
});
