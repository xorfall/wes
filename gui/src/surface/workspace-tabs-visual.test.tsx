import { describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { Split } from "./Split";
import { oneP, type Pane, type SplitState } from "./split-model";
import { readFileSync } from "node:fs";

const splitCss = readFileSync(new URL("./split.css", import.meta.url), "utf8");

const tabbed: Pane = { id: "p1", title: "session", tabs: [{}, { workspace: "sales-api" }, { workspace: "billing-eu-2026-q3-reconciliation" }] };
const state: SplitState = oneP(tabbed);

function draw(next: SplitState, onChange = vi.fn()): { tree: ReactTestRenderer; onChange: typeof onChange } {
  let tree: ReactTestRenderer | undefined;
  act(() => {
    tree = create(
      <Split originWorkspace="default" state={next} top={[]} prompt={[]} context={[]} onChange={onChange}
        content={(pane: Pane) => <span data-view={pane.workspace ?? "origin"}>{`view ${pane.workspace ?? "origin"}`}</span>} />,
    );
  });
  return { tree: tree!, onChange };
}

const tabsOf = (tree: ReactTestRenderer) => tree.root.findAllByProps({ role: "tab" }).filter(node => typeof node.type === "string");
const name = (node: { children: readonly unknown[] }) => node.children.join("");

describe("workspace tabs in a pane", () => {
  it("should_NameTheDefaultTabByItsWorkspace_And_ThePinnedOnesByWorkspace", () => {
    const { tree } = draw(state);
    expect(tabsOf(tree).map(name)).toEqual(["default", "sales-api", "billing-eu-2026-q3-reconciliation"]);
  });

  it("should_MarkOnlyTheActiveTabSelectedAndReachable_When_TheDefaultIsActive", () => {
    const { tree } = draw(state);
    expect(tabsOf(tree).map(tab => tab.props["aria-selected"])).toEqual([true, false, false]);
    expect(tabsOf(tree).map(tab => tab.props.tabIndex)).toEqual([0, -1, -1]);
  });

  it("should_KeepEveryTabViewMountedAndHideTheInactiveOnes", () => {
    const { tree } = draw({ ...state, panes: [{ ...tabbed, workspace: "sales-api" }] });
    const views = tree.root.findAllByProps({ role: "tabpanel" }).filter(node => typeof node.type === "string");
    expect(views.map(view => view.props.hidden)).toEqual([true, false, true]);
    expect(views.map(view => view.props["aria-labelledby"])).toEqual(tabsOf(tree).map(tab => tab.props.id));
  });

  it("should_SelectWithoutLettingThePaneClickOverwriteIt_When_ATabIsClicked", () => {
    const { tree, onChange } = draw(state);
    const stopPropagation = vi.fn();
    act(() => tabsOf(tree)[1]!.props.onClick({ stopPropagation }));
    expect(stopPropagation).toHaveBeenCalled();
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange.mock.calls[0]![0].panes[0]).toMatchObject({ workspace: "sales-api" });
  });

  it("should_OfferCloseOnlyWhileMoreThanOneTabIsOpen", () => {
    const closers = (next: SplitState) => draw(next).tree.root.findAllByProps({ className: "workspace-tab-close" }).filter(node => typeof node.type === "string");
    expect(closers(state)).toHaveLength(3);
    expect(closers(oneP({ ...tabbed, tabs: [{}] }))).toHaveLength(0);
  });

  it("should_CloseOnlyTheView_When_TheCloseControlIsClicked", () => {
    const { tree, onChange } = draw(state);
    const closer = tree.root.findAllByProps({ className: "workspace-tab-close" }).filter(node => typeof node.type === "string")[1]!;
    expect(closer.props["aria-label"]).toBe("Close sales-api tab");
    act(() => closer.props.onClick({ stopPropagation() {}, detail: 1 }));
    expect(onChange.mock.calls[0]![0].panes[0].tabs).toEqual([{}, { workspace: "billing-eu-2026-q3-reconciliation" }]);
  });
});

describe("workspace tab styling", () => {
  const rules = splitCss.slice(splitCss.indexOf(".workspace-tabs {"));

  it("should_UseOnlyLiveTokens_NoColourLiterals", () => {
    expect(rules).not.toMatch(/#[0-9a-fA-F]{3,8}\b|rgba?\(/);
  });

  it("should_NotDressATabAsAChip_NoRadiusNoFillAtRest", () => {
    expect(rules).not.toMatch(/border-radius/);
    expect(rules).not.toMatch(/\.workspace-tab\s*\{[^}]*background-color/);
  });

  it("should_TakeTheSelectedRailFromTheFocusedPaneAndShowKeyboardFocus", () => {
    expect(rules).toMatch(/\[aria-current="true"\] \.workspace-tab\[aria-selected="true"\] \{ border-bottom-color: var\(--mono-ref\)/);
    expect(rules).toMatch(/\.workspace-tab:focus-visible/);
  });

  it("should_ScrollHorizontallyAndCutLongNames", () => {
    expect(rules).toMatch(/overflow-x: auto/);
    expect(rules).toMatch(/text-overflow: ellipsis/);
  });
});

it("moves keyboard focus without activation and closes the focused tab with Delete", () => {
  const onChange = vi.fn();
  const buttons = Array.from({ length: 3 }, () => ({ focus: vi.fn() }));
  const row = { querySelectorAll: () => buttons, querySelector: () => null };
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<Split state={state} top={[]} prompt={[]} context={[]} onChange={onChange} />,
    { createNodeMock: node => node.props.className === "workspace-tabs" ? row : null }); });
  const press = (key: string, index: number, modifiers = {}) => {
    const event = { key, target: buttons[index], preventDefault: vi.fn(), stopPropagation: vi.fn(), ...modifiers };
    act(() => tree.root.findByProps({ role: "tablist" }).props.onKeyDown(event));
    return event;
  };
  press("ArrowLeft", 0); expect(buttons[2]!.focus).toHaveBeenCalledOnce();
  press("ArrowRight", 2); expect(buttons[0]!.focus).toHaveBeenCalledOnce();
  press("Home", 1); expect(buttons[0]!.focus).toHaveBeenCalledTimes(2);
  press("End", 0); expect(buttons[2]!.focus).toHaveBeenCalledTimes(2);
  expect(onChange).not.toHaveBeenCalled();
  expect(press("ArrowRight", 0, { metaKey: true }).preventDefault).not.toHaveBeenCalled();
  press("Delete", 1);
  expect(onChange.mock.calls[0]![0].panes[0].tabs).toEqual([{}, { workspace: "billing-eu-2026-q3-reconciliation" }]);
  expect(buttons[2]!.focus).toHaveBeenCalledTimes(3);
  act(() => tree.unmount());
});
