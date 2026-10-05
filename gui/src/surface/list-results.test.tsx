import { expect, it } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "../engine";
import type { StoredValue, TypeShape } from "../protocol";
import { emptyWorkspace } from "../workspace";
import { newCell } from "../cells";
import { cellBlocks } from "./cell-output";
import { readSession } from "./session-model";
import { OpenScreen } from "./screens/Open";
import { MonoLine, lineText } from "./MonoLine";

/* Synthetic lists only. The cell packs scalars on one preview line; the window lists one per line. */
const cases: [string, TypeShape, unknown[], string[], string[]][] = [
  ["integers", { kind: "primitive", name: "INT" }, [1, 2, 3], ["1 · 2 · 3"], ["1", "2", "3"]],
  ["decimals", { kind: "primitive", name: "DECIMAL" }, [1.25, -2.5], ["1.25 · -2.5"], ["1.25", "-2.5"]],
  ["empty", { kind: "primitive", name: "INT" }, [], ["no items"], ["no items"]],
  ["text", { kind: "primitive", name: "TEXT" }, ["one", "two"], ["one · two"], ["one", "two"]],
  ["records", { kind: "record", name: "Item", fields: [{ name: "n", type: { kind: "primitive", name: "INT" } }] }, [{ n: 1 }, { n: 2 }], ["n", "1", "2"], ["n", "1", "2"]],
];

const said = (node: ReactTestInstance): string => node.children.map((child) => (typeof child === "string" ? child : said(child))).join("");
/** Every mono line, then every table cell (a table draws cells, not lines), as trimmed text. */
const lines = (tree: ReactTestRenderer) => [
  ...tree.root.findAllByType(MonoLine).map((line) => lineText(line.props.segments).trimEnd()),
  ...tree.root.findAll((node) => (node.type === "td" || node.type==="th") && (node.props.role === "cell" || node.props.role === "columnheader")).map((cell) => said(cell).replace(/^↳/, "").replace(/^n(?=n$)/,"n").trimEnd()),
];

it.each(cases)("should_PresentTheSameValuesInTheCellAndTheWindow_When_TheValueIsAListOf %s", (_name, element, data, inCellExpected, inWindowExpected) => {
  // Arrange
  const value: StoredValue = { type: { kind: "list", element }, data, provenance: {} };
  const node = { id: "n", handle: "h", command: ":calc []", dependsOn: [], state: "ready" as const, kept: false, provenance: {}, cautions: [], type: "List" };
  const workspace = { ...emptyWorkspace, nodes: [node] };
  const held = new Map([["h", value]]);
  const cell = readSession({ workspace, held, cells: [{ ...newCell(node.command), state: "answered", nodes: ["n"] }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
  // Act
  const blocks = cellBlocks({ cell, workspace, held, reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" });
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<>{blocks.map((block) => <div key={block.key}>{block.content}</div>)}</>); });
  const inCell = lines(tree);
  act(() => tree.update(<OpenScreen top={[]} subject={[]} tab="result" value={value} viewing={{ value }} />));
  const inWindow = lines(tree);
  // Assert
  for (const line of inCellExpected) expect(inCell).toContain(line);
  for (const line of inWindowExpected) expect(inWindow).toContain(line);
  // No chart is drawn; the only pictures are the table controls' icons.
  expect(tree.root.findAllByType("svg").filter((svg) => svg.props.className !== "value-table-icon")).toHaveLength(0);
  act(() => tree.unmount());
});
