import { describe, expect, it } from "vitest";
import { decodeEvent } from "./protocol-decode";
import { apply, constructed, emptyWorkspace, inputsCaptured, type Workspace } from "./workspace";
import { cyclesIn, dependentsOf, edgesOf } from "./surface/graph-model";
import { connectedNodes } from "./surface/component-model";

/*
 * Independently authored synthetic analyses: invented node ids, run identities and commands. They
 * name no real source or recipe.
 */

const RUN = "00000000-0000-4000-8000-0000000000c1";
const created = (over: Record<string, unknown> = {}) => ({
  event: "created", node: "scan1", name: "summary", command: ":synthetic analysis", dependsOn: ["raw"],
  dependencyLifetime: "captured", inputsCaptured: true, interactive: false, run: RUN, ...over,
});
const raw = { event: "created", node: "raw", name: "raw", command: ":synthetic source", dependsOn: [], dependencyLifetime: "continuous", interactive: false };
/** An ordinary consumer of the analysis. */
const report = { event: "created", node: "report", name: "report", command: ":synthetic report", dependsOn: ["scan1"], dependencyLifetime: "continuous", interactive: false };
const run = (...events: unknown[]): Workspace => events.reduce<Workspace>((workspace, event) => apply(workspace, decodeEvent(event)), emptyWorkspace);
const scanOf = (workspace: Workspace) => workspace.nodes.find(node => node.id === "scan1")!;

describe("dependencyLifetime on created", () => {
  it.each([["continuous"], ["creation"], ["captured"]])("accepts %s", (lifetime) => {
    expect(decodeEvent(created({ dependencyLifetime: lifetime, inputsCaptured: lifetime === "captured" }))).toMatchObject({ dependencyLifetime: lifetime });
  });

  it.each([
    ["an unknown spelling", "capture"],
    ["another case", "Captured"],
    ["a null", null],
    ["a boolean", true],
  ])("refuses %s rather than reading it as continuous", (_name, lifetime) => {
    expect(() => decodeEvent(created({ dependencyLifetime: lifetime }))).toThrow();
  });

  it("refuses a created event without one", () => {
    const missing = (({ dependencyLifetime: _d, ...rest }) => rest)(created());
    expect(() => decodeEvent(missing)).toThrow();
  });
});

describe("inputsCaptured on created", () => {
  it("reads an absent flag as not captured", () => {
    const absent = (({ inputsCaptured: _i, ...rest }) => rest)(created());
    expect(inputsCaptured(scanOf(run(raw, absent)))).toBe(false);
  });

  it.each([["text", "true"], ["a number", 1], ["a null", null]])("refuses %s", (_name, flag) => {
    expect(() => decodeEvent(created({ inputsCaptured: flag }))).toThrow();
  });

  it.each([["continuous"], ["creation"]])("refuses true on a %s lifetime and accepts false", (lifetime) => {
    expect(() => decodeEvent(created({ dependencyLifetime: lifetime }))).toThrow();
    expect(decodeEvent(created({ dependencyLifetime: lifetime, inputsCaptured: false }))).toMatchObject({ inputsCaptured: false });
  });

  it("never infers capture from an admitted run: a spawned run that has not entered captured nothing", () => {
    const spawned = run(raw, created({ inputsCaptured: false }), report);
    const scan = scanOf(spawned);
    expect(scan.run).toBe(RUN);
    expect(inputsCaptured(scan)).toBe(false);
    expect([...dependentsOf(edgesOf(spawned.nodes), "raw")].sort()).toEqual(["report", "scan1"]);
  });

  it("follows each announcement: captured after entry, cleared when the engine says so again", () => {
    const entered = run(raw, created({ inputsCaptured: false }), created());
    expect(inputsCaptured(scanOf(entered))).toBe(true);
    expect(inputsCaptured(scanOf(run(raw, created(), created({ inputsCaptured: false }))))).toBe(false);
  });
});

describe("a captured-input analysis before its run is admitted", () => {
  it("has captured nothing yet: its input still reaches it and it stays pending", () => {
    const workspace = run(raw, created({ run: null, inputsCaptured: false }), report);
    const scan = scanOf(workspace);
    expect(scan.state).toBe("pending");
    expect(inputsCaptured(scan)).toBe(false);
    expect(constructed(scan)).toBe(false);
    const edges = edgesOf(workspace.nodes);
    expect(edges).toContainEqual({ from: "raw", to: "scan1", lifetime: "captured", captured: false });
    expect([...dependentsOf(edges, "raw")].sort()).toEqual(["report", "scan1"]);
  });
});

describe("a captured-input analysis after its run is admitted", () => {
  const admitted = () => run(raw, created(), { event: "node", node: "scan1", state: "ready" }, report);

  it("is never a completed construction, even if construction is reported complete", () => {
    const scan = scanOf(run(raw, created(), { event: "node", node: "scan1", state: "ready", constructionComplete: true }));
    expect(scan.dependencyLifetime).toBe("captured");
    expect(inputsCaptured(scan)).toBe(true);
    expect(constructed(scan)).toBe(false);
  });

  it("keeps every structural edge visible while invalidation stops at the analysis", () => {
    const workspace = admitted();
    const edges = edgesOf(workspace.nodes);
    expect(edges).toEqual([
      { from: "raw", to: "scan1", lifetime: "captured", captured: true },
      { from: "scan1", to: "report" },
    ]);
    // A producer update reaches neither the analysis nor anything downstream of it.
    expect([...dependentsOf(edges, "raw")]).toEqual([]);
    // Structure still connects them for ownership, deletion and cycle reasoning.
    expect([...connectedNodes(workspace.nodes, "raw")].sort()).toEqual(["raw", "report", "scan1"]);
    expect(cyclesIn(workspace.nodes, edges)).toEqual([]);
  });

  it("lets an explicit refresh of the analysis reach its ordinary downstream", () => {
    const edges = edgesOf(admitted().nodes);
    expect([...dependentsOf(edges, "scan1")]).toEqual(["report"]);
  });

  it("keeps its captured inputs across restoration as the engine restates them", () => {
    // A reopened workspace announces the node afresh, with the engine's restored flag.
    const restored = run(raw, created({ inputsCaptured: true }), { event: "node", node: "scan1", state: "stale" }, report);
    expect(inputsCaptured(scanOf(restored))).toBe(true);
    expect([...dependentsOf(edgesOf(restored.nodes), "raw")]).toEqual([]);
  });

  it("still sees a cycle through a captured edge", () => {
    const loop = { ...created({ dependsOn: ["raw"] }) };
    const back = { ...raw, dependsOn: ["scan1"] };
    const workspace = run(back, loop);
    expect(cyclesIn(workspace.nodes, edgesOf(workspace.nodes))).toHaveLength(1);
  });
});
