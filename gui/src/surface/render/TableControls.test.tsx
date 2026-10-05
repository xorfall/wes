import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { tableViewStore } from "../../presentation/table-views";
import type { TableArrangement } from "../../presentation/types";
import { MoreRows, PinButton, resizeColumn, TableToolbar } from "./TableControls";
import { columnsFor } from "./TableGrid";

let tree: ReactTestRenderer | undefined;
afterEach(() => { if (tree) act(() => tree!.unmount()); tree = undefined; tableViewStore.seed({}); });
const textOf = (node: ReactTestInstance | string): string => typeof node === "string" ? node : node.children.map(textOf).join("");
const arrangement = (over: Partial<TableArrangement> = {}): TableArrangement =>
  ({ key: "type:Order", columns: ["id", "customer", "note"], hidden: [], unpinned: false, step: 20, ...over });
const byLabel = (label: string) => tree!.root.findByProps({ "aria-label": label });

describe("table controls", () => {
  it("should_OfferIconOnlyButtons_When_TheToolbarIsDrawn", () => {
    // Act
    act(() => { tree = create(<TableToolbar path="" arrangement={arrangement()} onFilter={vi.fn()} />); });
    // Assert: every button names itself for assistive technology and draws only an icon
    for (const button of tree!.root.findAllByType("button")) {
      expect(button.props["aria-label"]).toBeTruthy();
      expect(textOf(button)).toBe("");
    }
  });

  it("should_FilterAndClear_When_TextIsTypedThenCleared", () => {
    // Arrange
    const onFilter = vi.fn();
    act(() => { tree = create(<TableToolbar path="/rows" arrangement={arrangement({ filter: { query: "yil", matched: 3, of: 24 } })} onFilter={onFilter} />); });
    // Act
    act(() => byLabel("Filter rows").props.onChange({ target: { value: "yılmaz" } }));
    const counted = textOf(tree!.root);
    act(() => byLabel("Clear the filter").props.onClick());
    // Assert
    expect(counted).toContain("3 of 24");
    expect(onFilter.mock.calls).toEqual([["/rows", "yılmaz"], ["/rows", ""]]);
  });

  it("should_HideAndShowColumnsButNeverTheKey_When_TheMenuIsUsed", () => {
    // Arrange
    act(() => { tree = create(<TableToolbar path="" arrangement={arrangement()} onFilter={vi.fn()} />); });
    act(() => byLabel("Columns, 3 of 3 shown").props.onClick());
    const boxes = () => tree!.root.findAllByProps({ type: "checkbox" });
    // Act
    act(() => boxes()[2]!.props.onChange());
    // Assert
    expect(boxes()[0]!.props.disabled).toBe(true);
    expect(tableViewStore.get()).toEqual({ "type:Order": { hidden: ["note"] } });
  });

  it("should_KeepPinWidthAndReset_When_TheTypeIsArranged", () => {
    // Arrange
    act(() => { tree = create(<PinButton arrangement={arrangement()} />); });
    // Act
    act(() => tree!.root.findByType("button").props.onClick());
    resizeColumn(arrangement(), "customer", 12);
    // Assert
    expect(tableViewStore.get()).toEqual({ "type:Order": { unpinned: true, widths: { customer: 12 } } });
    expect(columnsFor(17 + 10 * 7.8, 7.8)).toBe(10);
  });

  it("should_AddOneStep_When_MoreRowsIsPressed", () => {
    // Arrange
    const onShowRows = vi.fn();
    act(() => { tree = create(<MoreRows path="/rows" shown={4} left={96} exact step={20} onShowRows={onShowRows} />); });
    // Act
    act(() => tree!.root.findByType("button").props.onClick());
    // Assert
    expect(textOf(tree!.root)).toBe("+96 rows · show 20 more");
    expect(onShowRows).toHaveBeenCalledExactlyOnceWith("/rows", 24);
  });
});
