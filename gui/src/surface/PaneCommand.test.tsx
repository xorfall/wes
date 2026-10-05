import { act, create } from "react-test-renderer";
import { expect, it, vi } from "vitest";
import { PaneCommand } from "./PaneCommand";

it("offers workspace variables from value panes and accepts without submitting", () => {
  const onCommand = vi.fn(); let tree!: ReturnType<typeof create>;
  act(() => { tree = create(<PaneCommand onCommand={onCommand} variables={["orders", "other"]} />); });
  const field = () => tree.root.findByType("input");
  act(() => field().props.onChange({ target: { value: "/goto or", selectionStart: 8 } }));
  expect(JSON.stringify(tree.toJSON())).toContain("$orders");
  const preventDefault = vi.fn();
  act(() => field().props.onKeyDown({ key: "Tab", preventDefault }));
  expect(field().props.value).toBe("/goto $orders ");
  expect(onCommand).not.toHaveBeenCalled();
  act(() => tree.root.findByType("form").props.onSubmit({ preventDefault }));
  expect(onCommand).toHaveBeenCalledExactlyOnceWith("/goto $orders ");
  expect(field().props.value).toBe("");
  act(() => tree.unmount());
});

it.each([{ nativeEvent: { isComposing: true } }, { keyCode: 229 }])("leaves completion keys to an input method's composition (%j)", composition => {
  const onCommand = vi.fn(); let tree!: ReturnType<typeof create>;
  act(() => { tree = create(<PaneCommand onCommand={onCommand} variables={["orders", "other"]} />); });
  const field = () => tree.root.findByType("input");
  act(() => field().props.onChange({ target: { value: "/goto or", selectionStart: 8 } }));
  for (const key of ["Tab", "ArrowDown", "Escape"]) {
    const preventDefault = vi.fn(), stopPropagation = vi.fn();
    act(() => field().props.onKeyDown({ key, ...composition, preventDefault, stopPropagation }));
    expect([key, preventDefault.mock.calls.length, stopPropagation.mock.calls.length]).toEqual([key, 0, 0]);
  }
  expect(field().props.value).toBe("/goto or");
  expect(JSON.stringify(tree.toJSON())).toContain("$orders");
  expect(onCommand).not.toHaveBeenCalled();
  act(() => tree.unmount());
});
