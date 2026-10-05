import { expect, it } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "../engine";
import type { StoredValue, TypeShape } from "../protocol";
import { emptyWorkspace, type WorkspaceNode } from "../workspace";
import { newCell } from "../cells";
import { cleanType, prepareSync, presentationType } from "../presentation/prepare";
import { typeLine } from "../presentation/type-shape";
import { cellBlocks } from "./cell-output";
import { Cell } from "./Cell";
import { ResultType } from "./ResultType";
import { readSession } from "./session-model";
import { peekText } from "./screens/Peek";

/* Synthetic recipe values only: the item contract is a property of the recipe data, never a run. */
const text: TypeShape = { kind: "primitive", name: "TEXT" };
const iter = (over: Record<string, unknown> = {}) => ({ kind: "iter", element: text, ...over }) as unknown as TypeShape;
const recipe = (type: TypeShape, data: unknown): StoredValue => ({ type, data, provenance: {} });

it("should_ShowTheNominalItemContract_When_ALedgerCellHoldsAnAnnotatedRecipe", () => {
  // Arrange
  const value = recipe(iter(), { itemType: "TEXT", itemContract: "SyntheticEntry" });
  const node: WorkspaceNode = { id: "n", name: "entries", command: ":calc synthetic", dependsOn: [], state: "ready", handle: "h", provenance: {}, cautions: [], kept: false, type: "Iter" };
  const workspace = { ...emptyWorkspace, nodes: [node] };
  const held = new Map([["h", value]]);
  const cell = readSession({ workspace, held, cells: [{ ...newCell(node.command), state: "answered", nodes: ["n"] }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
  // Act
  const blocks = cellBlocks({ cell, workspace, held, reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" });
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<Cell theme="keys" state={cell.state} rows={cell.rows} verdict={cell.verdict} blocks={blocks} />); });
  const shown = tree.root.findByType(ResultType);
  // Assert
  expect(shown.props.shape).toEqual(prepareSync(value).type);
  expect(shown.findByProps({ className: "cell-action result-type" }).children.join("")).toBe("Iter<SyntheticEntry>");
  expect(peekText("type", value)).toBe("Iter<SyntheticEntry>");
  // The recipe's data is untouched: nothing was collected to name its items.
  expect(value.data).toEqual({ itemType: "TEXT", itemContract: "SyntheticEntry" });
  act(() => tree.unmount());
});

it("should_KeepTheTypesOwnValidContract_When_TheDataCarriesNoAnnotation", () => {
  // Arrange
  const declared = iter({ contract: "DeclaredEntry" });
  // Act
  const cleaned = cleanType(declared);
  const shown = presentationType(recipe(declared, { itemType: "TEXT" }));
  const annotated = presentationType(recipe(declared, { itemContract: "AnnotatedEntry" }));
  // Assert
  expect(cleaned).toEqual({ kind: "iter", element: text, contract: "DeclaredEntry" });
  expect(typeLine(shown)).toBe("Iter<DeclaredEntry>");
  expect(typeLine(annotated)).toBe("Iter<AnnotatedEntry>");
});

it.each([
  ["blank annotation", iter(), { itemContract: "   " }, "Iter<Text>"],
  ["empty annotation", iter(), { itemContract: "" }, "Iter<Text>"],
  ["non-text annotation", iter(), { itemContract: 7 }, "Iter<Text>"],
  ["array data", iter(), ["SyntheticEntry"], "Iter<Text>"],
  ["no data", iter(), undefined, "Iter<Text>"],
  ["empty declared contract", iter({ contract: "" }), {}, "Iter<Text>"],
  ["blank declared contract", iter({ contract: " " }), { itemContract: " " }, "Iter<Text>"],
  ["non-text declared contract", iter({ contract: { name: "x" } }), {}, "Iter<Text>"],
  ["blank annotation over a valid declaration", iter({ contract: "DeclaredEntry" }), { itemContract: "" }, "Iter<DeclaredEntry>"],
  ["annotation on a list", { kind: "list", element: text } as TypeShape, { itemContract: "SyntheticEntry" }, "List<Text>"],
  ["malformed iterator element", { kind: "iter" } as unknown as TypeShape, { itemContract: "SyntheticEntry" }, "Iter<SyntheticEntry>"],
  ["malformed record", { kind: "record" } as unknown as TypeShape, {}, "{}"],
  ["not a type", "Iter<Text>" as unknown as TypeShape, {}, "Unknown"],
])("should_FallBackSafely_When_TheMetadataIsMalformed: %s", (_name, type, data, expected) => {
  // Arrange
  const value = recipe(type, data);
  // Act
  const shown = presentationType(value);
  // Assert
  expect(typeLine(shown)).toBe(expected);
  expect(prepareSync(value).type).toEqual(shown);
});
