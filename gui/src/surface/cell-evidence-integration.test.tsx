import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { describe, expect, it } from "vitest";
import type { Engine } from "../engine";
import { newCell } from "../cells";
import type { StoredValue } from "../protocol";
import { emptyWorkspace, type WorkspaceNode } from "../workspace";
import { Cell } from "./Cell";
import { cellBlocks } from "./cell-output";
import { readSession } from "./session-model";

const failed: WorkspaceNode = {
  id: "failed-stage", name: "rejected", command: ":calc synthetic", dependsOn: [], state: "failed",
  provenance: {}, cautions: [], kept: false, failure: "SYN001: synthetic failure",
  failureRecord: { id: "error", code: "SYN001", message: "synthetic failure", causeId: "", issues: [] },
};
const textOf = (tree: ReactTestRenderer) => tree.root.findAll(node => typeof node.type === "string")
  .flatMap(node => node.children.filter(child => typeof child === "string")).join("");
function draw(nodes: WorkspaceNode[], held = new Map<string, StoredValue>()) {
  const workspace = { ...emptyWorkspace, nodes };
  const cell = readSession({ workspace, cells: [{ ...newCell(nodes.map(node => node.command).join("\n")),
    state: "answered", nodes: nodes.map(node => node.id) }], held,
    context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
  const blocks = cellBlocks({ cell, workspace, held, reads: new Map(), retryRead() {},
    engine: {} as Engine, generation: "synthetic" });
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<Cell theme="keys" state={cell.state} rows={cell.rows} verdict={cell.verdict} blocks={blocks} />); });
  return tree;
}

describe("execution evidence and results together", () => {
  it("shows a lone failure once and produces no empty result or data action group", () => {
    const tree = draw([failed]);
    try {
      const text = textOf(tree);
      expect(text.match(/synthetic failure/g)).toHaveLength(1);
      expect(text.match(/SYN001/g)).toHaveLength(1);
      expect(text).not.toContain("no stored value");
      expect(text).not.toContain("no value");
      expect(tree.root.findAll(node => node.props["data-node"] === failed.id)).toHaveLength(0);
      expect(tree.root.findAllByProps({ "aria-label": "Data actions" })).toHaveLength(0);
    } finally { act(() => tree.unmount()); }
  });

  it("preserves successful siblings and identifies each failed stage in a mixed pipeline", () => {
    const ready: WorkspaceNode = { ...failed, id: "ready-stage", name: "amount", state: "ready",
      handle: "ready-value", failure: undefined, failureRecord: undefined, type: "Int" };
    const tree = draw([ready, failed], new Map([["ready-value", {
      type: { kind: "primitive", name: "INT" }, data: 17, provenance: {},
    }]]));
    try {
      const text = textOf(tree);
      expect(text.match(/synthetic failure/g)).toHaveLength(1);
      expect(text.match(/SYN001/g)).toHaveLength(1);
      const run = tree.root.findByProps({ "data-zone": "run" });
      expect(run.findAllByProps({ className: "value-line value-error-stage" })).toHaveLength(1);
      expect(tree.root.findByProps({ "data-node": ready.id }).findByProps({ "data-zone": "data" })).toBeDefined();
      expect(text).toContain("17");
    } finally { act(() => tree.unmount()); }
  });
});
