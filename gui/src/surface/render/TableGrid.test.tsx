import { describe, expect, it } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { beyond, TableGrid } from "./TableGrid";

const columns = [{ name: "id", key: true, numeric: true }, { name: "customer" }, { name: "total", numeric: true }];
const rows = [
  [[{ text: "10431", role: "mono-literal" as const }], [{ text: "Northwind Ltd", role: "mono-literal" as const }], [{ text: "248.00", role: "mono-literal" as const }]],
  [[{ text: "10432", role: "mono-literal" as const }], [{ text: "Contoso GmbH", role: "mono-literal" as const }], [{ text: "1 940.00", role: "mono-literal" as const }]],
];

function draw(): ReactTestRenderer {
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<TableGrid columns={columns} rows={rows} total={78} />); });
  return tree;
}
const classes = (node: ReactTestInstance) => String(node.props.className).split(" ");

describe("the table grid", () => {
  it("should_DrawAHeaderRowAndOneRowPerEntry_When_CellsAreGiven", () => {
    // Arrange / Act
    const tree = draw();
    const drawn = tree.root.findAllByProps({ role: "row" }).filter((node) => node.type === "tr");
    // Assert
    expect(drawn).toHaveLength(3);
    expect(classes(drawn[0]!)).toContain("value-table-head");
    expect(tree.root.findByProps({ role: "table" }).props["aria-rowcount"]).toBe(78);
    expect(tree.root.findByProps({ role: "table" }).type).toBe("table");
    expect(tree.root.findAllByProps({ role: "columnheader" }).filter((node) => node.type === "th").map((node) => node.children.join(""))).toEqual(["id", "customer", "total"]);
    act(() => tree.unmount());
  });

  it("should_RightAlignNumbersAndGiveTheKeyColumnItsRole_When_TheColumnsSaySo", () => {
    // Arrange / Act
    const tree = draw();
    const cells = tree.root.findAllByProps({ role: "cell" }).filter((node) => node.type === "td");
    // Assert
    expect(cells).toHaveLength(6);
    expect(classes(cells[0]!)).toEqual(expect.arrayContaining(["value-table-cell", "value-table-numeric", "table-key"]));
    expect(cells[0]!.children).toEqual(["10431"]);
    expect(classes(cells[1]!)).not.toContain("table-key");
    expect(cells[1]!.findAllByType("span").map((node) => node.props.className)).toContain("mono-literal");
    expect(classes(cells[2]!)).toContain("value-table-numeric");
    act(() => tree.unmount());
  });
});

describe("a table wider than its block", () => {
  it("should_ScrollInsideItsOwnFrame_And_PinTheKeyColumn_When_Drawn", () => {
    // Arrange / Act
    const tree = draw();
    // Assert: the grid sits in a scrolling box inside a frame; head and body key cells are pinned
    const frame = tree.root.findByProps({ className: "value-table-frame" });
    expect(frame.findByProps({ className: "value-table-scroll" })).toBeDefined();
    const pinned = tree.root.findAll((node) => typeof node.type === "string" && classes(node).includes("value-table-key"));
    expect(pinned).toHaveLength(1 + rows.length);
    expect(pinned[0]!.props.role).toBe("columnheader");
  });

  it("should_NameTheEdgesWithMoreBehindThem_When_TheBoxIsMeasured", () => {
    // Arrange / Act / Assert
    expect(beyond(0, 500, 500)).toBe("");
    expect(beyond(0, 800, 500)).toBe("right");
    expect(beyond(100, 800, 500)).toBe("left right");
    expect(beyond(300, 800, 500)).toBe("left");
    expect(beyond(0, 800, 0)).toBe("");
  });
});
