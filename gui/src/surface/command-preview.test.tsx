import { act, create } from "react-test-renderer";
import { expect, it, vi } from "vitest";
import { Cell } from "./Cell";
import { lineText } from "./MonoLine";
import { readSession, rowsOf } from "./session-model";
import { newCell } from "../cells";
import { emptyWorkspace } from "../workspace";

it("bounds source-band text before highlighting and keeps the complete submitted source", () => {
  const source = `:calc pure { return [${"12345,".repeat(100_000)}0]; } > numbers`;
  const cell = newCell(source);
  const model = readSession({ workspace: emptyWorkspace, cells: [cell], context: { workspace: "fixture", connection: "connected" } });
  const shown = model.cells[0]!;
  expect(shown.source).toBe(source);
  expect(shown.chars).toBe(source.length);
  expect(shown.sourceTruncated).toBe(true);
  expect(shown.rows.map(row => lineText(row.segments)).join("\n").length).toBeLessThanOrEqual(2048);
  expect(shown.rows.flatMap(row => row.segments).length).toBeLessThan(20);
});

it("does not split a cluster or diagnose the cut as a malformed string", () => {
  const lead = ':calc pure { return "';
  const source = lead + "x".repeat(2048 - lead.length - 1) + "e\u0301" + "tail".repeat(1000) + '"; }';
  const { rows } = rowsOf(source, [], new Map());
  const shown = rows.map(row => lineText(row.segments)).join("\n");
  expect(source.startsWith(shown)).toBe(true);
  expect(shown.endsWith("e")).toBe(false);
  expect(shown.length).toBeLessThanOrEqual(2048);
  expect(rows.flatMap(row => row.segments).some(segment => segment.role === "mono-bad")).toBe(false);
});

it("opens shortened commands read only without unfolding or repeating them", () => {
  const openSource = vi.fn(), repeat = vi.fn(), edit = vi.fn();
  let tree!: ReturnType<typeof create>;
  act(() => { tree = create(<Cell theme="controls" state="default" rows={[{segments:[{text:":calc pure { return [1,…"}],nodes:[]}]} sourceTruncated verdict={[]} actions={{openSource, repeat, edit}} />); });
  const button = tree.root.findAllByType("button").find(button => button.children.includes("open full command"));
  expect(button).toBeDefined();
  act(() => button!.props.onClick());
  expect(openSource).toHaveBeenCalledOnce();
  expect(repeat).not.toHaveBeenCalled();
  expect(edit).not.toHaveBeenCalled();
  expect(tree.root.findAllByType("button").some(button => button.children.includes("▾ fold command"))).toBe(false);
  act(() => tree.unmount());
});
