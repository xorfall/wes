import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { ReactNode } from "react";
import type { Engine } from "./engine";
import type { StoredValue } from "./protocol";
import { decodeEvent } from "./protocol-decode";
import { apply, emptyWorkspace, type Workspace, type WorkspaceNode } from "./workspace";
import { newCell } from "./cells";
import { Cell } from "./surface/Cell";
import { cellBlocks } from "./surface/cell-output";
import { readSession } from "./surface/session-model";
import { ComposeContext, type Composer } from "./surface/dataset-management";
import { localReconciliation, LocalReconciliationControls, RECONCILE_NOTE, RECONCILE_WAITING } from "./surface/LocalReconciliationControls";
import { Inspector } from "./surface/Inspector";
import { OpenScreen } from "./surface/screens/Open";

/*
 * Independently authored synthetic runs: invented node ids, runs, commands and receipt values. They
 * name no real source, store or transaction.
 */

const RUN = "00000000-0000-4000-8000-0000000000c1";
const LATER = "00000000-0000-4000-8000-0000000000c2";
const ACTION = "reconcile local write…";
const created = (over: Record<string, unknown> = {}) => ({
  event: "created", node: "id7", name: "analysis", command: ":synthetic", dependsOn: [], dependencyLifetime: "captured",
  interactive: false, run: RUN, reconciliationControl: { command: "scan", available: true }, ...over,
});
const without = ({ reconciliationControl: _c, ...rest }: Record<string, unknown>) => rest;
const state = (value: string) => ({ event: "node", node: "id7", state: value, constructionComplete: false });
const withdrawn = { event: "result-access", node: "id7", readable: false };
const run = (...events: unknown[]): Workspace => events.reduce<Workspace>((workspace, event) => apply(workspace, decodeEvent(event)), emptyWorkspace);
const node = (...events: unknown[]): WorkspaceNode => run(...events).nodes[0]!;
const textOf = (item: ReactTestInstance): string => item.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
const action = (tree: ReactTestRenderer) => tree.root.findAll(item => item.type === "button" && textOf(item) === ACTION);
const composer = (taken: readonly string[] = []) => { const compose = vi.fn(); return { compose, value: { compose, taken: new Set(taken) } satisfies Composer }; };

const trees: ReactTestRenderer[] = [];
afterEach(() => { act(() => trees.splice(0).forEach(tree => tree.unmount())); });
function draw(content: ReactNode, value?: Composer): ReactTestRenderer {
  let tree!: ReactTestRenderer;
  act(() => { tree = create(value ? <ComposeContext.Provider value={value}>{content}</ComposeContext.Provider> : <>{content}</>); });
  trees.push(tree);
  return tree;
}

describe("reconciliationControl on created", () => {
  it.each([
    ["scan, available", { command: "scan", available: true }],
    ["scan, not yet joined", { command: "scan", available: false }],
    ["dataset, available", { command: "dataset", available: true }],
    ["dataset, not yet joined", { command: "dataset", available: false }],
  ])("accepts exactly a command and a boolean: %s", (_name, control) => {
    expect(decodeEvent(created({ reconciliationControl: control }))).toEqual({ ...created(), reconciliationControl: control });
  });

  it("accepts null and an absent field as no control, beside an independent recording control", () => {
    expect(decodeEvent(created({ reconciliationControl: null }))).toMatchObject({ node: "id7", reconciliationControl: null });
    expect(decodeEvent(without(created()))).toEqual(without(created()));
    const recording = { active: true, statusAvailable: true, stopAvailable: true, discardAvailable: false };
    expect(decodeEvent(created({ recordingControl: recording, reconciliationControl: { command: "dataset", available: false } })))
      .toMatchObject({ recordingControl: recording, reconciliationControl: { command: "dataset", available: false } });
  });

  it.each([
    ["a surplus start key", { command: "scan", available: true, start: true }],
    ["a surplus run copy", { command: "scan", available: true, run: RUN }],
    ["a missing available", { command: "scan" }],
    ["a missing command", { available: true }],
    ["an empty object", {}],
    ["a resume command", { command: "resume", available: true }],
    ["a recording command", { command: "recording", available: true }],
    ["an uppercase command", { command: "Scan", available: true }],
    ["a text flag", { command: "scan", available: "true" }],
    ["a numeric flag", { command: "dataset", available: 1 }],
    ["a null flag", { command: "dataset", available: null }],
    ["a null command", { command: null, available: true }],
    ["an array", ["scan", true]],
    ["a bare true", true],
    ["a bare command", "scan"],
  ])("refuses %s", (_name, control) => {
    expect(() => decodeEvent(created({ reconciliationControl: control }))).toThrow();
  });
});

describe("which reconciliation a node offers", () => {
  it("binds the engine's control to the run it was said for", () => {
    expect(node(created()).reconciliationControl).toEqual({ command: "scan", available: true, run: RUN });
    expect(localReconciliation(node(created(), state("ready")))).toEqual({ command: "scan", available: true, run: RUN });
    expect(localReconciliation(node(created({ reconciliationControl: { command: "dataset", available: true } }), state("failed"))))
      .toEqual({ command: "dataset", available: true, run: RUN });
  });

  it("offers nothing for null, an absent field, no run, or a node without an exactly nameable run", () => {
    expect(localReconciliation(node(created({ reconciliationControl: null })))).toBeUndefined();
    expect(localReconciliation(node(without(created()))) ).toBeUndefined();
    expect(localReconciliation(node(created({ run: null })))).toBeUndefined();
    for (const odd of [RUN.toUpperCase(), `{${RUN}}`, RUN.replace(/-/g, ""), ` ${RUN}`, `${RUN}"`, "r1"]) {
      expect(localReconciliation(node(created({ run: odd })))).toBeUndefined();
    }
    expect(localReconciliation(node(created({ node: "id-7" })))).toBeUndefined();
  });

  it("never infers a control from the command text, a copied receipt or result shape without the engine's control", () => {
    const copy = node(created({ command: `:scan reconcile $id3 run:"${RUN}" > receipt`, reconciliationControl: null }));
    expect(localReconciliation(copy)).toBeUndefined();
    const recorded = node(without(created({ command: ":dataset record source:$synthetic from:start > capture" })));
    expect(localReconciliation(recorded)).toBeUndefined();
  });

  it("clears on run replacement, a later announcement without it, and access withdrawal", () => {
    expect(localReconciliation(node(created(), created({ run: LATER, reconciliationControl: null })))).toBeUndefined();
    expect(localReconciliation(node(created(), without(created())))).toBeUndefined();
    expect(localReconciliation(node(created(), withdrawn))).toBeUndefined();
    expect(node(created(), withdrawn).reconciliationControl).toBeUndefined();
    // A run change the control was not said for never inherits it.
    expect(localReconciliation({ ...node(created()), run: LATER })).toBeUndefined();
    // Only the engine's own statement for the new run speaks for it.
    expect(localReconciliation(node(created(), created({ run: LATER, reconciliationControl: { command: "scan", available: false } }))))
      .toEqual({ command: "scan", available: false, run: LATER });
  });

  it("is unavailable while running or waiting and becomes available only when the engine says the run joined", () => {
    expect(localReconciliation(node(created({ reconciliationControl: { command: "scan", available: false } }), state("running")))!.available).toBe(false);
    // The engine has not yet re-announced the joined run: still unavailable.
    expect(localReconciliation(node(created({ reconciliationControl: { command: "scan", available: false } }), state("running"), state("ready")))!.available).toBe(false);
    expect(localReconciliation(node(created({ reconciliationControl: { command: "scan", available: false } }), state("running"), state("ready"), created()))!.available).toBe(true);
    // A state seen running or waiting withholds an earlier availability; it never grants one.
    expect(localReconciliation(node(created(), state("running")))!.available).toBe(false);
    expect(localReconciliation(node(created(), state("pending")))!.available).toBe(false);
  });
});

describe("the shared local reconciliation control", () => {
  it.each([
    ["scan", `:scan reconcile $id7 run:"${RUN}" > receipt2`],
    ["dataset", `:dataset reconcile $id7 run:"${RUN}" > receipt2`],
  ])("prepares the exact %s command by node id, run and a fresh receipt name, and never runs on render", (command, expected) => {
    const { compose, value } = composer(["receipt", "analysis"]);
    const tree = draw(<LocalReconciliationControls node={node(created({ reconciliationControl: { command, available: true } }), state("ready"))} />, value);
    expect(compose).not.toHaveBeenCalled();
    expect(tree.root.findAll(item => item.type === "button").map(textOf)).toEqual([ACTION]);
    expect(textOf(tree.root)).toContain(RECONCILE_NOTE);
    act(() => action(tree)[0]!.props.onClick());
    expect(compose.mock.calls).toEqual([[expected]]);
    expect(compose.mock.calls[0]![0]).not.toContain("$analysis");
  });

  it("offers no source, resume or repair action and promises no outcome", () => {
    const { value } = composer();
    const tree = draw(<LocalReconciliationControls node={node(created(), state("failed"))} />, value);
    const said = textOf(tree.root);
    expect(said).not.toMatch(/\b(start|cancel|repair|fix|restored|success)/i);
    expect(said).toContain("does not rerun the source or resume analysis");
  });

  it("names the run the engine stated after the node moves to a new run, never the earlier one", () => {
    const { compose, value } = composer();
    const tree = draw(<LocalReconciliationControls node={node(created(), created({ run: LATER }), state("ready"))} />, value);
    act(() => action(tree)[0]!.props.onClick());
    expect(compose.mock.calls).toEqual([[`:scan reconcile $id7 run:"${LATER}" > receipt`]]);
    expect(compose.mock.calls[0]![0]).not.toContain(RUN);
  });

  it("shows a disabled action with a short truthful note while the run has not joined, then enables it in place", () => {
    const { compose, value } = composer();
    const pending = { command: "scan", available: false };
    const tree = draw(<LocalReconciliationControls node={node(created({ reconciliationControl: pending }), state("running"))} />, value);
    expect(action(tree)[0]!.props.disabled).toBe(true);
    expect(textOf(tree.root)).toContain(RECONCILE_WAITING);
    expect(textOf(tree.root)).not.toMatch(/\d+\s*(s|sec|%)|…\s*$/);
    act(() => action(tree)[0]!.props.onClick());
    expect(compose).not.toHaveBeenCalled();
    act(() => tree.update(<ComposeContext.Provider value={value}><LocalReconciliationControls node={node(created({ reconciliationControl: pending }), state("running"), state("ready"), created())} /></ComposeContext.Provider>));
    expect(action(tree)[0]!.props.disabled).toBe(false);
    expect(textOf(tree.root)).toContain(RECONCILE_NOTE);
  });

  it("offers no command when no fresh receipt name is free", () => {
    const taken = ["receipt", ...Array.from({ length: 998 }, (_, at) => `receipt${at + 2}`)];
    const { compose, value } = composer(taken);
    const tree = draw(<LocalReconciliationControls node={node(created(), state("ready"))} />, value);
    expect(action(tree)[0]!.props.disabled).toBe(true);
    act(() => action(tree)[0]!.props.onClick());
    expect(compose).not.toHaveBeenCalled();
  });

  it("says the command is prepared in the session, and offers none, where there is no session prompt", () => {
    const tree = draw(<LocalReconciliationControls node={node(created(), state("ready"))} />);
    expect(action(tree)).toHaveLength(0);
    expect(textOf(tree.root)).toContain("prepared in the session");
  });
});

describe("every host of the control", () => {
  const ready: StoredValue = { type: { kind: "primitive", name: "INT" }, data: 17, provenance: {} };
  function session(workspace: Workspace, held: Map<string, StoredValue>, view: "collapsed" | "preview" = "preview") {
    return readSession({ workspace, held, cells: [{ ...newCell(":synthetic"), view, state: "answered", nodes: ["id7"] }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
  }
  function cellTree(workspace: Workspace, held = new Map<string, StoredValue>(), view: "collapsed" | "preview" = "preview") {
    const cell = session(workspace, held, view);
    const blocks = cellBlocks({ cell, workspace, held, reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" });
    const { compose, value } = composer();
    const tree = draw(<Cell theme="keys" state={cell.state} rows={cell.rows} verdict={cell.verdict} blocks={blocks} view={view} />, value);
    return { tree, blocks, compose };
  }

  it("keeps the action in the cell's run zone when the result is collapsed", () => {
    const workspace = run(created(), { event: "ready", node: "id7", type: "Int", handle: "h7", bytes: 2, provenance: {}, cautions: [], kept: false });
    const { tree, compose } = cellTree(workspace, new Map([["h7", ready]]), "collapsed");
    expect(tree.root.findByProps({ "data-node": "id7" }).props["data-view"]).toBe("collapsed");
    const zone = tree.root.findByProps({ "data-zone": "run" });
    expect(zone.findAll(item => item.type === "button" && textOf(item) === ACTION)).toHaveLength(1);
    act(() => action(tree)[0]!.props.onClick());
    expect(compose.mock.calls).toEqual([[`:scan reconcile $id7 run:"${RUN}" > receipt`]]);
  });

  it("keeps the action in the run zone of a failed run with no readable value and no result card", () => {
    const workspace = run(created({ reconciliationControl: { command: "dataset", available: true } }), state("failed"));
    const { tree, compose } = cellTree(workspace);
    expect(tree.root.findAll(item => item.props["data-node"] === "id7")).toHaveLength(0);
    expect(tree.root.findByProps({ "data-zone": "run" }).findAll(item => item.type === "button" && textOf(item) === ACTION)).toHaveLength(1);
    act(() => action(tree)[0]!.props.onClick());
    expect(compose.mock.calls).toEqual([[`:dataset reconcile $id7 run:"${RUN}" > receipt`]]);
  });

  it("adds no row for a node the engine gave no control, even when its value looks like a receipt", () => {
    const receipt: StoredValue = { type: { kind: "record", name: "LocalWriteReceipt", fields: [{ name: "writeOutcome", type: { kind: "primitive", name: "TEXT" } }] }, data: { writeOutcome: "committed" }, provenance: {} };
    const workspace = run(created({ command: `:scan reconcile $id3 run:"${RUN}" > receipt`, reconciliationControl: null }),
      { event: "ready", node: "id7", type: "LocalWriteReceipt", handle: "h7", bytes: 2, provenance: {}, cautions: [], kept: false });
    const { tree, blocks } = cellTree(workspace, new Map([["h7", receipt]]));
    expect(blocks.some(block => block.key === "id7:reconciliation")).toBe(false);
    expect(action(tree)).toHaveLength(0);
  });

  it("offers the same action in /open, including with no value, and withdraws it there on access withdrawal", () => {
    const { compose, value } = composer();
    const joined = node(created(), state("failed"));
    const tree = draw(<OpenScreen top={[]} subject={[]} tab="result" viewing={{ node: joined }} />, value);
    expect(action(tree)).toHaveLength(1);
    act(() => action(tree)[0]!.props.onClick());
    expect(compose.mock.calls).toEqual([[`:scan reconcile $id7 run:"${RUN}" > receipt`]]);
    act(() => tree.update(<ComposeContext.Provider value={value}><OpenScreen top={[]} subject={[]} tab="result" viewing={{ node: node(created(), state("failed"), withdrawn) }} /></ComposeContext.Provider>));
    expect(action(tree)).toHaveLength(0);
  });

  it("offers the same action in the inspector for a run with no value, and only the engine's run", async () => {
    const { compose, value } = composer(["receipt"]);
    const workspace = run(created({ reconciliationControl: { command: "dataset", available: true } }), state("failed"));
    const cell = readSession({ workspace, cells: [{ ...newCell(":synthetic"), id: "c", state: "answered", nodes: ["id7"] }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
    // A run with no handle has nothing to read; any read would be a defect, not a fixture.
    const engine = { fetch: vi.fn(async () => undefined), liveView: vi.fn(async () => undefined) } as unknown as Engine;
    const props = { engine, workspace, generation: "g", cell, selection: { cell: "c", node: "id7", tab: "inspect" as const }, active: true, onTab: vi.fn(), onClose: vi.fn(), onWindow: vi.fn(), overlay: false };
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<ComposeContext.Provider value={value}><Inspector {...props} /></ComposeContext.Provider>); });
    trees.push(tree);
    expect(compose).not.toHaveBeenCalled();
    expect(action(tree)).toHaveLength(1);
    act(() => action(tree)[0]!.props.onClick());
    expect(compose.mock.calls).toEqual([[`:dataset reconcile $id7 run:"${RUN}" > receipt2`]]);
    expect((engine as unknown as { fetch: ReturnType<typeof vi.fn> }).fetch).not.toHaveBeenCalled();
    const replaced = apply(workspace, decodeEvent(created({ run: LATER, reconciliationControl: null })));
    await act(async () => tree.update(<ComposeContext.Provider value={value}><Inspector {...props} workspace={replaced} /></ComposeContext.Provider>));
    expect(action(tree)).toHaveLength(0);
  });
});
