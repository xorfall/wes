import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { Screen } from "./Screen";
import { EnvScreen } from "./screens/Env";
import { environments } from "./screens/fixtures";

afterEach(() => vi.unstubAllGlobals());
const event = (defaultPrevented = false) => ({ key: "Escape", defaultPrevented, preventDefault: vi.fn(), stopPropagation: vi.fn() });
const frame = (tree: ReactTestRenderer) => tree.root.find(node => typeof node.type === "string" && node.props.tabIndex === -1);

describe.each(["full", "pane"] as const)("%s screen keyboard ownership", chrome => {
  it.each([false, true])("focuses visible screens without stealing focus from hidden tabs (hidden=%s)", hidden => {
    vi.stubGlobal("document", { activeElement: {} });
    const element = { closest: () => hidden ? {} : null, contains: () => false, focus: vi.fn() };
    const onClose = vi.fn();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Screen name="/settings" top={[]} footer={[]} chrome={chrome} onClose={onClose}><button>setting</button></Screen>,
      { createNodeMock: node => node.props.tabIndex === -1 ? element : null }); });
    expect(element.focus).toHaveBeenCalledTimes(hidden ? 0 : 1);
    if (!hidden) expect(element.focus).toHaveBeenCalledWith({ preventScroll: true });
    const key = event();
    act(() => frame(tree).props.onKeyDown(key));
    expect(onClose).toHaveBeenCalledOnce();
    expect(key.preventDefault).toHaveBeenCalledOnce();
    expect(key.stopPropagation).toHaveBeenCalledOnce();
    act(() => tree.unmount());
  });

  it("does not steal focus when restoring a screen in another visible pane", () => {
    vi.stubGlobal("document", { activeElement: {} });
    const element = { closest: (selector: string) => selector === ".split-pane" ? { getAttribute: () => undefined } : null,
      contains: () => false, focus: vi.fn() };
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Screen name="/graph" top={[]} footer={[]} chrome={chrome}>{null}</Screen>,
      { createNodeMock: node => node.props.tabIndex === -1 ? element : null }); });
    expect(element.focus).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("leaves child-consumed Escape and existing editor focus alone", () => {
    vi.stubGlobal("document", { activeElement: {} });
    const element = { closest: () => null, contains: () => true, focus: vi.fn() };
    const onClose = vi.fn();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Screen name="/edit" top={[]} footer={[]} chrome={chrome} onClose={onClose}><textarea /></Screen>,
      { createNodeMock: node => node.props.tabIndex === -1 ? element : null }); });
    expect(element.focus).not.toHaveBeenCalled();
    const key = event(true);
    act(() => frame(tree).props.onKeyDown(key));
    expect(onClose).not.toHaveBeenCalled();
    expect(key.stopPropagation).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it.each([{ nativeEvent: { isComposing: true } }, { isComposing: true }, { keyCode: 229 }])("leaves a composing Escape to the input method and closes on the next one (%j)", composition => {
    vi.stubGlobal("document", { activeElement: {} });
    const element = { closest: () => null, contains: () => true, focus: vi.fn() };
    const onClose = vi.fn();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Screen name="/edit" top={[]} footer={[]} chrome={chrome} onClose={onClose}><textarea /></Screen>,
      { createNodeMock: node => node.props.tabIndex === -1 ? element : null }); });
    const composed = { ...event(), ...composition };
    act(() => frame(tree).props.onKeyDown(composed));
    expect(onClose).not.toHaveBeenCalled();
    expect(composed.preventDefault).not.toHaveBeenCalled();
    expect(composed.stopPropagation).not.toHaveBeenCalled();
    act(() => frame(tree).props.onKeyDown(event()));
    expect(onClose).toHaveBeenCalledOnce();
    act(() => tree.unmount());
  });

  it("cancels environment confirmation before closing its screen", () => {
    const onClose = vi.fn(), onChoose = vi.fn();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<EnvScreen top={[]} environments={environments} chosen="DEV" chrome={chrome} onClose={onClose} onChoose={onChoose} />); });
    act(() => tree.root.findAllByProps({ role: "radio" })[2]!.props.onClick());
    act(() => frame(tree).props.onKeyDown(event()));
    expect(onClose).not.toHaveBeenCalled();
    expect(onChoose).not.toHaveBeenCalled();
    act(() => frame(tree).props.onKeyDown(event()));
    expect(onClose).toHaveBeenCalledOnce();
    act(() => tree.unmount());
  });
});
