import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, it, vi } from "vitest";
import { ResultSize } from "./ResultSize";
import type { CellView } from "../cells";

it("keeps one tab stop, wraps arrow navigation, and focuses the requested size", () => {
  const change = vi.fn(), focus = [vi.fn(), vi.fn(), vi.fn()];
  let tree!: ReactTestRenderer;
  const render = (value: CellView, disabled = false) => <ResultSize value={value} identity="id1" disabled={disabled} onChange={change}/>;
  act(() => { tree = create(render("preview"), { createNodeMock: element => element.type === "button" ? { focus: focus[["collapsed id1", "preview id1", "expanded id1"].indexOf(element.props["aria-label"])] } : null }); });
  expect(tree.root.findAllByType("button").map(button => button.props.tabIndex)).toEqual([-1, 0, -1]);
  const key = (name: string, key: string) => act(() => tree.root.findByProps({ "aria-label": name }).props.onKeyDown({ key, preventDefault(){}, stopPropagation(){} }));
  key("preview id1", "ArrowRight"); expect(change).toHaveBeenLastCalledWith("expanded"); expect(focus[2]).toHaveBeenCalledWith({preventScroll:true});
  key("expanded id1", "ArrowDown"); expect(change).toHaveBeenLastCalledWith("collapsed");
  key("collapsed id1", "ArrowLeft"); expect(change).toHaveBeenLastCalledWith("expanded");
  key("preview id1", "Home"); expect(change).toHaveBeenLastCalledWith("collapsed");
  key("preview id1", "End"); expect(change).toHaveBeenLastCalledWith("expanded");
  act(() => tree.update(render("expanded", true)));
  expect(tree.root.findAllByType("button").every(button => button.props.disabled)).toBe(true);
  change.mockClear(); key("expanded id1 (unavailable: no stored value)", "ArrowRight"); expect(change).not.toHaveBeenCalled();
  act(() => tree.unmount());
});
