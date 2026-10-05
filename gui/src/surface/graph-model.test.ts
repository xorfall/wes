import { describe, expect, it } from "vitest";
import { emptyWorkspace, type Workspace, type WorkspaceNode } from "../workspace";
import type { StoredValue } from "../protocol";
import { becauseOf, cyclesIn, dependentsOf, detailOf, edgesOf, madeByOf, readGraph, shapeOf } from "./graph-model";

/**
 * A synthetic workspace, built as the engine would have left it.
 *
 * `$acme` feeds `$orders`, which feeds `$totals` and `$daily`; `$totals` went stale because the
 * cell that made `$orders` was run again. Nothing here is a live value or a real provider.
 */
function node(over: Partial<WorkspaceNode> & { id: string; command: string }): WorkspaceNode {
  return { dependsOn: [], state: "ready", provenance: {}, cautions: [], kept: false, ...over };
}

const nodes: WorkspaceNode[] = [
  node({ id: "acme", name: "acme", command: "acme connect", type: "Service", startedAt: "2026-09-20T09:10:00.000" }),
  node({
    id: "orders", name: "orders", command: "acme orders.list", dependsOn: ["acme"],
    type: "List<Order>", bytes: 2048, handle: "h-orders", startedAt: "2026-09-20T09:16:00.000",
  }),
  node({ id: "totals", name: "totals", command: ":calc totals", dependsOn: ["orders"], state: "stale", type: "Record" }),
  node({ id: "daily", name: "daily", command: ":calc daily", dependsOn: ["orders"], state: "stale" }),
  node({ id: "spread", name: "spread", command: ":calc spread", dependsOn: ["totals"], state: "running" }),
  node({ id: "id7", command: "acme customer.get id:99999", state: "failed", failure: "not found. try another id" }),
];

const workspace: Workspace = {
  ...emptyWorkspace,
  nodes,
  cells: { "attempt-1": ["acme"], "attempt-2": ["orders"], "attempt-3": ["totals", "daily"] },
  repeatedRuns: { "attempt-2": "run-9" },
};

const held = new Map<string, StoredValue>([
  ["h-orders", { type: { kind: "list", element: { kind: "record", name: "Order", fields: [] } }, provenance: {}, data: [{}, {}, {}] }],
]);

describe("the graph the workspace is", () => {
  it("should_SayNothingIsThere_When_TheWorkspaceHasNoNodes", () => {
    const view = readGraph(emptyWorkspace);
    expect(view.nodes).toEqual([]);
    expect(view.edges).toEqual([]);
    expect(view.cycles).toEqual([]);
    expect(view.selected).toBeUndefined();
  });

  it("should_DrawOneEdgePerDependency_When_BothEndsAreStillThere", () => {
    expect(edgesOf(nodes)).toEqual([
      { from: "acme", to: "orders" },
      { from: "orders", to: "totals" },
      { from: "orders", to: "daily" },
      { from: "totals", to: "spread" },
    ]);
  });

  it("should_IgnoreADependencyOnANodeThatIsGone_When_TheWorkspaceDroppedIt", () => {
    expect(edgesOf([node({ id: "one", command: "x", dependsOn: ["vanished"] })])).toEqual([]);
  });

  it("should_TakeEachNodesStateFromTheEngine_When_ItIsRead", () => {
    const view = readGraph(workspace);
    expect(Object.fromEntries(view.nodes.map((it) => [it.id, it.state]))).toEqual({
      acme: "ok", orders: "ok", totals: "stale", daily: "stale", spread: "running", id7: "failed",
    });
  });

  it("should_MarkTheChosenNodeSelected_When_ItIsStaleAsWell", () => {
    const view = readGraph(workspace, { selected: "totals" });
    expect(view.nodes.find((it) => it.id === "totals")?.state).toBe("selected");
  });

  it("should_NameANodeWithADollar_When_SomebodyNamedIt", () => {
    const view = readGraph(workspace);
    expect(view.nodes.map((it) => it.name)).toContain("$orders");
    expect(view.nodes.map((it) => it.name)).toContain("id7");
  });

  it("should_SayWhatANodeHoldsOrWhatIsHappeningToIt_When_ItIsDescribed", () => {
    expect(detailOf(nodes[1]!)).toBe("List<Order> · 2.0 KB");
    expect(detailOf(nodes[2]!)).toBe("stale · Record");
    expect(detailOf(nodes[4]!)).toBe("running…");
    expect(detailOf({ ...nodes[4]!, updatePending: true })).toBe("running… · newer input waiting");
    expect(detailOf(nodes[5]!)).toBe("not found");
  });
});

describe("stale only", () => {
  it("should_KeepTheStaleNodesAndWhatTheyReach_When_ItIsOn", () => {
    const view = readGraph(workspace, { staleOnly: true });
    expect(view.nodes.map((it) => it.id).sort()).toEqual(["daily", "spread", "totals"]);
  });

  it("should_DropAnEdgeWhoseOtherEndWasFiltered_When_ItIsOn", () => {
    const view = readGraph(workspace, { staleOnly: true });
    expect(view.edges).toEqual([{ from: "totals", to: "spread" }]);
  });
});

describe("cycles", () => {
  it("should_FindNone_When_NothingDependsOnItself", () => {
    expect(cyclesIn(nodes, edgesOf(nodes))).toEqual([]);
  });

  it("should_ListAKnotOnce_When_TwoNodesDependOnEachOther", () => {
    const knotted = [
      node({ id: "a", name: "a", command: "x", dependsOn: ["b"] }),
      node({ id: "b", name: "b", command: "y", dependsOn: ["a"] }),
      node({ id: "c", name: "c", command: "z", dependsOn: ["b"] }),
    ];
    expect(cyclesIn(knotted, edgesOf(knotted))).toEqual([["$a", "$b"]]);
  });

  it("should_CountANodeThatDependsOnItself_When_TheEngineAllowedIt", () => {
    const loop = [node({ id: "a", name: "a", command: "x", dependsOn: ["a"] })];
    expect(cyclesIn(loop, edgesOf(loop))).toEqual([["$a"]]);
  });
});

describe("the selection panel", () => {
  const view = readGraph(workspace, { selected: "totals", held });

  it("should_CountEveryDependentHoweverFar_When_ItSaysWhatBreaks", () => {
    expect(dependentsOf(edgesOf(nodes), "orders")).toEqual(new Set(["totals", "daily", "spread"]));
    expect(view.selected?.breaks).toBe(1);
  });

  it("should_NotInferTheCauseFromUnrelatedRetryHistory", () => {
    expect(view.selected?.because).toBe("The reason this result became stale was not recorded.");
  });

  it("should_ExplainWhenTheCauseWasNotRecorded", () => {
    expect(becauseOf({ ...workspace, repeatedRuns: {} }, nodes[2]!, edgesOf(nodes))).toBe("The reason this result became stale was not recorded.");
  });

  it("should_NameTheCellByItsTime_When_TheNodeSaysWhenItStarted", () => {
    expect(madeByOf(workspace, nodes[1]!)).toMatch(/^cell \d\d:\d\d$/);
  });

  it("should_SayHowManyRowsTheValueHas_When_ItHasBeenFetchedBack", () => {
    expect(shapeOf(nodes[1]!, held)).toBe("List<Order> · 3 rows · 2.0 KB");
  });

  it("should_FallBackToWhatTheNodeSaid_When_TheValueIsNotFetchedYet", () => {
    expect(shapeOf(nodes[2]!, new Map())).toBe("Record");
  });
});

/*
 * A value the client cannot read is a value drawn as unknown, never a client that stops drawing.
 *
 * `/values/<handle>` is read with a cast, so what arrives is whatever the engine encoded. This is
 * asked on every render of the session — the graph is projected whether or not `/graph` is open —
 * so reaching into a type that did not arrive took the whole client down from the scrollback, with
 * nothing clicked. Every reader of a stored type now asks instead.
 */
describe("a stored value the client cannot read", () => {
  const node: WorkspaceNode = {
    id: "id7", command: "sh run cmd:\"echo\"", dependsOn: [], state: "ready",
    provenance: {}, cautions: [], kept: true, handle: "h7", bytes: 64,
  };
  const held = (type: unknown, data: unknown) =>
    new Map([["h7", { type, data, provenance: {} } as unknown as StoredValue]]);

  it("should_SayUnknownAndKeepDrawing_When_AValueArrivesWithNoType", () => {
    expect(shapeOf(node, held(undefined, { a: 1 }))).toBe("Unknown · 64 B");
    expect(shapeOf(node, held(undefined, [1, 2, 3]))).toBe("Unknown · 3 rows · 64 B");
    expect(shapeOf(node, held({ kind: "wormhole" }, { a: 1 }))).toBe("Unknown · 64 B");
    // A record without the fields array cannot be counted, and says nothing rather than throwing.
    expect(shapeOf(node, held({ kind: "record", name: "R" }, { a: 1 }))).toBe("R · 64 B");
    expect(shapeOf(node, held({ kind: "record", name: "R", fields: [] }, { a: 1 }))).toBe("R · 0 fields · 64 B");
  });

  it("should_StillSayWhatTheNodeSaid_When_NothingHasBeenFetchedBack", () => {
    expect(shapeOf({ ...node, type: "ProcessOutput" }, new Map())).toBe("ProcessOutput · 64 B");
  });
});

describe("connected only", () => {
  const node = (id: string, dependsOn: string[] = []) =>
    ({ id, name: id, command: ":calc {}", dependsOn, state: "ready" as const, kept: true, provenance: {}, cautions: [] });
  const linked = { ...emptyWorkspace, nodes: [node("a"), node("b", ["a"]), node("lone"), node("alone")] };

  it("should_LeaveOutTheNodesNoEdgeTouches_And_CountThem_When_ItIsOn", () => {
    // Arrange / Act
    const view = readGraph(linked, { connectedOnly: true });
    // Assert
    expect(view.nodes.map((it) => it.id).sort()).toEqual(["a", "b"]);
    expect(view.hidden).toBe(2);
  });

  it("should_KeepTheSelectedNode_When_NothingTouchesIt", () => {
    // Arrange / Act
    const view = readGraph(linked, { connectedOnly: true, selected: "lone" });
    // Assert
    expect(view.nodes.map((it) => it.id).sort()).toEqual(["a", "b", "lone"]);
    expect(view.hidden).toBe(1);
  });

  it("should_KeepEveryNodeAndSayNothing_When_ItIsOff", () => {
    // Arrange / Act
    const view = readGraph(linked);
    // Assert
    expect(view.nodes).toHaveLength(4);
    expect(view.hidden).toBeUndefined();
  });
});

describe("creation-lifetime inputs", () => {
  const raw = node({ id: "raw", name: "raw", command: "acme orders.list" });
  const mapped = node({ id: "mapped", name: "mapped", command: "toSeries input:$raw", dependsOn: ["raw"] });
  const chart = (complete: boolean) => node({ id: "chart", name: "chart", command: ":view create Metric", dependsOn: ["mapped"],
    dependencyLifetime: "creation", constructionComplete: complete || undefined, state: complete ? "ready" : "pending" });
  const graph = (complete: boolean): Workspace => ({ ...emptyWorkspace, nodes: [raw, mapped, chart(complete)] });

  it("should_MarkTheInputEdgeAsCreation_When_TheEngineSaysTheConsumerHasACreationLifetime", () => {
    // Arrange
    const nodes = graph(true).nodes;
    // Act
    const edges = edgesOf(nodes);
    // Assert
    expect(edges).toEqual([
      { from: "raw", to: "mapped" },
      { from: "mapped", to: "chart", lifetime: "creation", constructed: true },
    ]);
  });

  it("should_NotCountACompletedViewAsBrokenByARefresh_When_ItsConstructionSucceeded", () => {
    // Arrange
    const [pending, complete] = [edgesOf(graph(false).nodes), edgesOf(graph(true).nodes)];
    // Act
    const [before, after] = [dependentsOf(pending, "raw"), dependentsOf(complete, "raw")];
    // Assert
    expect([...before].sort()).toEqual(["chart", "mapped"]);
    expect([...after]).toEqual(["mapped"]);
  });

  it("should_SayWhatTheCreationInputMeans_When_TheViewIsSelected", () => {
    // Arrange
    const [waiting, built] = [readGraph(graph(false), { selected: "chart" }), readGraph(graph(true), { selected: "chart" })];
    // Act
    const [before, after] = [waiting.selected?.input, built.selected?.input];
    // Assert
    expect(before).toBe("waits for $mapped before it is created");
    expect(after).toBe("created once from $mapped; refreshing it does not create this again");
    expect(readGraph(graph(true), { selected: "mapped" }).selected?.input).toBeUndefined();
    expect(readGraph(graph(true), { selected: "raw" }).selected?.breaks).toBe(1);
  });
});
