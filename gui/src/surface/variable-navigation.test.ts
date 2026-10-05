import { expect, it } from "vitest";
import { read } from "./commands";
import { promptCompletion } from "./prompt-complete";
import { definitionTarget } from "./definition-target";
import { newCell } from "../cells";
import { apply, emptyWorkspace } from "../workspace";

it("separates definition navigation, value destinations and related command destinations", () => {
  for (const target of ["orders", "$orders"]) expect(read(`/goto ${target}`)).toEqual({ kind: "goto", node: "orders" });
  expect(read("/tab $orders pane:p2")).toEqual({ kind: "value-tab", node: "orders", pane: "p2", activate: false });
  expect(read("/tabx related $orders")).toEqual({ kind: "value-tab", node: "orders", related: true, activate: true });
  expect(read("/split $orders")).toMatchObject({ direction: "right", content: { value: "orders" } });
  expect(read("/split down related $orders")).toMatchObject({ direction: "down", content: { value: "orders", related: true } });
  expect(read("/split related $orders")).toMatchObject({ direction: "right", content: { value: "orders", related: true } });
  expect(read("/lsplitx related $orders")).toMatchObject({ direction: "left", takeFocus: true, content: { value: "orders", related: true } });
  expect(read('/tab "related"')).toMatchObject({ kind: "workspace-tab", workspace: "related" });
  expect(read('/rsplit "related"')).toMatchObject({ content: { workspace: "related" } });
  expect(read("/tab orders")).toMatchObject({ kind: "workspace-tab", workspace: "orders" });
  for (const line of ["/goto", "/goto $", "/goto $$orders", "/goto orders extra", "/tab $", "/tab related", "/rsplit related", "/tab $orders extra", "/tab related $orders extra", "/split $orders extra", "/split related", "/split related orders"])
    expect(read(line).kind, line).toBe("trouble");
});

it("completes workspace variables with either goto spelling and distinguishes workspace destinations", () => {
  const input = { catalogue: emptyWorkspace.catalogue, aliases: {}, names: ["node-id", "orders", "overview"], variables: ["orders", "overview"], workspaces: ["orders", "related"] };
  for (const line of ["/goto or", "/goto $or", "/tab $or", "/split related $or", "/split down $or", "/tabx related or"]) {
    const items = promptCompletion({ ...input, line, caret: line.length }).items;
    expect(items.map(item => item.text)).toEqual(line.endsWith("related or") ? [] : ["$orders"]);
  }
  for (const line of ["/goto ", "/tab related ", "/split related "]) {
    expect(promptCompletion({ ...input, line, caret: line.length }).items.map(item => item.text)).toEqual(["$orders", "$overview"]);
  }
  const line = "/tab ", items = promptCompletion({ ...input, line, caret: line.length }).items;
  expect(items.map(item => item.text)).toEqual(["related", "xterm", "orders", '"related"', "$orders", "$overview"]);
  expect(items.find(item => item.text === "xterm")?.detail).toBe("terminal");
  expect(items.some(item => item.text === "$node-id")).toBe(false);
});

it("finds an acknowledged definition without manufacturing a new cell", () => {
  const workspace = apply(emptyWorkspace, { event: "created", dependencyLifetime: "continuous", node: "node-id", name: "orders", command: ":calc 2 > orders", dependsOn: [], interactive: false });
  const cell = { ...newCell(":calc 2 > orders"), nodes: ["node-id"] };
  expect(definitionTarget(workspace, [cell], "orders")).toBe(cell);
  expect(() => definitionTarget(workspace, [], "orders")).toThrow("no longer available");
  expect(() => definitionTarget(workspace, [cell], "other")).toThrow("knows no result");
});
