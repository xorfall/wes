import { expect, it, vi } from "vitest";
import { newCell } from "../cells";
import { apply, emptyWorkspace } from "../workspace";
import { connectedCells, connectedNodes, sharedCellEvent } from "./component-model";
import { read } from "./commands";
import { applyPaneCommand } from "./pane-command";
import { divide, oneP, SESSION_PANE } from "./split-model";
import { restoreSplit } from "./split-storage";

const nodes = [
  { id: "A", dependsOn: [] }, { id: "B", dependsOn: ["A"] }, { id: "C", dependsOn: ["B", "X"] },
  { id: "X", dependsOn: [] }, { id: "Z", dependsOn: ["X"] }, { id: "unrelated", dependsOn: [] },
];
it("traverses joins and sibling branches recursively in both directions", () => {
  expect([...connectedNodes(nodes, "B")].sort()).toEqual(["A", "B", "C", "X", "Z"]);
  expect([...connectedNodes(nodes, "C")].sort()).toEqual(["A", "B", "C", "X", "Z"]);
  expect([...connectedNodes(nodes, "unrelated")]).toEqual(["unrelated"]);
  expect([...connectedNodes(nodes, "missing")]).toEqual([]);
});
it("handles cycles and absent references without crossing phantom nodes", () => {
  expect([...connectedNodes([{ id: "a", dependsOn: ["b"] }, { id: "b", dependsOn: ["a", "absent"] }, { id: "c", dependsOn: ["absent"] }], "a")].sort()).toEqual(["a", "b"]);
});
it("preserves owning cell order and identity, including multi-node groups, without adding disconnected work", () => {
  const cells = ["unrelated", "C", "A", "B", "X"].map(id => ({ ...newCell(id), nodes: id === "B" ? ["B", "group-peer"] : [id] }));
  const result = connectedCells(cells, nodes, "B");
  expect(result.map(cell => cell.text)).toEqual(["C", "A", "B", "X"]);
  expect(result[0]).toBe(cells[1]); expect(result[2]!.nodes).toEqual(["B", "group-peer"]);
});
it("updates component membership after new dependencies and removes the projection when its root disappears", () => {
  expect(connectedNodes([...nodes, { id: "joined", dependsOn: ["Z"] }], "B").has("joined")).toBe(true);
  expect(connectedNodes(nodes.filter(node => node.id !== "B"), "B").size).toBe(0);
});
it.each(["l", "r", "t", "b"])("parses /%ssplit $node without interpreting it as an open screen", prefix => {
  expect(read(`/${prefix}split $B`)).toMatchObject({ kind: "directional-split", takeFocus: false, content: { value: "B" } });
  expect(read(`/${prefix}splitx $id1001`)).toMatchObject({ takeFocus: true, content: { value: "id1001" } });
  expect(read(`/${prefix}split $`).kind).toBe("trouble");
  expect(read(`/${prefix}split $B extra`).kind).toBe("trouble");
});
it("resolves a component exactly once, respects capacity and preserves the generation in saved layouts", () => {
  const fresh = oneP(SESSION_PANE), resolve = vi.fn(() => ({ node: "id1001", generation: "workspace-g", label: "B" }));
  const split = applyPaneCommand(fresh, "/lsplitx related $B", "p1", shown => shown, false, resolve);
  expect(resolve).toHaveBeenCalledExactlyOnceWith("B");
  expect(split.focused).toBe("p2");
  expect(split.panes[1]!.value).toEqual({ node: "id1001", generation: "workspace-g", label: "B", related: true });
  expect(restoreSplit(JSON.parse(JSON.stringify(split)))).toEqual(split);
  expect(() => applyPaneCommand(divide(fresh, 4), "/rsplit $B", "p1", shown => shown, false, resolve)).toThrow("The pane budget is full");
  expect(resolve).toHaveBeenCalledOnce();
  const invalid = { ...split, panes: [split.panes[0], { ...split.panes[1], value: { node: "id1001" } }] };
  expect(restoreSplit(invalid)).toEqual(fresh);
});
it("refuses unknown references without installing a pane", () => {
  const fresh = oneP(SESSION_PANE);
  expect(() => applyPaneCommand(fresh, "/rsplit $missing", "p1", shown => shown, false, () => { throw new Error("Unknown node"); })).toThrow("Unknown node");
  expect(fresh.panes).toHaveLength(1);
});
it("keeps one acknowledged cell across repeats, including another pane's work, and clears on session replacement", () => {
  const planned = { event: "planned" as const, cell: "other-pane", text: ":calc 1", nodes: ["A"] };
  const initial = sharedCellEvent([], planned);
  const repeated = sharedCellEvent(initial, { ...planned, cell: "repeated", repeatOf: "other-pane" });
  expect(repeated).toHaveLength(1); expect(repeated[0]!.lastRun).toBe("repeated");
  expect(sharedCellEvent(repeated, { event: "session", generation: "another" })).toEqual([]);
  expect(apply(emptyWorkspace, { event: "session", generation: "another" }).nodes).toEqual([]);
});

it("retires shared work without removing another pane's surviving cell", () => {
  const initial = sharedCellEvent([], { event: "planned", cell: "removed", text: "bad", nodes: [] });
  const both = sharedCellEvent(initial, { event: "planned", cell: "kept", text: "good", nodes: ["K"] });
  expect(sharedCellEvent(both, { event: "work-retired", cells: ["removed"] })).toEqual([both[1]]);
});
