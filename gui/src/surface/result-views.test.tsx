import { describe, expect, it } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { httpValue } from "../testing/http-response";
import { RESULT_VIEWS, RESULT_VIEW_NAMES, viewsFor, type ViewSubject } from "./result-views";
import { openRoute, readOpenRoute } from "./open-route";
import type { WorkspaceNode } from "../workspace";

const node = (over: Partial<WorkspaceNode> = {}): WorkspaceNode => ({
  id: "n1", command: "acme orders.list", dependsOn: [], state: "ready", provenance: {}, cautions: [], kept: true, ...over,
});
const trace = {
  type: { kind: "unknown" as const }, provenance: {},
  data: { schema: 1, profile: "http", run: "synthetic", state: "ready", events: [] },
};
const plain = { type: { kind: "unknown" as const }, provenance: {}, data: "ordinary" };

describe("which views a result admits", () => {
  it("points at nothing from an ordinary value the command row already shows whole", () => {
    const ordinary = { value: plain, node: node() };
    // Any command can be read as written; only one the row cannot show is worth pointing at.
    expect(viewsFor(ordinary).map(view => view.name)).toEqual(["source"]);
  });

  it("keeps numeric lists and single values in the basic result inspector", () => {
    const numbers = { type: { kind: "list" as const, element: { kind: "primitive" as const, name: "INT" } }, provenance: {}, data: [3, 1, 4] };
    expect(viewsFor({ value: numbers }).map(view => view.name)).toEqual([]);
    expect(viewsFor({ value: plain }).map(view => view.name)).toEqual([]);
  });

  it("names the HTTP response, the retained trace, the declared View and the written source", () => {
    const cases: [ViewSubject, string][] = [
      [{ value: httpValue() }, "http"],
      [{ value: trace }, "trace"],
      [{ node: node({ traced: true }) }, "trace"],
      [{ value: { type: { kind: "record", name:"Histogram",fields:[{name:"view",type:{kind:"primitive",name:"TEXT"}},{name:"bins",type:{kind:"list",element:{kind:"record",name:"Bin",fields:[{name:"lower",type:{kind:"primitive",name:"DECIMAL"}},{name:"upper",type:{kind:"primitive",name:"DECIMAL"}},{name:"count",type:{kind:"primitive",name:"INT"}}]}}},{name:"total",type:{kind:"primitive",name:"INT"}}]}, provenance: {}, data: { view: "histogram", bins: [], total: 0 } } }, "histogram"],
      [{ node: node({ command: ":calc {\n  return 1;\n}" }) }, "source"],
    ];
    for (const [subject, name] of cases) expect(viewsFor(subject)[0]?.name, name).toBe(name);
  });

  it("puts the response ahead of the trace of the call that fetched it", () => {
    const both = { value: httpValue(), node: node({ traced: true, command: "acme orders.list\n> orders" }) };
    expect(viewsFor(both).map(view => view.name)).toEqual(["http", "trace", "source"]);
    expect(viewsFor({ value: httpValue(), node: node() }).map(view => view.name)).toEqual(["http", "source"]);
  });

  it("gives every registered view a name the address can carry, and no two the same", () => {
    expect(new Set(RESULT_VIEW_NAMES).size).toBe(RESULT_VIEWS.length);
    for (const name of RESULT_VIEW_NAMES) {
      expect(readOpenRoute(openRoute("n1", name))).toEqual({ node: "n1", tab: name });
    }
    expect(readOpenRoute("#open/n1/not-a-view")).toEqual({ node: "n1", tab: "result" });
  });
});

it("draws the command as it was written, read only, with nothing to submit", async () => {
  const source = ':calc {\n\tconst label = "two  spaces";\n\n  return label;\n} > label';
  const view = RESULT_VIEWS.find(it => it.name === "source")!;
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(<view.Draw subject={{ node: node({ command: source }) }} />); });
  const shown = tree.root.findByType("textarea");
  expect(shown.props.value).toBe(source);
  expect(shown.props.readOnly).toBe(true);
  expect(shown.props.onChange).toBeUndefined();
  expect(tree.root.findAllByType("button")).toHaveLength(0);
  await act(async () => tree.unmount());
});
