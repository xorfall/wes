import { terminalPane } from "./split-model";
import { afterEach, expect, it, vi } from "vitest";
import { act, create } from "react-test-renderer";
import { read } from "./commands";
import { close, focus, geometry, navigate, nextPane, oneP, rectangles, SESSION_PANE, splitPane, type SplitDirection, type SplitState } from "./split-model";
import { restoreSplit } from "./split-storage";
import { Split } from "./Split";
import { promptCompletion } from "./prompt-complete";
import { emptyCatalogue } from "../vocabulary";
const directions = ["left", "right", "up", "down"] as const;
const add = (s: SplitState, d: SplitDirection, x = false, source = s.focused) => splitPane(s, d, { id: nextPane(s), title: "session" }, x, source);
const fresh = () => oneP(SESSION_PANE);
afterEach(() => vi.unstubAllGlobals());
it.each([["l", "left"], ["r", "right"], ["t", "up"], ["b", "down"]] as const)("parses /%ssplit and its focus/content variant", (prefix, direction) => {
  expect(read(`/${prefix}split`)).toEqual({ kind: "directional-split", direction, takeFocus: false });
  expect(read(`/${prefix}splitx xterm`)).toEqual({ kind: "directional-split", direction, takeFocus: true, content: { terminal: true } });
  expect(read(`/${prefix}split /open $orders`)).toMatchObject({ content: { screen: "open", node: "orders" } });
});
it.each(["/rsplit ../invalid", "/rsplitx xterm extra", "/bsplit /theme ink", "/lsplit /graph extra", "/tsplit /settings unknown", "/rsplit /open $x split"])("refuses invalid content: %s", command => {
  expect(read(command).kind).toBe("trouble");
});
it("offers the directional commands in the actual surface completion", () => {
  expect(promptCompletion({ line: "/rs", caret: 3, catalogue: emptyCatalogue, names: [], aliases: {} }).items.map(i => i.text)).toEqual(["/rsplit", "/rsplitx"]);
});
it("splits its source without moving unrelated panes, preserves focus, and adds the fourth below A", () => {
  let state = add(fresh(), "right");
  expect(state.focused).toBe("p1");
  state = add(state, "down", true, "p2");
  expect(geometry(state).gridTemplateAreas).toBe('"p1 p2" "p1 p3"');
  expect(state.focused).toBe("p3");
  const right = rectangles(state).filter(p => p.id !== "p1");
  state = add(state, "down", false, "p1");
  expect(geometry(state).gridTemplateAreas).toBe('"p1 p2" "p4 p3"');
  expect(rectangles(state).filter(p => p.id === "p2" || p.id === "p3")).toEqual(right);
  expect(add(state, "left", true)).toBe(state);
});
it("exhausts every source/direction combination up to four panes, including closes and reloads", () => {
  function check(state: SplitState) {
    const boxes = rectangles(state);
    expect(boxes.reduce((sum, r) => sum + r.width * r.height, 0)).toBeCloseTo(1);
    expect(new Set(boxes.map(r => r.id)).size).toBe(state.panes.length);
    for (const a of boxes) for (const b of boxes) if (a !== b) {
      const overlap = Math.min(a.x+a.width,b.x+b.width)-Math.max(a.x,b.x) > 1e-8 && Math.min(a.y+a.height,b.y+b.height)-Math.max(a.y,b.y) > 1e-8;
      expect(overlap).toBe(false);
    }
    expect(restoreSplit(JSON.parse(JSON.stringify(state)))).toEqual(state);
    for (const pane of state.panes) {
      const closed = close(state, pane.id);
      expect(rectangles(closed).reduce((sum,r) => sum+r.width*r.height,0)).toBeCloseTo(1);
      expect(closed.panes.some(p => p.id === closed.focused)).toBe(true);
      expect(restoreSplit(closed)).toEqual(closed);
    }
    if (state.panes.length === 4) return;
    for (const pane of state.panes) for (const direction of directions) check(add(state, direction, false, pane.id));
  }
  check(fresh());
});
it("navigates real neighbors without wrapping and remembers which of two neighbors was used", () => {
  let state = add(add(fresh(), "right"), "down", true, "p2");
  expect(navigate(state, "right")).toBe(state);
  expect(navigate(state, "down")).toBe(state);
  state = navigate(state, "left");
  expect(state.focused).toBe("p1");
  expect(navigate(state, "left")).toBe(state);
  expect(navigate(state, "right").focused).toBe("p3");
  expect(navigate(focus(state, "p2"), "down").focused).toBe("p3");
  expect(navigate(focus(state, "p3"), "up").focused).toBe("p2");
});
it.each(["right", "down"] as const)("coalesces rounded shared edges in nested %s splits without adding phantom tracks", axis => {
  const state: SplitState = { ...fresh(), panes: [1, 2, 3, 4].map(n => ({ id: `p${n}`, title: "session" })),
    layout: { axis, ratio: 0.31, first: { axis, ratio: 0.67,
      first: { axis, ratio: 0.31, first: { pane: "p4" }, second: { pane: "p3" } }, second: { pane: "p2" } }, second: { pane: "p1" } } };
  const grid = geometry(state);
  expect(grid.gridTemplateAreas).toBe(axis === "right" ? '"p4 p3 p2 p1"' : '"p4" "p3" "p2" "p1"');
  const tracks = axis === "right" ? grid.gridTemplateColumns : grid.gridTemplateRows;
  expect(tracks.match(/minmax/g)).toHaveLength(4);
});
it("rejects broken trees, duplicate leaves, unsupported versions and invalid content", () => {
  const state = add(fresh(), "right");
  for (const bad of [null, {}, { ...state, version: 2 }, { ...state, focused: "missing" }, { ...state, layout: { pane: "p1" } },
    { ...state, layout: { axis: "down", ratio: 0.5, first: { pane: "p1" }, second: { pane: "p1" } } },
    { ...state, panes: [{ id: "p1", shows: { screen: "malicious" } }, state.panes[1]] },
    { ...state, layout: { ...state.layout, ratio: NaN } }]) expect(restoreSplit(bad)).toEqual(fresh());
});
it("restores screen descriptors, terminal directory, ratios and focus without storing a process", () => {
  let state = splitPane(fresh(), "left", terminalPane("p2", { cwd: "/synthetic/project" }), true);
  state = splitPane(state, "down", { id: "p3", title: "/open $orders", shows: { screen: "open", node: "orders", tab: "source" } });
  if (!("axis" in state.layout)) throw new Error("split expected");
  state = { ...state, layout: { ...state.layout, ratio: 0.3 } };
  expect(restoreSplit(JSON.parse(JSON.stringify(state)))).toEqual(state);
});
it("collapses a closed parent and never reuses the original session identity", () => {
  const state = close(add(fresh(), "left", true), "p1");
  expect(state.layout).toEqual({ pane: "p2" });
  expect(add(state, "up").panes.map(p => p.id)).toEqual(["p2", "p3"]);
  expect(close(state, "p2")).toBe(state);
});
it("captures Cmd+arrows before editor/shell handlers and transfers actual DOM focus", () => {
  const source = { focus: vi.fn() }, target = { focus: vi.fn() };
  const pane = (input: typeof source) => ({ closest: () => null, contains: (node: unknown) => node === input, querySelector: () => input });
  vi.stubGlobal("document", { activeElement: source });
  const container = { querySelector: (selector: string) => selector.includes('p2') ? pane(target) : pane(source) };
  const onChange = vi.fn(); const state = add(fresh(), "right");
  let tree!: ReturnType<typeof create>;
  const draw = (s: SplitState) => <Split state={s} top={[]} prompt={[]} context={[]} onChange={onChange} />;
  act(() => { tree = create(draw(state), { createNodeMock: element => element.props.className === "split" ? container : null }); });
  const event = { key: "ArrowRight", metaKey: true, preventDefault: vi.fn(), stopPropagation: vi.fn() };
  act(() => tree.root.findByProps({ className: "split" }).props.onKeyDownCapture(event));
  expect(event.preventDefault).toHaveBeenCalled(); expect(event.stopPropagation).toHaveBeenCalled();
  act(() => tree.update(draw(onChange.mock.calls[0]![0])));
  expect(target.focus).toHaveBeenCalledWith({ preventScroll: true });
  act(() => tree.unmount());
});

it("keeps session and xterm contents title-free and exposes pane count for conditional frames", () => {
  const states = [fresh(), oneP(terminalPane("p2")),
    splitPane(fresh(), "right", terminalPane("p2"))];
  for (const state of states) {
    let tree!: ReturnType<typeof create>;
    act(() => { tree = create(<Split state={state} top={[]} prompt={[]} context={[]}
      content={pane => <span className="test-pane-content">{`content:${pane.id}`}</span>} />); });
    expect(tree.root.findAllByProps({ className: "split-pane-title" })).toHaveLength(0);
    const panes = tree.root.findAllByType("section");
    expect(panes).toHaveLength(state.panes.length);
    expect(panes.map(p => p.props["data-terminal"])).toEqual(state.panes.map(p => p.terminal ? "true" : undefined));
    expect(tree.root.findByProps({ className: "split-panes" }).props["data-panes"]).toBe(state.panes.length);
    expect(panes.every(p => p.props.className === "split-pane split-pane-plain")).toBe(true);
    expect(panes.map(p => p.findByProps({className:"test-pane-content"}).children.join(""))).toEqual(state.panes.map(p => `content:${p.id}`));
    act(() => tree.unmount());
  }
});

it.each(["session", "xterm", "screen"] as const)("returns to the %s input rather than a previously clicked action, preserving draft and selection", kind => {
  const selected = kind === "session" ? ".prompt-field" : kind === "xterm" ? ".xterm-helper-textarea" : ".pane-command input";
  const input = { focus: vi.fn(), value: "unfinished command", selectionStart: 4, selectionEnd: 9 };
  const clicked = { focus: vi.fn(), isConnected: true }, other = { focus: vi.fn() };
  const documentState = { activeElement: clicked as unknown };
  vi.stubGlobal("document", documentState);
  const panel = {
    closest: () => null, contains: (node: unknown) => node === clicked || node === input,
    querySelector: (selector: string) => selector === `:is(${selected}):not([hidden], [hidden] *)` ? input : null,
  };
  const container = { querySelector: (selector: string) => selector.includes('p1') ? panel
    : { closest: () => null, contains: (node: unknown) => node === other, querySelector: () => other } };
  const onChange = vi.fn(); let state = add(fresh(), "right");
  let tree!: ReturnType<typeof create>;
  const draw = () => <Split state={state} top={[]} prompt={[]} context={[]} onChange={onChange} />;
  act(() => { tree = create(draw(), { createNodeMock: node => node.props.className === "split" ? container : null }); });
  // A mouse focus on an output/action must stay there until keyboard navigation is requested.
  act(() => tree.root.findByProps({ "data-pane-id": "p1" }).props.onFocus({ target: clicked }));
  expect(input.focus).not.toHaveBeenCalled();
  const move = (key: string) => act(() => {
    tree.root.findByProps({ className: "split" }).props.onKeyDownCapture({ key, metaKey: true, preventDefault() {}, stopPropagation() {} });
    state = onChange.mock.calls.at(-1)![0]; tree.update(draw());
  });
  move("ArrowRight"); documentState.activeElement = other;
  move("ArrowLeft");
  expect(input.focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
  expect(clicked.focus).not.toHaveBeenCalled();
  expect(input).toMatchObject({ value: "unfinished command", selectionStart: 4, selectionEnd: 9 });
  input.focus.mockClear(); documentState.activeElement = input;
  move("ArrowLeft"); // Outer edge: do not cycle, rerender input or issue another focus.
  expect(state.focused).toBe("p1"); expect(input.focus).not.toHaveBeenCalled();
  act(() => tree.unmount());
});


it.each(["single", "p1", "p2"])("Cmd+L refocuses the same session prompt from its cell without changing text (%s)", which => {
  const id = which === "single" ? "p1" : which;
  const input = { focus: vi.fn(), value: "unfinished command", selectionStart: 4, selectionEnd: 9 };
  const cell = {};
  vi.stubGlobal("document", { activeElement: cell });
  const panel = { closest: () => null, contains: () => true, querySelector: (selector: string) => selector === ":is(.prompt-field):not([hidden], [hidden] *)" ? input : null };
  const container = { querySelector: vi.fn(() => panel) };
  const onChange = vi.fn();
  const state = { ...(which === "single" ? fresh() : add(fresh(), "right")), focused: id };
  let tree!: ReturnType<typeof create>;
  act(() => { tree = create(<Split state={state} top={[]} prompt={[]} context={[]} onChange={onChange} />,
    { createNodeMock: node => node.props.className === "split" ? container : null }); });
  const event = { key: "l", metaKey: true, preventDefault: vi.fn(), stopPropagation: vi.fn() };
  act(() => tree.root.findByProps({ className: "split" }).props.onKeyDownCapture(event));
  expect(container.querySelector).toHaveBeenLastCalledWith(`[data-pane-id="${id}"] .workspace-tab-view:not([hidden], [hidden] *)`);
  expect(input.focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
  expect(input).toMatchObject({ value: "unfinished command", selectionStart: 4, selectionEnd: 9 });
  expect(event.preventDefault).toHaveBeenCalledOnce(); expect(event.stopPropagation).toHaveBeenCalledOnce();
  expect(onChange).not.toHaveBeenCalled();
  act(() => tree.unmount());
});

it.each([{}, { metaKey: true, shiftKey: true }, { metaKey: true, ctrlKey: true }, { metaKey: true, altKey: true }])("leaves other L shortcuts alone (%j)", modifiers => {
  const input = { focus: vi.fn() };
  const container = { querySelector: () => ({ closest: () => null, contains: () => true, querySelector: () => input }) };
  vi.stubGlobal("document", { activeElement: {} });
  let tree!: ReturnType<typeof create>;
  act(() => { tree = create(<Split state={fresh()} top={[]} prompt={[]} context={[]} />,
    { createNodeMock: node => node.props.className === "split" ? container : null }); });
  const event = { key: "l", ...modifiers, preventDefault: vi.fn(), stopPropagation: vi.fn() };
  act(() => tree.root.findByProps({ className: "split" }).props.onKeyDownCapture(event));
  expect(input.focus).not.toHaveBeenCalled(); expect(event.preventDefault).not.toHaveBeenCalled();
  act(() => tree.unmount());
});

it("leaves Cmd+L to non-session panes and inactive workspaces", () => {
  vi.stubGlobal("document", { activeElement: {} });
  for (const active of [true, false]) {
    const prompt = { focus: vi.fn() };
    const container = { querySelector: () => ({ closest: () => null, contains: () => true, querySelector: () => active ? null : prompt }) };
    let tree!: ReturnType<typeof create>;
    act(() => { tree = create(<Split state={fresh()} active={active} top={[]} prompt={[]} context={[]} />,
      { createNodeMock: node => node.props.className === "split" ? container : null }); });
    const event = { key: "l", metaKey: true, preventDefault: vi.fn(), stopPropagation: vi.fn() };
    act(() => tree.root.findByProps({ className: "split" }).props.onKeyDownCapture(event));
    expect(event.preventDefault).not.toHaveBeenCalled(); expect(prompt.focus).not.toHaveBeenCalled();
    act(() => tree.unmount());
  }
});

it("focuses the visible tab screen and never sends Cmd+L to its hidden session prompt", () => {
  vi.stubGlobal("document", { activeElement: {} });
  const screen = { focus: vi.fn() };
  const hiddenPrompt = { focus: vi.fn() };
  const panel = { closest: () => null, contains: () => false,
    querySelector: vi.fn((selector: string) => {
      if (selector === ":is(.screen):not([hidden], [hidden] *)") return screen;
      if (selector === ".prompt-field") return hiddenPrompt;
      return null;
    }) };
  const container = { querySelector: () => panel };
  let tree!: ReturnType<typeof create>;
  act(() => { tree = create(<Split state={fresh()} top={[]} prompt={[]} context={[]} />,
    { createNodeMock: node => node.props.className === "split" ? container : null }); });
  expect(screen.focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
  const key = { key: "l", metaKey: true, preventDefault: vi.fn(), stopPropagation: vi.fn() };
  act(() => tree.root.findByProps({ className: "split" }).props.onKeyDownCapture(key));
  expect(hiddenPrompt.focus).not.toHaveBeenCalled();
  expect(key.preventDefault).not.toHaveBeenCalled();
  act(() => tree.unmount());
});
