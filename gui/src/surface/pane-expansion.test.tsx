import { terminalPane } from "./split-model";
import { afterEach, expect, it, vi } from "vitest";
import { useEffect, useState } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { readFileSync } from "node:fs";
import { Split, type SplitProps } from "./Split";
import { close, focus, geometry, nextPane, oneP, SESSION_PANE, splitPane, type Layout, type SplitState } from "./split-model";
import { openTerminalTab } from "./terminal-tabs";
import { openWorkspaceTab } from "./workspace-tabs";
import { Prompt } from "./Prompt";
import { emptyCatalogue } from "../vocabulary";

afterEach(() => vi.unstubAllGlobals());
const fresh = () => oneP(SESSION_PANE);
const pair = () => splitPane(fresh(), "right", { id: "p2", title: "session" });
const expandKeys = { metaKey: true, shiftKey: true };
function keyEvent(key: string, held = {}) {
  const event = { key, metaKey: false, shiftKey: false, ctrlKey: false, altKey: false, repeat: false,
    defaultPrevented: false, ...held, preventDefault: vi.fn(() => { event.defaultPrevented = true; }), stopPropagation: vi.fn() };
  return event;
}
function draw(initial: SplitState, props: Partial<SplitProps> = {}, options?: Parameters<typeof create>[1]) {
  let state = initial, active = props.active ?? true, tree!: ReactTestRenderer;
  const onChange = vi.fn((next: SplitState) => { state = next; tree.update(render()); });
  const render = () => <Split top={[]} prompt={[]} context={[]} {...props} active={active} state={state} onChange={onChange} />;
  act(() => { tree = create(render(), options); });
  return {
    tree, onChange, get state() { return state; },
    root: () => tree.root.findByProps({ className: "split" }),
    grid: () => tree.root.findByProps({ className: "split-panes" }),
    panes: () => tree.root.findAllByType("section").filter(node => node.props["data-pane-id"]),
    update(next: SplitState, nextActive = true) { state = next; active = nextActive; act(() => tree.update(render())); },
    press(key: string, held = {}) {
      const event = keyEvent(key, held);
      act(() => {
        const root = tree.root.findByProps({ className: "split" });
        root.props.onKeyDownCapture(event);
        if (!event.stopPropagation.mock.calls.length) root.props.onKeyDown(event);
      });
      return event;
    },
    unmount() { act(() => tree.unmount()); },
  };
}

function* layouts(state = fresh()): Generator<SplitState> {
  yield state;
  if (state.panes.length === 4) return;
  for (const pane of state.panes) for (const direction of ["left", "right", "up", "down"] as const) {
    yield* layouts(splitPane(state, direction, { id: nextPane(state), title: "session" }, false, pane.id));
  }
}
const uneven = (layout: Layout, depth = 0): Layout => "pane" in layout ? layout
  : { ...layout, ratio: depth % 2 ? 0.67 : 0.31, first: uneven(layout.first, depth + 1), second: uneven(layout.second, depth + 1) };

it.each(["left", "right", "up", "down"] as const)("expands every leaf and restores every nested directional layout through four panes, including unequal ratios (first split %s)", direction => {
  const initial = fresh();
  const first = splitPane(initial, direction, { id: nextPane(initial), title: "session" }, false, initial.panes[0]!.id);
  for (const original of layouts(first)) {
    const state = { ...original, layout: uneven(original.layout) };
    const h = draw(state);
    for (const pane of state.panes) {
      const focused = focus(state, pane.id);
      h.update(focused);
      const before = JSON.stringify(focused), expected = geometry(focused);
      const event = h.press("F", expandKeys);
      expect(event.preventDefault).toHaveBeenCalledOnce();
      expect(h.grid().props.style).toEqual({ gridTemplateAreas: `"${pane.id}"`, gridTemplateRows: "minmax(0, 1fr)", gridTemplateColumns: "minmax(0, 1fr)" });
      expect(h.grid().props["data-panes"]).toBe(1);
      expect(h.panes().filter(node => !node.props.hidden).map(node => node.props["data-pane-id"])).toEqual([pane.id]);
      expect(h.tree.root.findByProps({ className: "split-keys" }).props.segments.map((s: { text: string }) => s.text).join(""))
        .toBe(`esc show all ${state.panes.length} panes`);
      expect(h.press("Escape").stopPropagation).toHaveBeenCalledOnce();
      expect(h.grid().props.style).toEqual(expected);
      expect(h.panes().every(node => !node.props.hidden)).toBe(true);
      expect(h.state).toBe(focused);
      expect(JSON.stringify(h.state)).toBe(before);
      expect(h.onChange).not.toHaveBeenCalled();
    }
    h.unmount();
  }
});

it("keeps all pane/tab instances and drafts while selecting workspace and terminal tabs", () => {
  let state = openWorkspaceTab(fresh(), "p1", "synthetic");
  state = splitPane(state, "down", terminalPane("p2"));
  state = openTerminalTab(state, "p2");
  const mounted = vi.fn(), disposed = vi.fn();
  function Draft({ id }: { id: string }) {
    const [text, setText] = useState("unfinished command");
    useEffect(() => { mounted(id); return () => { disposed(id); }; }, [id]);
    return <textarea data-draft={id} value={text} onChange={event => setText(event.target.value)} />;
  }
  const h = draw(state, { content: pane => <Draft id={`${pane.id}:${pane.history ?? pane.workspace ?? "origin"}`} /> });
  const drafts = h.tree.root.findAllByType("textarea");
  act(() => drafts[0]!.props.onChange({ target: { value: "kept draft" } }));
  expect(mounted).toHaveBeenCalledTimes(4);
  for (const id of ["p1", "p2"]) {
    h.update(focus(h.state, id)); h.press("f", expandKeys);
    const pane = h.panes().find(node => node.props["data-pane-id"] === id)!;
    const tabs = pane.findAllByProps({ role: "tab" });
    expect(tabs).toHaveLength(2);
    act(() => tabs[0]!.props.onClick({ stopPropagation() {} }));
    act(() => tabs[1]!.props.onClick({ stopPropagation() {} }));
    expect(h.grid().props["data-expanded-pane"]).toBe(id);
    h.press("Escape");
  }
  expect(h.tree.root.findAllByType("textarea")).toEqual(drafts);
  expect(drafts[0]!.props.value).toBe("kept draft");
  expect(mounted).toHaveBeenCalledTimes(4); expect(disposed).not.toHaveBeenCalled();
  h.unmount(); expect(disposed).toHaveBeenCalledTimes(4);
});

it("restores on focus navigation and never revives expansion when focus returns", () => {
  const h = draw(pair());
  h.press("f", expandKeys);
  h.press("ArrowRight", { metaKey: true });
  expect(h.state.focused).toBe("p2");
  expect(h.grid().props["data-expanded-pane"]).toBeUndefined();
  expect(h.panes().every(node => !node.props.hidden)).toBe(true);
  h.press("ArrowLeft", { metaKey: true });
  expect(h.grid().props["data-expanded-pane"]).toBeUndefined();
  h.press("f", expandKeys);
  h.press("Tab", { ctrlKey: true });
  expect(h.state.focused).toBe("p2");
  expect(h.grid().props["data-expanded-pane"]).toBeUndefined();
  h.unmount();
});

it("uses current splits and tab changes without restoring a stale snapshot, and clears expansion at one pane", () => {
  const h = draw(pair());
  h.press("f", expandKeys);
  h.update(openWorkspaceTab(h.state, "p1", "synthetic", true));
  expect(h.grid().props["data-expanded-pane"]).toBe("p1");
  act(() => h.panes()[0]!.findAllByProps({ className: "workspace-tab-close" })[1]!.props.onClick({ stopPropagation() {}, detail: 1 }));
  expect(h.state.panes[0]!.tabs).toHaveLength(1);
  expect(h.grid().props["data-expanded-pane"]).toBe("p1");
  h.update(splitPane(h.state, "up", { id: "p3", title: "session" }));
  expect(h.grid().props["data-expanded-pane"]).toBe("p1");
  h.press("Escape");
  expect(h.grid().props.style).toEqual(geometry(h.state)); expect(h.state.panes).toHaveLength(3);
  h.press("f", expandKeys);
  h.update(close(h.state, "p1"));
  expect(h.grid().props["data-expanded-pane"]).toBeUndefined();
  h.press("f", expandKeys);
  h.update(close(h.state, h.state.panes.find(p => p.id !== h.state.focused)!.id));
  expect(h.grid().props["data-expanded-pane"]).toBeUndefined();
  h.update(splitPane(h.state, "left", { id: "p4", title: "session" }));
  expect(h.grid().props["data-expanded-pane"]).toBeUndefined();
  h.unmount();
});

it("restores before dismissing prompt suggestions or a screen, then returns normal Escape ownership", () => {
  const state = { ...pair(), panes: [{ ...SESSION_PANE, shows: { screen: "open" as const } }, pair().panes[1]!] };
  const h = draw(state, { content: pane => pane.id === "p1" && <Prompt draft="$" catalogue={emptyCatalogue} names={["orders"]} aliases={{}}
    onDraft={vi.fn()} onSubmit={vi.fn()} onGrow={vi.fn()} onChrome={vi.fn()} chromeName="keys" /> });
  const pressAtPrompt = () => {
    const event = keyEvent("Escape");
    act(() => {
      h.root().props.onKeyDownCapture(event);
      if (!event.stopPropagation.mock.calls.length) h.tree.root.findByType("textarea").props.onKeyDown(event);
      if (!event.stopPropagation.mock.calls.length) h.root().props.onKeyDown(event);
    });
  };
  h.press("f", expandKeys);
  pressAtPrompt();
  expect(h.grid().props["data-expanded-pane"]).toBeUndefined();
  expect(h.tree.root.findAllByProps({ role: "listbox" })).toHaveLength(1);
  expect(h.onChange).not.toHaveBeenCalled();
  pressAtPrompt();
  expect(h.tree.root.findAllByProps({ role: "listbox" })).toHaveLength(0);
  expect(h.onChange).not.toHaveBeenCalled();
  pressAtPrompt();
  expect(h.state.panes[0]!.shows).toBeUndefined();
  h.unmount();
});

it("toggles deliberately, ignores held repeats and other modifier combinations, and does nothing for one pane", () => {
  const h = draw(pair());
  for (const held of [{ metaKey: true }, { shiftKey: true }, { ...expandKeys, ctrlKey: true }, { ...expandKeys, altKey: true }]) {
    expect(h.press("f", held).preventDefault).not.toHaveBeenCalled();
  }
  h.press("f", expandKeys);
  h.press("F", { ...expandKeys, repeat: true });
  expect(h.grid().props["data-expanded-pane"]).toBe("p1");
  for (const held of [{ ctrlKey: true }, { shiftKey: true }, { metaKey: true }, { altKey: true }]) {
    expect(h.press("Escape", held).preventDefault).not.toHaveBeenCalled();
    expect(h.grid().props["data-expanded-pane"]).toBe("p1");
  }
  h.press("f", expandKeys);
  expect(h.grid().props["data-expanded-pane"]).toBeUndefined();
  h.update(fresh());
  expect(h.press("f", expandKeys).preventDefault).not.toHaveBeenCalled();
  expect(h.press("Escape").preventDefault).not.toHaveBeenCalled();
  h.unmount();
});

it("handles body-focus shortcuts once, leaves Logs and inactive workspaces alone, and cleans up the listener", () => {
  class TestNode {}
  vi.stubGlobal("Node", TestNode);
  const inside = new TestNode(), outside = new TestNode();
  let listener!: (event: ReturnType<typeof keyEvent>) => void;
  const addEventListener = vi.fn((_name, handler) => { listener = handler; }), removeEventListener = vi.fn();
  vi.stubGlobal("document", { activeElement: outside, addEventListener, removeEventListener });
  const container = { contains: (node: unknown) => node === inside, querySelector: () => null };
  const h = draw(pair(), {}, { createNodeMock: node => node.props.className === "split" ? container : null });
  act(() => listener(keyEvent("f", { ...expandKeys, target: outside })));
  expect(h.grid().props["data-expanded-pane"]).toBe("p1");
  const event = keyEvent("f", { ...expandKeys, target: inside });
  act(() => { listener(event); h.root().props.onKeyDownCapture(event); });
  expect(h.grid().props["data-expanded-pane"]).toBeUndefined();
  expect(event.preventDefault).toHaveBeenCalledOnce();
  h.press("f", expandKeys);
  const logs = { closest: (selector: string) => selector === ".application-logs" ? {} : null };
  expect(h.press("Escape", { target: logs }).preventDefault).not.toHaveBeenCalled();
  h.update(h.state, false);
  for (const event of [keyEvent("Escape"), keyEvent("f", expandKeys)]) {
    act(() => listener(event)); expect(event.preventDefault).not.toHaveBeenCalled();
  }
  h.update(h.state);
  act(() => listener(keyEvent("Escape", { target: outside })));
  expect(h.grid().props["data-expanded-pane"]).toBeUndefined();
  h.unmount();
  expect(removeEventListener).toHaveBeenCalledWith("keydown", listener, true);
});

it("hides sibling flex panes from layout rather than merely covering them", () => {
  const css = readFileSync(new URL("./split.css", import.meta.url), "utf8");
  expect(css).toMatch(/\.wes-terminal \.split-pane\[hidden\]\s*\{\s*display:\s*none;/);
});
