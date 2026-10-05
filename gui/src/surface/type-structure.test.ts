import { describe, expect, it } from "vitest";
import type { TypeShape } from "../protocol";
import { typeStructure } from "./type-structure";

const INT: TypeShape = { kind: "primitive", name: "INT" };
const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };

describe("a type written out whole", () => {
  it("should_OpenEveryRecordFieldByField_When_RecordsNestUnderListsAndOptions", () => {
    // Arrange
    const shape: TypeShape = {
      kind: "list",
      element: { kind: "record", name: "", fields: [
        { name: "id", type: INT },
        { name: "rows", type: { kind: "option", element: { kind: "list", element: { kind: "record", name: "Row", fields: [{ name: "name", type: TEXT }] } } } },
      ] },
    };
    // Act / Assert
    expect(typeStructure(shape)).toBe([
      "List<{",
      "  id: Int",
      "  rows: Option<List<Row {",
      "    name: Text",
      "  }>>",
      "}>",
    ].join("\n"));
  });

  it("should_SayTheShortForms_When_ThereIsNothingToOpen", () => {
    expect(typeStructure({ kind: "record", name: "Empty", fields: [] })).toBe("Empty {}");
    expect(typeStructure({ kind: "iter", element: TEXT })).toBe("Iter<Text>");
    expect(typeStructure(undefined)).toBe("Unknown");
  });
});
