import { afterEach, describe, expect, it, vi } from "vitest";
import { restoreSettings } from "../settings";
import { clampWidth, restoreTableViews, tableKey, tableViewStore, withTableView } from "./table-views";

afterEach(() => { tableViewStore.keepWith(undefined); tableViewStore.seed({}); });

describe("table arrangements as preferences", () => {
  it("should_KeepOnlyBoundedValidEntries_When_StoredArrangementsAreRead", () => {
    // Arrange
    const stored = { "type:Order": { hidden: ["note", "note", 3, ""], widths: { customer: 9000, total: -4, bad: "x" }, unpinned: true, extra: 1 }, "type:Empty": { hidden: [] }, "": { unpinned: true } };
    // Act
    const views = restoreTableViews(stored);
    // Assert
    expect(views).toEqual({ "type:Order": { hidden: ["note"], widths: { customer: 400, total: 3 }, unpinned: true } });
    expect(restoreSettings({ tables: stored }).tables).toEqual(views);
    expect(restoreSettings({}).tables).toEqual({});
  });

  it("should_ForgetAnArrangement_When_ItIsBackToTheDefaults", () => {
    // Act
    const views = withTableView({ "type:Order": { unpinned: true } }, "type:Order", { hidden: [], unpinned: undefined });
    // Assert
    expect(views).toEqual({});
    expect(clampWidth(2.4)).toBe(3);
  });

  it("should_NameATableByItsRowTypeElseItsColumns_When_KeyingAnArrangement", () => {
    expect(tableKey({ kind: "record", name: "Order", fields: [] }, ["b", "a"])).toBe("type:Order");
    expect(tableKey({ kind: "unknown" }, ["b", "a"])).toBe("columns:a,b");
  });

  it("should_PublishAndKeep_When_ATableIsArranged", () => {
    // Arrange
    const kept = vi.fn();
    const heard = vi.fn();
    tableViewStore.keepWith(kept);
    const stop = tableViewStore.subscribe(heard);
    // Act
    tableViewStore.change("type:Order", { hidden: ["note"] });
    stop();
    // Assert
    expect(tableViewStore.get()).toEqual({ "type:Order": { hidden: ["note"] } });
    expect(kept).toHaveBeenCalledExactlyOnceWith({ "type:Order": { hidden: ["note"] } });
    expect(heard).toHaveBeenCalledOnce();
  });
});
