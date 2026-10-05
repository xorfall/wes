import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "../engine";
import type { StoredValue, WorkHistory, WorkRun, WorkRunDetail } from "../protocol";
import { emptyWorkspace } from "../workspace";
import { ResultReadError } from "../result-reader";
import { RunHistory, type RunHistoryTarget } from "./RunHistory";
import { Session } from "./Session";
import { readSession } from "./session-model";
import { newCell } from "../cells";

vi.mock("../views/Result", () => ({ ValueView: ({ value }: { value: StoredValue }) => <pre data-result>{JSON.stringify(value.data)}</pre> }));
const node = { id: "node-a", label: "$orders", glyph: "ready" as const };
const old: WorkRun = { node: node.id, run: "old-run", state: "Ready", at: "2026-09-26T09:00:00Z", handle: "old-result", protected: false, trace: false };
const recent: WorkRun = { ...old, run: "new-run", at: "2026-09-28T09:00:00Z", handle: "new-result" };
const history: WorkHistory = { runs: [old, recent], attempts: [{ id: "cell", source: "catalog echo value:synthetic", nodes: [node.id], revisionOf: null, repeatOf: null, failure: null, diagnostics: [] }], unconfirmedWrites: "0" };
const detail = (run: WorkRun): WorkRunDetail => ({ run, canProtect: true, definition: { text: "catalog echo value:original" }, error: null, trace: null, contextNote: "Captured context only.", traceNote: "No recorded trace for this run." });
const value = (text: string): StoredValue => ({ type: { kind: "primitive", name: "String" }, data: text, provenance: {} });
function fixture() {
  const api = { workHistory: vi.fn().mockResolvedValue(history), workRun: vi.fn((_cell: string, id: string) => Promise.resolve(detail(id === old.run ? old : recent))),
    fetch: vi.fn((handle: string) => Promise.resolve(value(handle))), protectRun: vi.fn().mockResolvedValue(detail({ ...old, protected: true })),
    runProtection: vi.fn(), onRunProtection: vi.fn(() => () => {}), submit: vi.fn() };
  const props: RunHistoryTarget & { onClose: () => void } = { engine: api as unknown as Engine, workspace: emptyWorkspace, generation: "g", cell: "cell", nodes: [node], onClose: vi.fn() };
  return { api, props };
}
const trees: ReactTestRenderer[] = [];
afterEach(() => { act(() => trees.splice(0).forEach(tree => tree.unmount())); });
async function mount(props: RunHistoryTarget & { onClose: () => void }) {
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(<RunHistory {...props} />); }); trees.push(tree); return tree;
}
const text = (tree: ReactTestRenderer) => JSON.stringify(tree.toJSON());
const runButtons = (tree: ReactTestRenderer) => tree.root.findAllByType("button").filter(button => String(button.props.className).split(" ").includes("history-run"));
async function select(tree: ReactTestRenderer, id: string) {
  const button = runButtons(tree).find(button => JSON.stringify(button.children.map(child => typeof child === "object" ? child.props : child)).includes(id));
  // Run identity is the accessible description of a span in the button, independent of visible formatting.
  const exact = runButtons(tree).find(button => button.findAllByProps({ "aria-description": id }).length > 0) ?? button!;
  await act(async () => { exact.props.onClick(); });
}
const button = (tree: ReactTestRenderer, name: string) => tree.root.findAllByType("button").find(button => button.children.join("") === name)!;

describe("fixed run inspection", () => {
  it("reads the selected historical handle and preserves it when a new run arrives", async () => {
    const { api, props } = fixture(); const tree = await mount(props);
    await select(tree, old.run);
    expect(api.workRun).toHaveBeenCalledWith("cell", old.run);
    expect(api.fetch).toHaveBeenCalledWith(old.handle);
    expect(api.fetch).not.toHaveBeenCalledWith(recent.handle);
    const newest = { ...recent, run: "newest-run", at: "2026-09-28T10:00:00Z", handle: "newest-result" };
    api.workHistory.mockResolvedValue({ ...history, runs: [newest, ...history.runs] });
    await act(async () => button(tree, "refresh history").props.onClick());
    expect(text(tree)).toContain("New runs recorded");
    expect(tree.root.findByProps({ "data-result": true }).children).toEqual(['"old-result"']);
    expect(api.submit).not.toHaveBeenCalled();
    expect(text(tree)).toContain("No recorded trace");
  });

  it("discards a late detail reply from a previous selection", async () => {
    const { api, props } = fixture();
    let resolve!: (value: WorkRunDetail) => void;
    api.workRun.mockImplementation((_cell, id) => id === old.run ? new Promise(done => { resolve = done; }) : Promise.resolve(detail(recent)));
    const tree = await mount(props); await select(tree, old.run); await select(tree, recent.run);
    await act(async () => resolve(detail(old)));
    expect(tree.root.findByProps({ "data-result": true }).children).toEqual(['"new-result"']);
    expect(api.fetch).not.toHaveBeenCalledWith(old.handle);
  });

  it("does not silently select one node in a pipeline and exposes submissions without runs", async () => {
    const { api, props } = fixture(); const other = { ...node, id: "node-b", label: "$other" };
    const sibling = { ...recent, node: other.id, run: "sibling-run" };
    api.workHistory.mockResolvedValue({ ...history, runs: [...history.runs, sibling], attempts: [...history.attempts,
      { id: "bad-attempt", source: "synthetic invalid", nodes: [], revisionOf: "cell", repeatOf: null, failure: "No node created", diagnostics: ["invalid field"] }] });
    const tree = await mount({ ...props, nodes: [node, other] });
    expect(runButtons(tree)).toHaveLength(0);
    expect(text(tree)).toContain("Choose the node");
    await act(async () => button(tree, "$orders").props.onClick());
    expect(runButtons(tree)).toHaveLength(2);
    const related = tree.root.findAllByType("button").find(button => button.children.join("").startsWith("related work"))!;
    await act(async () => related.props.onClick());
    expect(runButtons(tree)).toHaveLength(3);
    expect(text(tree)).toContain("Submission without runs");
    expect(text(tree)).toContain("No node created");
  });

  it.each([false, true])("separates missing result evidence from a transport failure (missing=%s)", async missing => {
    const { api, props } = fixture();
    api.fetch.mockRejectedValue(missing ? new ResultReadError(404, { code: "VALUE_UNAVAILABLE", message: "Not available", retryable: false, context: { operation: "read-value", handle: old.handle } }) : new Error("Timed out; not evidence of deletion"));
    const tree = await mount(props); await select(tree, old.run);
    expect(text(tree)).toContain(missing ? "Result unavailable" : "Could not read result");
    expect(tree.root.findAllByProps({ "data-result": true })).toHaveLength(0);
    expect(button(tree, "protect result")).toBeUndefined();
    api.fetch.mockResolvedValue(value("recovered historical result"));
    await act(async () => button(tree, "read again").props.onClick());
    expect(api.fetch.mock.calls.every(([handle]) => handle === old.handle)).toBe(true);
    expect(api.submit).not.toHaveBeenCalled();
  });

  it("rejects a changed result identity and never reads the replacement", async () => {
    const { api, props } = fixture(); api.workRun.mockResolvedValue(detail({ ...old, handle: "replacement" }));
    const tree = await mount(props); await select(tree, old.run);
    expect(text(tree)).toContain("recorded result reference changed"); expect(api.fetch).not.toHaveBeenCalled();
  });

  it("shows exact errors, retains an uncertain protection status, and only checks on request", async () => {
    const { api, props } = fixture();
    api.workRun.mockResolvedValue({ ...detail(old), error: { code: "SYN001", message: "Recorded failure", issues: [] } });
    const tree = await mount(props); await select(tree, old.run);
    expect(text(tree)).toContain("Recorded failure");
    api.runProtection.mockReturnValue("uncertain");
    await act(async () => tree.update(<RunHistory {...props} />));
    expect(button(tree, "protect result")).toBeUndefined();
    await act(async () => button(tree, "check protection").props.onClick());
    expect(text(tree)).toContain("Protection is unconfirmed");
    expect(api.protectRun).not.toHaveBeenCalled();
  });

  it("protects exactly the selected run and shows only the acknowledged result", async () => {
    const { api, props } = fixture(); const tree = await mount(props); await select(tree, old.run);
    await act(async () => button(tree, "protect result").props.onClick());
    expect(api.protectRun).toHaveBeenCalledWith("cell", old.run);
    expect(text(tree)).toContain("Protected");
    expect(api.submit).not.toHaveBeenCalled();
  });

  it("respects backend protection eligibility and shows recording gaps", async () => {
    const { api, props } = fixture(); api.workRun.mockResolvedValue({ ...detail(old), canProtect: false });
    api.workHistory.mockResolvedValue({ ...history, unconfirmedWrites: "2" });
    const tree = await mount(props); await select(tree, old.run);
    expect(button(tree, "protect result")).toBeUndefined();
    expect(text(tree)).toContain("history may be incomplete");
  });

  it("uses Escape to return to the list before closing and consumes cell shortcuts", async () => {
    const { props } = fixture(); const tree = await mount(props); await select(tree, old.run);
    const event = { key: "Escape", preventDefault: vi.fn(), stopPropagation: vi.fn() };
    await act(async () => tree.root.findByProps({ "aria-label": "Run history" }).props.onKeyDown(event));
    expect(props.onClose).not.toHaveBeenCalled(); expect(event.stopPropagation).toHaveBeenCalled();
    expect(tree.root.findByProps({ "aria-label": "Run history" }).props.className).not.toContain("history-show-detail");
    await act(async () => tree.root.findByProps({ "aria-label": "Run history" }).props.onKeyDown(event));
    expect(props.onClose).toHaveBeenCalledOnce();
  });

  it("offers History for rejected attempts and closes immediately when the session changes", async () => {
    const { props } = fixture(); let tree!: ReactTestRenderer;
    const workspace = { ...emptyWorkspace, attemptFailures: { cell: "SYN001: synthetic rejection" } };
    const model=readSession({workspace,cells:[{...newCell("synthetic"),id:"cell",lastRun:"cell",state:"answered"}],context:{workspace:"synthetic",connection:"connected"}});
    const draw = (generation: string) => <Session chrome="controls" model={model} prompt={null} history={{engine:props.engine,workspace,generation}}/>;
    await act(async () => { tree = create(draw("g")); }); trees.push(tree);
    await act(async () => tree.root.findByProps({ "aria-keyshortcuts": "h" }).props.onClick());
    expect(tree.root.findAllByType(RunHistory)).toHaveLength(1);
    await act(async () => tree.update(draw("another")));
    expect(tree.root.findAllByType(RunHistory)).toHaveLength(0);
  });

  it("does not offer History to an empty unrun cell merely because the workspace has a history context", async () => {
    const { props, api } = fixture(); let tree!: ReactTestRenderer;
    const injected = vi.fn();
    const model=readSession({workspace:emptyWorkspace,cells:[{...newCell("synthetic"),id:"cell",state:"answered"}],context:{workspace:"synthetic",connection:"connected"}});
    await act(async () => { tree = create(<Session chrome="controls" model={model} prompt={null} actions={() => ({ history: injected })}
      history={{engine:props.engine,workspace:emptyWorkspace,generation:"g"}}/>); }); trees.push(tree);
    expect(tree.root.findAllByProps({ "aria-keyshortcuts": "h" })).toHaveLength(0);
    expect(api.workHistory).not.toHaveBeenCalled();
  });

  it.each([
    ["an admitted pending node", { nodes: [{ id: node.id, command: "synthetic", dependsOn: [], state: "pending" as const, provenance: {}, cautions: [], kept: false }] }, [node.id]],
    ["a recorded diagnostic of the cell", { history: [{ event: "log-diagnostic" as const, durable: true, persistenceProblem: "",
      record: { id: "d1", at: "2026-09-28T09:00:00Z", cell: "cell", source: "synthetic", diagnostic: { code: "SYN002", message: "synthetic", severity: "error" as const, start: 0, end: 0, hints: [] } } }] }, []],
  ])("offers History for %s", async (_name, over, nodes) => {
    const { props } = fixture(); let tree!: ReactTestRenderer;
    const workspace: typeof emptyWorkspace = { ...emptyWorkspace, ...over };
    const model=readSession({workspace,cells:[{...newCell("synthetic"),id:"cell",state:"answered",nodes}],context:{workspace:"synthetic",connection:"connected"}});
    await act(async () => { tree = create(<Session chrome="controls" model={model} prompt={null} history={{engine:props.engine,workspace,generation:"g"}}/>); }); trees.push(tree);
    expect(tree.root.findAllByProps({ "aria-keyshortcuts": "h" })).toHaveLength(1);
  });

  it("keeps a provided History callback when the cell has evidence and no history context", async () => {
    const injected = vi.fn(); let tree!: ReactTestRenderer;
    const workspace = { ...emptyWorkspace, attemptFailures: { cell: "SYN001: synthetic rejection" } };
    const model=readSession({workspace,cells:[{...newCell("synthetic"),id:"cell",lastRun:"cell",state:"answered"}],context:{workspace:"synthetic",connection:"connected"}});
    await act(async () => { tree = create(<Session chrome="controls" model={model} prompt={null} actions={() => ({ history: injected })}/>); }); trees.push(tree);
    await act(async () => tree.root.findByProps({ "aria-keyshortcuts": "h" }).props.onClick());
    expect(injected).toHaveBeenCalledOnce();
  });
});
