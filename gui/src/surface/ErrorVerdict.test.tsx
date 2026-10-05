import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { Cell, type Theme, type CellProps } from "./Cell";
import { lineText, type Segment } from "./MonoLine";

const failure: Segment[] = [
  { text: "CAL001: a synthetic error with a long diagnostic\n  called from fixture.wes · line 2, column 4", role: "mono-ink" },
];
const rows = (text: string) => [{ segments: [{ text }], nodes: [] }];
const props: CellProps = { theme: "keys", state: "failed", rows: rows(":calc invalid"), verdict: [{slot:"state",segments:[{text:"failed",role:"mono-bad-strong"}],keep:true},{ segments: failure, keep: true },{slot:"retention",segments:[{text:"not kept",role:"mono-dim"}],keep:false}] };
let tree: ReactTestRenderer | undefined;
afterEach(() => { if (tree) act(() => tree!.unmount()); tree = undefined; vi.unstubAllGlobals(); });
function fixture(overrides: Partial<CellProps> = {}, overflow = true) {
  const geometry = { clientWidth: 200, scrollWidth: overflow ? 500 : 200 };
  const callbacks: (() => void)[] = [];
  const observers: { observe: ReturnType<typeof vi.fn>; disconnect: ReturnType<typeof vi.fn> }[] = [];
  vi.stubGlobal("ResizeObserver", class {
    observe = vi.fn(); disconnect = vi.fn();
    constructor(callback: () => void) { callbacks.push(callback); observers.push(this); }
  });
  act(() => { tree = create(<Cell {...props} {...overrides} />, {
    createNodeMock: node => node.type === "pre" && String(node.props.className).includes("cell-verdict") ? geometry : null,
  }); });
  return { geometry, callbacks, observers, update: (next: Partial<CellProps>) => act(() => tree!.update(<Cell {...props} {...overrides} {...next} />)) };
}
const region = () => tree!.root.findByProps({ className: "cell-verdict-disclosure" });
const content = () => tree!.root.findAllByType("pre").find(pre => String(pre.props.className).includes("cell-verdict"))!;
const expanded = () => region().props["aria-expanded"];
const said = () => content().findAllByType("span").map(span => span.children.join("")).join("");
function pointer(overrides: Record<string, unknown> = {}) {
  return { metaKey: true, ctrlKey: false, altKey: false, shiftKey: false, button: 0, preventDefault: vi.fn(), stopPropagation: vi.fn(), ...overrides };
}
function click(overrides: Record<string, unknown> = {}) {
  const event = pointer(overrides);
  act(() => region().props.onClick(event));
  return event;
}
function key(name: string, overrides: Record<string, unknown> = {}) {
  const event = { key: name, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, preventDefault: vi.fn(), stopPropagation: vi.fn(), ...overrides };
  act(() => region().props.onKeyDown(event));
  return event;
}

describe("clipped failed verdict disclosure", () => {
  it.each<Theme>(["keys", "controls"])("keeps Cmd+click and a visible expand control for a clipped message in %s", chrome => {
    const repeat = vi.fn();
    const { geometry, callbacks } = fixture({ theme: chrome, actions: { repeat } });
    expect(tree!.root.findAllByType("button").filter(button => /show (more|less)/.test(button.children.join("")))).toHaveLength(0);
    expect(tree!.root.findAll(node => String(node.props.className ?? "").includes("cell-verdict-toggle"))).toHaveLength(1);
    expect(region().props.role).toBe("button");
    expect(region().props.tabIndex).toBe(0);
    expect(expanded()).toBe(false);
    expect(region().props["data-hint"]).toBeUndefined();
    expect(region().props["aria-description"]).toContain("⌘click");
    expect(said()).toBe(lineText(failure));
    const opened = click();
    expect(opened.preventDefault).toHaveBeenCalledOnce();
    expect(opened.stopPropagation).toHaveBeenCalledOnce();
    expect(expanded()).toBe(true);
    expect(content().props.className).toContain("cell-verdict-expanded");
    expect(said()).toBe(lineText(failure));
    expect(content().props.onCopy).toBeUndefined();
    expect(content().props.onMouseDown).toBeUndefined();
    expect(region().props.onMouseDown).toBeUndefined();
    // Wrapped text becomes narrower, but resize must not take the expanded message away.
    geometry.scrollWidth = 200;
    act(() => callbacks[0]!());
    expect(expanded()).toBe(true);
    geometry.scrollWidth = 500;
    click();
    expect(expanded()).toBe(false);
    expect(content().props.className).not.toContain("cell-verdict-expanded");
    expect(said()).toBe(lineText(failure));
    expect(repeat).not.toHaveBeenCalled();
  });

  it("opens the full failure through a discoverable button without invoking peek or run actions", () => {
    const peek=vi.fn(),repeat=vi.fn();
    fixture({actions:{peek,repeat}});
    const toggle=()=>tree!.root.findByProps({className:"cell-action cell-verdict-toggle"});
    expect(toggle().children.join("")).toBe("▸ show full message");
    expect(toggle().props["aria-expanded"]).toBe(false);
    const event=pointer({metaKey:false});
    act(()=>toggle().props.onClick(event));
    expect(event.stopPropagation).toHaveBeenCalledOnce();
    expect(expanded()).toBe(true);
    expect(toggle().children.join("")).toBe("▾ fold message");
    expect(toggle().props["aria-expanded"]).toBe(true);
    expect(said()).toBe(lineText(failure));
    act(()=>toggle().props.onClick(pointer({metaKey:false})));
    expect(expanded()).toBe(false);
    expect(peek).not.toHaveBeenCalled();expect(repeat).not.toHaveBeenCalled();
    click();expect(peek).toHaveBeenCalledWith("error");
  });

  it("ignores ordinary clicks, selection gestures, other modifiers and other mouse buttons", () => {
    fixture();
    for (const overrides of [{ metaKey: false }, { ctrlKey: true }, { altKey: true }, { shiftKey: true }, { button: 1 }, { button: 2 }]) {
      const event = click(overrides);
      expect(event.preventDefault).not.toHaveBeenCalled();
      expect(event.stopPropagation).not.toHaveBeenCalled();
      expect(expanded()).toBe(false);
    }
  });

  it("keeps the click away from the cell and from the command's own Cmd+click", () => {
    const openSource = vi.fn();
    fixture({ actions: { openSource } });
    expect(tree!.root.findByType("section").props.onClick).toBeUndefined();
    const event = click();
    expect(event.stopPropagation).toHaveBeenCalledOnce();
    expect(openSource).not.toHaveBeenCalled();
    expect(region()).not.toBe(tree!.root.findAllByProps({ className: "cell-source" })[0]);
  });

  it("answers to Enter and Space when focused, without the cell taking Space as collapse", () => {
    const cycle = vi.fn();
    fixture({ actions: { cycle } });
    const enter = key("Enter");
    expect(enter.preventDefault).toHaveBeenCalledOnce(); expect(enter.stopPropagation).toHaveBeenCalledOnce();
    expect(expanded()).toBe(true);
    const space = key(" ");
    expect(space.stopPropagation).toHaveBeenCalledOnce();
    expect(expanded()).toBe(false);
    for (const other of [key("j"), key("Enter", { metaKey: true }), key("Enter", { ctrlKey: true }), key(" ", { shiftKey: true }), key("Enter", { altKey: true })]) {
      expect(other.stopPropagation).not.toHaveBeenCalled();
    }
    expect(expanded()).toBe(false);
    expect(cycle).not.toHaveBeenCalled();
  });

  it("offers no tab stop, hint or gesture until genuine overflow, and follows width changes", () => {
    const { geometry, callbacks, observers, update } = fixture({}, false);
    expect(region().props.role).toBeUndefined();
    expect(region().props.tabIndex).toBeUndefined();
    expect(region().props["data-hint"]).toBeUndefined();
    expect(region().props.onClick).toBeUndefined();
    expect(tree!.root.findAllByType("button")).toHaveLength(0);
    // A wider font can change only text width during a parent rerender.
    geometry.scrollWidth = 250;
    update({ theme: "controls" });
    expect(region().props.role).toBe("button");
    geometry.clientWidth = 100;
    act(() => callbacks[0]!());
    expect(region().props.role).toBe("button");
    geometry.clientWidth = 600;
    act(() => callbacks[0]!());
    expect(region().props.role).toBeUndefined();
    act(() => tree!.unmount()); tree = undefined;
    expect(observers[0]!.disconnect).toHaveBeenCalledOnce();
    act(() => callbacks[0]!());
  });

  it("does not call a successful unknown result an error", () => {
    fixture({ state: "default", verdict: [{slot:"state",segments:[{text:"ok"}],keep:true},{segments:[{text:"Unknown"}],keep:true},{slot:"retention",segments:[{text:"kept"}],keep:false}] });
    expect(tree!.root.findAllByProps({ className: "cell-verdict-disclosure" })).toHaveLength(0);
    expect(tree!.root.findAllByProps({ role: "button" })).toHaveLength(0);
  });

  it("wraps explanatory notes without truncation and preserves passive inspection", () => {
    const text = "newer input waiting · previous result shown";
    fixture({ state: "default", verdict: [{slot:"state",segments:[{text:"ok"}],keep:true},{ segments: [{ text }], keep: true }] });
    const scroll = tree!.root.findByProps({ "aria-label": "Result status and type" });
    expect(scroll.props.tabIndex).toBeUndefined();
    expect(scroll.findByProps({className:"mono-line cell-run-notes"}).findAllByType("span").map(span => span.children.join("")).join("")).toBe(text);
    // Not a disclosure: a plain click does nothing, and only ⌘click reaches the type's window.
    const peek = vi.fn();
    fixture({ state: "default", verdict: [{slot:"state",segments:[{text:"ok"}],keep:true},{ segments: [{ text }], keep: true }], actions: { peek } });
    const again = tree!.root.findByProps({ "aria-label": "Result status and type" });
    act(() => again.props.onClick({ metaKey: false, button: 0, preventDefault() {}, stopPropagation() {}, target: { closest: () => null } }));
    expect(peek).not.toHaveBeenCalled();
    act(() => again.props.onClick({ metaKey: true, button: 0, preventDefault() {}, stopPropagation() {}, target: { closest: () => null } }));
    expect(peek).toHaveBeenCalledWith("type");
  });

  it("resets after a different error, edited source or a new run", () => {
    const { update } = fixture({ attempt: "first" });
    click();
    update({ attempt: "second" });
    expect(expanded()).toBe(false);
    click();
    update({ verdict: [{slot:"state",segments:[{text:"failed"}],keep:true},{ segments: [{ text: "changed message" }], keep: true },{slot:"retention",segments:[{text:"not kept"}],keep:false}] });
    expect(expanded()).toBe(false);
    click();
    update({ rows: rows(":calc another"), attempt: "another" });
    expect(expanded()).toBe(false);
    click();
    update({ state: "live" });
    expect(tree!.root.findAllByProps({ className: "cell-verdict-disclosure" })).toHaveLength(0);
    update({ state: "failed" });
    expect(expanded()).toBe(false);
  });
});
