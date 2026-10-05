import { terminalPane } from "./split-model";
import { expect, it, vi } from "vitest";
import { act, create } from "react-test-renderer";
import { useEffect, useState } from "react";
import { oneP, SESSION_PANE, splitPane } from "./split-model";
import { openWorkspaceTab, selectWorkspaceTab, closeWorkspaceTab, workspaceViews } from "./workspace-tabs";
import { restoreSplit } from "./split-storage";
import { read } from "./commands";
import { Split } from "./Split";

it("opens without focus theft, reuses a workspace tab and preserves other panes", () => {
  const initial = splitPane(oneP(SESSION_PANE), "right", terminalPane("p2"), true);
  expect(selectWorkspaceTab(initial, "p1", undefined).focused).toBe("p1");
  const opened = openWorkspaceTab(initial, "p1", "shared");
  expect(opened.focused).toBe("p2");
  expect(opened.panes[1]).toBe(initial.panes[1]);
  expect(opened.panes[0]!.workspace).toBeUndefined();
  expect(openWorkspaceTab(opened, "p1", "shared").panes[0]!.tabs).toHaveLength(2);
  expect(openWorkspaceTab(opened, "p1", "shared", true)).toMatchObject({ focused: "p1", panes: [{ workspace: "shared" }, {}] });
  expect(workspaceViews(opened).map(p => p.workspace)).toEqual([undefined, "shared", undefined]);
});

it("closes only a view and retains the final command tab", () => {
  const initial = oneP(SESSION_PANE);
  const opened = openWorkspaceTab(initial, "p1", "shared", true);
  const closed = closeWorkspaceTab(opened, "p1", "shared");
  expect(closed.panes[0]).toMatchObject({ id: "p1", tabs: [{}] });
  expect(closed.panes[0]!.workspace).toBeUndefined();
  expect(closeWorkspaceTab(closed, "p1", undefined)).toBe(closed);
  expect(selectWorkspaceTab(opened, "p1", "missing")).toBe(opened);
});

it("restores validated tabs and old layouts while rejecting forged tab descriptors", () => {
  const old = oneP(SESSION_PANE), state = openWorkspaceTab(old, "p1", "shared", true);
  expect(restoreSplit(JSON.parse(JSON.stringify(state)))).toEqual(state);
  expect(restoreSplit(old)).toEqual(old);
  for (const tabs of [[], [{ workspace: "../escape" }], [{ workspace: "shared" }, { workspace: "shared" }], Array.from({ length: 17 }, (_, n) => ({ workspace: `w${n}` }))]) {
    expect(restoreSplit({ ...state, panes: [{ ...state.panes[0], tabs }] })).toEqual(old);
  }
  expect(() => openWorkspaceTab(state, "missing", "new")).toThrow("session pane");
  expect(() => openWorkspaceTab(oneP(terminalPane("p1")), "p1", "new")).toThrow("session pane");
  let full = old;
  for (let i = 0; i < 15; i++) full = openWorkspaceTab(full, "p1", `w${i}`);
  expect(() => openWorkspaceTab(full, "p1", "excess")).toThrow("workspace tab budget");
});

it("parses explicit tab targets without changing pane command semantics", () => {
  expect(read('/tab "shared work" pane:p1')).toEqual({ kind: "workspace-tab", workspace: "shared work", pane: "p1", activate: false });
  expect(read('/tabx shared')).toEqual({ kind: "workspace-tab", workspace: "shared", activate: true });
  for (const text of ['/tab', '/tab ../bad', '/tab shared pane:missing', '/tab shared extra arguments']) expect(read(text).kind).toBe("trouble");
});

it("retains typed input and the mounted terminal while tabs are added and selected", () => {
  const mounted = vi.fn(), disposed = vi.fn();
  function Content({ name }: { name: string }) {
    const [draft, setDraft] = useState("");
    useEffect(() => { mounted(name); return () => disposed(name); }, []);
    return <input aria-label={name} value={draft} onChange={e => setDraft(e.target.value)} />;
  }
  let state = splitPane(oneP(SESSION_PANE), "right", terminalPane("p2"));
  const draw = () => <Split state={state} top={[]} prompt={[]} context={[]} content={p => <Content name={p.terminal ? "terminal" : p.workspace ?? "original"} />} />;
  let tree!: ReturnType<typeof create>;
  act(() => { tree = create(draw()); });
  act(() => tree.root.findByProps({ "aria-label": "original" }).props.onChange({ target: { value: "user draft" } }));
  state = openWorkspaceTab(state, "p1", "shared", true);
  act(() => tree.update(draw()));
  state = selectWorkspaceTab(state, "p1", undefined);
  act(() => tree.update(draw()));
  expect(tree.root.findByProps({ "aria-label": "original" }).props.value).toBe("user draft");
  expect(mounted.mock.calls.map(([name]) => name)).toEqual(["original", "terminal", "shared"]);
  expect(disposed).not.toHaveBeenCalled();
  act(() => tree.unmount());
});

import { openValueTab, selectViewTab, closeViewTab, updateValueTab, activeView, viewKey } from "./workspace-tabs";
import { close } from "./split-model";

it("retains session drafts and independent value tabs when navigating and closing mixed views", () => {
  const value = { node: "node-one", generation: "g1", label: "orders" };
  let state = openValueTab(oneP(SESSION_PANE), "p1", undefined, value);
  const key = viewKey({ value });
  expect(activeView(state.panes[0]!)).toBe("origin");
  state = selectViewTab(state, "p1", key);
  state = updateValueTab(state, "p1", key, "json");
  expect(state.panes[0]!.value?.tab).toBe("json");
  state = openValueTab(state, "p1", undefined, { ...value, related: true }, true);
  expect(state.panes[0]!.tabs).toHaveLength(3);
  expect(state.panes[0]!.value?.tab).toBeUndefined();
  state = selectViewTab(state, "p1", key);
  expect(state.panes[0]!.value?.tab).toBe("json");
  state = openValueTab(state, "p1", undefined, value, true);
  expect(state.panes[0]!.tabs).toHaveLength(3);
  expect(restoreSplit(JSON.parse(JSON.stringify(state)))).toEqual(state);
  state = closeViewTab(state, "p1", key);
  expect(state.panes[0]!.value?.related).toBe(true);
  state = closeViewTab(state, "p1", activeView(state.panes[0]!));
  expect(state.panes[0]!.value).toBeUndefined();
  expect(state.panes[0]!.tabs).toHaveLength(1);
});

it("keeps node, generation and workspace identities distinct and protects the final session", () => {
  const value = { node: "same", generation: "g1", label: "orders" };
  let state = openValueTab(oneP(SESSION_PANE), "p1", undefined, value);
  state = openValueTab(state, "p1", undefined, { ...value, generation: "g2" });
  state = openValueTab(state, "p1", "other", value);
  expect(state.panes[0]!.tabs).toHaveLength(4);
  state = selectViewTab(state, "p1", viewKey({ value }));
  state = splitPane(state, "right", { id: "p2", title: "$orders", value });
  expect(close(state, "p1")).toBe(state);
  expect(closeViewTab(state, "p2", viewKey({ value })).panes).toHaveLength(1);
  for (const broken of [{ node: "same", label: "orders" }, { ...value, generation: "" }, { ...value, related: false }, { ...value, tab: "\n" }]) {
    expect(restoreSplit({ ...state, panes: [{ ...state.panes[0], value: broken }, state.panes[1]] })).toEqual(oneP(SESSION_PANE));
  }
});

it("retains the mounted session while value tabs are added and selected", () => {
  const mounted = vi.fn(), disposed = vi.fn();
  function Content({ name }: { name: string }) {
    const [draft, setDraft] = useState("");
    useEffect(() => { mounted(name); return () => disposed(name); }, []);
    return <input aria-label={name} value={draft} onChange={event => setDraft(event.target.value)} />;
  }
  const value = { node: "one", generation: "g1", label: "orders" };
  let state = oneP(SESSION_PANE);
  const draw = () => <Split state={state} top={[]} prompt={[]} context={[]} content={pane => <Content name={pane.value ? "value" : "session"} />} />;
  let tree!: ReturnType<typeof create>;
  act(() => { tree = create(draw()); });
  act(() => tree.root.findAllByType("input").find(input => input.props["aria-label"] === "session")!.props.onChange({ target: { value: "unfinished source" } }));
  state = openValueTab(state, "p1", undefined, value, true);
  act(() => tree.update(draw()));
  state = selectWorkspaceTab(state, "p1");
  act(() => tree.update(draw()));
  expect(tree.root.findAllByType("input").find(input => input.props["aria-label"] === "session")!.props.value).toBe("unfinished source");
  expect(mounted.mock.calls.map(([name]) => name)).toEqual(["session", "value"]);
  expect(disposed).not.toHaveBeenCalled();
  act(() => tree.unmount());
});

it("retains the final session tab when value tabs surround it", () => {
  const value = { node: "one", generation: "g1", label: "orders" };
  const state = openValueTab(oneP(SESSION_PANE), "p1", undefined, value, true);
  expect(closeWorkspaceTab(state, "p1")).toBe(state);
});
