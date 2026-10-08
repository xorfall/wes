import { describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "./engine";
import { decodeEvent } from "./protocol-decode";
import { apply, emptyWorkspace, type Workspace, type WorkspaceNode } from "./workspace";
import { newCell } from "./cells";
import { cellBlocks } from "./surface/cell-output";
import { readSession } from "./surface/session-model";
import { ComposeContext, type Composer } from "./surface/dataset-management";
import { recordingActions, RecordingControls } from "./surface/RecordingControls";
import { OpenScreen } from "./surface/screens/Open";

/*
 * Independently authored synthetic recording runs: invented node ids, runs and commands. They name
 * no real source, writer or store.
 */

const RUN = "00000000-0000-4000-8000-0000000000f1";
const LATER = "00000000-0000-4000-8000-0000000000f2";
const created = (over: Record<string, unknown> = {}) => ({
  event: "created", node: "n7", name: "capture", command: ":synthetic", dependsOn: [], dependencyLifetime: "continuous",
  interactive: false, run: RUN, recordingControl: { active: true, statusAvailable: true, stopAvailable: true, discardAvailable: false }, ...over,
});
const DRAINING = { active: true, statusAvailable: true, stopAvailable: false, discardAvailable: false };
const FINISHED = { active: false, statusAvailable: true, stopAvailable: false, discardAvailable: false };
/** A recording setup prepared before its held source runs, then attaching, then finished. */
const SETUP = { active: true, statusAvailable: true, stopAvailable: false, discardAvailable: true };
const ATTACHING = { active: true, statusAvailable: true, stopAvailable: false, discardAvailable: false };
const SETUP_ENDED = { active: false, statusAvailable: true, stopAvailable: false, discardAvailable: false };
const run = (...events: unknown[]): Workspace => events.reduce<Workspace>((workspace, event) => apply(workspace, decodeEvent(event)), emptyWorkspace);
const node = (...events: unknown[]): WorkspaceNode => run(...events).nodes[0]!;
const textOf = (item: ReactTestInstance): string => item.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
const buttons = (tree: ReactTestRenderer) => tree.root.findAll(item => item.type === "button").map(textOf);
const composer = (taken: readonly string[] = []) => { const compose = vi.fn(); return { compose, value: { compose, taken: new Set(taken) } satisfies Composer }; };

describe("recordingControl on created", () => {
  it.each([
    ["an attached writer with status and stop", { active: true, statusAvailable: true, stopAvailable: true, discardAvailable: false }],
    ["an attached writer draining", DRAINING],
    ["a finished writer with status", FINISHED],
    ["a finished writer with none", { active: false, statusAvailable: false, stopAvailable: false, discardAvailable: false }],
    ["a prepared setup", SETUP],
    ["a setup attaching", ATTACHING],
    ["a setup that ended", SETUP_ENDED],
  ])("accepts exactly four booleans: %s", (_name, control) => {
    expect(decodeEvent(created({ recordingControl: control }))).toEqual({ ...created(), recordingControl: control });
  });

  it("accepts null and an absent field as no controls, and keeps the rest of created as it was", () => {
    expect(decodeEvent(created({ recordingControl: null }))).toMatchObject({ node: "n7", command: ":synthetic", recordingControl: null });
    const plain = (({ recordingControl: _c, ...rest }) => rest)(created());
    expect(decodeEvent(plain)).toEqual(plain);
  });

  it.each([
    ["a surplus start key", { active: true, statusAvailable: true, stopAvailable: true, discardAvailable: false, startAvailable: true }],
    ["the three-key shape without discard", { active: true, statusAvailable: true, stopAvailable: true }],
    ["a missing key", { active: true, statusAvailable: true, discardAvailable: false }],
    ["a text flag", { active: true, statusAvailable: "true", stopAvailable: true, discardAvailable: false }],
    ["a text discard flag", { active: true, statusAvailable: true, stopAvailable: false, discardAvailable: "true" }],
    ["a numeric flag", { active: 1, statusAvailable: 1, stopAvailable: 0, discardAvailable: 0 }],
    ["a null active", { active: null, statusAvailable: true, stopAvailable: false, discardAvailable: false }],
    ["a null discard", { active: true, statusAvailable: true, stopAvailable: false, discardAvailable: null }],
    ["stop offered for a finished writer", { active: false, statusAvailable: true, stopAvailable: true, discardAvailable: false }],
    ["stop alone offered for a finished writer", { active: false, statusAvailable: false, stopAvailable: true, discardAvailable: false }],
    ["discard offered for a finished setup", { active: false, statusAvailable: true, stopAvailable: false, discardAvailable: true }],
    ["both stop and discard", { active: true, statusAvailable: true, stopAvailable: true, discardAvailable: true }],
    ["an array", [true, true]],
    ["a bare true", true],
  ])("refuses %s", (_name, control) => {
    expect(() => decodeEvent(created({ recordingControl: control }))).toThrow();
  });
});

describe("which controls a node offers", () => {
  it("offers both while the exact run records, only stop while it is prepared, only status while draining or once finished", () => {
    expect(recordingActions(node(created()))).toEqual({ status: true, stop: true, discard: false, run: RUN });
    expect(recordingActions(node(created({ recordingControl: { active: true, statusAvailable: false, stopAvailable: true, discardAvailable: false } })))).toEqual({ status: false, stop: true, discard: false, run: RUN });
    expect(recordingActions(node(created({ recordingControl: DRAINING })))).toEqual({ status: true, stop: false, discard: false, run: RUN });
    expect(recordingActions(node(created({ recordingControl: FINISHED })))).toEqual({ status: true, stop: false, discard: false, run: RUN });
  });

  it("offers discard only for a prepared setup, and only status while it attaches or once it ended", () => {
    expect(recordingActions(node(created({ recordingControl: SETUP })))).toEqual({ status: true, stop: false, discard: true, run: RUN });
    expect(recordingActions(node(created({ recordingControl: ATTACHING })))).toEqual({ status: true, stop: false, discard: false, run: RUN });
    expect(recordingActions(node(created({ recordingControl: SETUP_ENDED })))).toEqual({ status: true, stop: false, discard: false, run: RUN });
    // The engine's later announcement replaces the earlier one: discard disappears once attaching.
    expect(recordingActions(node(created({ recordingControl: SETUP }), created({ recordingControl: ATTACHING })))!.discard).toBe(false);
    // A setup's control is bound to its run and cleared on withdrawal or another run, like a writer's.
    expect(recordingActions(node(created({ recordingControl: SETUP }), { event: "result-access", node: "n7", readable: false }))).toBeUndefined();
    expect(recordingActions({ ...node(created({ recordingControl: SETUP })), run: LATER })).toBeUndefined();
  });

  it("never infers a setup from its command or a copied value without the engine's control", () => {
    const copied = node(created({ command: ":dataset record source:$synthetic from:start > capture", recordingControl: null }));
    expect(recordingActions(copied)).toBeUndefined();
  });

  it.each([
    ["an uppercase run", RUN.toUpperCase()],
    ["a braced run", `{${RUN}}`],
    ["an unhyphenated run", RUN.replace(/-/g, "")],
    ["a run with surrounding space", ` ${RUN}`],
    ["a run with a quote", `${RUN}"`],
    ["a run that is not a UUID", "r1"],
  ])("offers nothing for %s, which no command could name exactly", (_name, run) => {
    expect(recordingActions(node(created({ run })))).toBeUndefined();
  });

  it("keeps the engine's four flags bound to the run they were said for, and clears them all on withdrawal", () => {
    expect(node(created({ recordingControl: DRAINING })).recordingControl).toEqual({ ...DRAINING, run: RUN });
    expect(node(created(), { event: "result-access", node: "n7", readable: false }).recordingControl).toBeUndefined();
  });

  it("offers nothing for copies, restored work or a node the engine gave no control", () => {
    expect(recordingActions(node(created({ recordingControl: null })))).toBeUndefined();
    expect(recordingActions(node((({ recordingControl: _c, ...rest }) => rest)(created())))).toBeUndefined();
    expect(recordingActions(node(created({ run: null })))).toBeUndefined();
  });

  it("drops controls on a new run, a later announcement without them, and a withdrawal", () => {
    expect(recordingActions(node(created(), created({ run: LATER, recordingControl: null })))).toBeUndefined();
    expect(recordingActions(node(created(), (({ recordingControl: _c, ...rest }) => rest)(created())))).toBeUndefined();
    expect(recordingActions(node(created(), { event: "result-access", node: "n7", readable: false }))).toBeUndefined();
    // A control said for one run never speaks for another, even if the node's run changes otherwise.
    expect(recordingActions({ ...node(created()), run: LATER })).toBeUndefined();
    expect(recordingActions({ ...node(created()), run: undefined })).toBeUndefined();
  });

  it("follows a later run only when the engine states a control for it, and then names that run", () => {
    expect(recordingActions(node(created(), created({ run: LATER })))).toEqual({ status: true, stop: true, discard: false, run: LATER });
  });
});

describe("the shared recording controls", () => {
  it("prepares the exact reviewed command by node id and current run, and never runs anything on render", () => {
    const { compose, value } = composer(["status", "capture"]);
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><RecordingControls node={node(created())} /></ComposeContext.Provider>); });
    expect(compose).not.toHaveBeenCalled();
    expect(buttons(tree)).toEqual(["recording status…", "stop recording…"]);
    expect(textOf(tree.root)).toContain("its source keeps running");
    act(() => tree.root.findAll(item => item.type === "button")[0]!.props.onClick());
    act(() => tree.root.findAll(item => item.type === "button")[1]!.props.onClick());
    expect(compose.mock.calls).toEqual([
      [`:dataset recording-status $n7 run:"${RUN}" > status2`],
      [`:dataset stop $n7 run:"${RUN}" > stopped`],
    ]);
    for (const [command] of compose.mock.calls) expect(command).not.toMatch(/stop-recording|\$capture\b/);
    act(() => tree.unmount());
  });

  it("names the run the engine stated, never an earlier one, after the node moves to a new run", () => {
    const { compose, value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><RecordingControls node={node(created(), created({ run: LATER }))} /></ComposeContext.Provider>); });
    act(() => tree.root.findAll(item => item.type === "button")[1]!.props.onClick());
    expect(compose.mock.calls).toEqual([[`:dataset stop $n7 run:"${LATER}" > stopped`]]);
    expect(compose.mock.calls[0]![0]).not.toContain(RUN);
    act(() => tree.unmount());
  });

  it("withholds the controls entirely when the run cannot be named exactly", () => {
    const { compose, value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><RecordingControls node={node(created({ run: RUN.toUpperCase() }))} /></ComposeContext.Provider>); });
    expect(tree.toJSON()).toBeNull();
    expect(compose).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("offers no command when no fresh result name is free", () => {
    const taken = ["status", "stopped", ...Array.from({ length: 998 }, (_, at) => [`status${at + 2}`, `stopped${at + 2}`]).flat()];
    const { compose, value } = composer(taken);
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><RecordingControls node={node(created())} /></ComposeContext.Provider>); });
    for (const button of tree.root.findAll(item => item.type === "button")) {
      expect(button.props.disabled).toBe(true);
      act(() => button.props.onClick());
    }
    expect(compose).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("prepares discard of a recording setup for review, explains it, and offers no stop or source command", () => {
    const { compose, value } = composer(["discarded"]);
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><RecordingControls node={node(created({ recordingControl: SETUP }))} /></ComposeContext.Provider>); });
    expect(compose).not.toHaveBeenCalled();
    expect(buttons(tree)).toEqual(["recording status…", "discard recording setup…"]);
    const said = textOf(tree.root);
    expect(said).toContain("removes this unused recording setup; it neither starts nor cancels the source");
    expect(said).toContain("Once attached, a recording is ended with Stop.");
    expect(said).not.toMatch(/start the source|cancel the source|then record/i);
    act(() => tree.root.findAll(item => item.type === "button")[1]!.props.onClick());
    expect(compose.mock.calls).toEqual([[`:dataset discard $n7 run:"${RUN}" > discarded2`]]);
    // Attaching: discard is gone and nothing replaces it with a source action.
    act(() => tree.update(<ComposeContext.Provider value={value}><RecordingControls node={node(created({ recordingControl: ATTACHING }))} /></ComposeContext.Provider>));
    expect(buttons(tree)).toEqual(["recording status…"]);
    act(() => tree.unmount());
  });

  it("shows only the actions the engine flagged", () => {
    const { value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><RecordingControls node={node(created({ recordingControl: DRAINING }))} /></ComposeContext.Provider>); });
    expect(buttons(tree)).toEqual(["recording status…"]);
    act(() => tree.unmount());
  });

  it("says commands are prepared in the session, and offers none, where there is no session prompt", () => {
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<RecordingControls node={node(created())} />); });
    expect(buttons(tree)).toEqual([]);
    expect(textOf(tree.root)).toContain("prepared in the session");
    act(() => tree.unmount());
  });

  it("appears in the cell's run zone and in /open, for the same node, with the same commands", () => {
    const workspace = run(created(), { event: "node", node: "n7", state: "running" });
    const held = new Map();
    const cell = readSession({ workspace, held, cells: [{ ...newCell(":synthetic"), state: "answered", nodes: ["n7"] }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
    const blocks = cellBlocks({ cell, workspace, held, reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" });
    const block = blocks.find(item => item.key === "n7:recording-controls");
    expect(block?.zone).toBe("run");
    const { compose, value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}>{block!.content}</ComposeContext.Provider>); });
    expect(buttons(tree)).toEqual(["recording status…", "stop recording…"]);
    act(() => tree.update(<ComposeContext.Provider value={value}><OpenScreen top={[]} subject={[]} tab="result" viewing={{ node: workspace.nodes[0]! }} /></ComposeContext.Provider>));
    expect(buttons(tree)).toEqual(expect.arrayContaining(["recording status…", "stop recording…"]));
    expect(compose).not.toHaveBeenCalled();
    act(() => tree.unmount());
    const none = cellBlocks({ cell, workspace: run(created({ recordingControl: null })), held, reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" });
    expect(none.some(item => item.key === "n7:recording-controls")).toBe(false);
  });

  it("offers the same setup discard in the cell's run zone and in /open", () => {
    const workspace = run(created({ recordingControl: SETUP }), { event: "node", node: "n7", state: "pending" });
    const held = new Map();
    const cell = readSession({ workspace, held, cells: [{ ...newCell(":synthetic"), state: "answered", nodes: ["n7"] }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
    const block = cellBlocks({ cell, workspace, held, reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" }).find(item => item.key === "n7:recording-controls");
    const { compose, value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}>{block!.content}</ComposeContext.Provider>); });
    expect(buttons(tree)).toEqual(["recording status…", "discard recording setup…"]);
    act(() => tree.update(<ComposeContext.Provider value={value}><OpenScreen top={[]} subject={[]} tab="result" viewing={{ node: workspace.nodes[0]! }} /></ComposeContext.Provider>));
    expect(buttons(tree)).toEqual(expect.arrayContaining(["recording status…", "discard recording setup…"]));
    expect(compose).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });
});
