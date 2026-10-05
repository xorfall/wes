import { expect, it } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { newCell } from "../cells";
import { emptyWorkspace } from "../workspace";
import type { Engine } from "../engine";
import type { Diagnostic, StoredValue } from "../protocol";
import { joined, readSession } from "./session-model";
import { cellBlocks } from "./cell-output";
import { lineText } from "./MonoLine";

const diagnostic = (message: string, severity: Diagnostic["severity"] = "info"): Diagnostic =>
  ({ code: "ENV000", severity, message, start: 0, end: 0, hints: [] });
function read(text: string, diagnostics: Diagnostic[]) {
  return readSession({ context: { workspace: "test", connection: "connected" }, workspace: emptyWorkspace, cells: [{ ...newCell(text), state: "answered" as const, diagnostics }] }).cells[0]!;
}
function render(cell: ReturnType<typeof read>, workspace = emptyWorkspace, held = new Map<string, StoredValue>()) {
  const blocks = cellBlocks({ cell, workspace, held, reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "test" });
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<>{blocks.map((block) => <div key={block.key}>{block.content}</div>)}</>); });
  return tree;
}
it.each([":inspect env:demo", ":list environments", ":help", ":policy mode:reactive"])("shows produced information for %s without pretending it kept a value", text => {
  const cell = read(text, [diagnostic("demo revision=123\nprovider: docker")]);
  expect(lineText(joined(cell.verdict))).toBe("ok");
  const tree = render(cell);
  expect(tree.root.findByProps({ "aria-label": "Command messages" })).toBeDefined();
  // Drawn line by line as the engine wrote it, never parsed.
  expect(JSON.stringify(tree.toJSON())).toContain("demo revision=123");
  expect(JSON.stringify(tree.toJSON())).toContain("provider: docker");
  act(() => tree.unmount());
});
it("keeps warnings beside a normal readable result", () => {
  const value: StoredValue = { type: {kind: "primitive", name: "INT"}, data: 42, provenance: {} };
  const workspace = { ...emptyWorkspace, nodes: [{ id: "n", command: "", name: "answer", dependsOn: [], state: "ready" as const, type: "Int", handle: "h", kept: false, provenance: {}, cautions: [] }] };
  const held = new Map([["h", value]]);
  const cell = readSession({ context: { workspace: "test", connection: "connected" }, workspace, held, cells: [{ ...newCell(":calc { return 42; }"), state: "answered" as const, nodes: ["n"], diagnostics: [diagnostic("Some observations were omitted.", "warning")] }] }).cells[0]!;
  const tree = render(cell, workspace, held);
  const output = JSON.stringify(tree.toJSON());
  expect(output).toContain("Some observations were omitted.");
  expect(output).toContain("42");
  expect(output).toContain("mono-warn");
  act(() => tree.unmount());
});
it("does not turn failures into informational success or fabricate messages for silent acknowledgments", () => {
  const failed = read(":env inspect absent", [diagnostic("Unexpected positional word.", "error")]);
  expect(lineText(joined(failed.verdict))).toContain("not run");
  expect(failed.notices).toEqual([]);
  const silent = render(read(":clear", []));
  expect(silent.toJSON()).toBeNull();
  act(() => silent.unmount());
});
it("shows existing document-plan guidance once", () => {
  const base = read(":env plan file:env.yaml > proposed", [diagnostic("Plan ready.")]);
  const tree = render({ ...base, documentNotice: "Plan ready.\nApply separately." });
  const output = JSON.stringify(tree.toJSON());
  expect(output.split("Plan ready.")).toHaveLength(2);
  expect(output).toContain("Apply separately.");
  act(() => tree.unmount());
});
