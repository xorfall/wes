import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, it } from "vitest";
import { newCell } from "../cells";
import type { Engine } from "../engine";
import type { Event, StoredValue } from "../protocol";
import { apply, emptyWorkspace } from "../workspace";
import { Cell } from "./Cell";
import { cellBlocks } from "./cell-output";
import { ResultObservations } from "./results";
import { readSession } from "./session-model";
import { ValueBlock } from "./render/ValueBlock";

it.each(["cancelled", "skipped"] as const)("does not resurrect an older success when the latest %s run later becomes stale", state => {
  let workspace = apply(emptyWorkspace, { event: "created", dependencyLifetime: "continuous", node: "sample", name: "sample",
    command: "synthetic source", dependsOn: [], interactive: false });
  workspace = apply(workspace, { event: "ready", node: "sample", type: "Int", handle: "old-success",
    bytes: 8, provenance: {}, cautions: [], kept: true });
  const stored: StoredValue = { type: { kind: "primitive", name: "INT" }, data: 47, provenance: {} };
  const observations = new ResultObservations();
  observations.select(workspace.nodes, new Map([["old-success", stored]]), new Map());
  const outcome: Event = state === "cancelled"
    ? { event: "cancelled", node: "sample", code: "RUN002", reason: "Synthetic timeout" }
    : { event: "node", constructionComplete: false, node: "sample", state: "skipped" };
  workspace = apply(workspace, outcome);
  expect(observations.select(workspace.nodes, new Map(), new Map()).size).toBe(0);
  workspace = apply(workspace, { event: "node", constructionComplete: false, node: "sample", state: "stale",
    staleReason: { code: "dependency_changed", message: "An upstream definition changed." } });
  const observed = observations.select(workspace.nodes, new Map(), new Map());
  const client = { ...newCell("synthetic source"), state: "answered" as const, nodes: ["sample"] };
  const cell = readSession({ workspace, cells: [client], held: new Map(),
    context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
  const blocks = cellBlocks({ cell, workspace, held: new Map(), reads: new Map(), observations: observed,
    retryRead() {}, engine: {} as Engine, generation: "synthetic" });
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<Cell theme="keys" state={cell.state} rows={cell.rows} verdict={cell.verdict} blocks={blocks} />); });
  try {
    expect(tree.root.findAllByType(ValueBlock)).toHaveLength(0);
    expect(JSON.stringify(tree.toJSON())).toContain("stale · the value was not re-run");
    expect(JSON.stringify(tree.toJSON())).not.toContain("Stale · previous result");
    expect(observed.size).toBe(0);
  } finally { act(() => tree.unmount()); }
});
