import { expect, it } from "vitest";
import { decodeContract, type ViewDefinition } from "./definition";
import { definition } from "../../../views/metric/contract";

function boundedText(minLength: number | null, maxLength: number | null) {
  const tested: ViewDefinition = {
    ...definition,
    contracts: {
      ...definition.contracts,
      BoundedText: {
        kind: "scalar",
        primitive: "Text",
        constraints: {
          min: null, max: null, minLength, maxLength,
          minItems: null, maxItems: null, patterns: [], enum: [],
        },
      },
    },
  };
  return (text: string, wire = false) => decodeContract(
    tested, "BoundedText", text, wire,
    wire ? { kind: "primitive", name: "TEXT" } : undefined,
  );
}

it("matches Wes Text code-point lengths for wire inputs and interaction values", () => {
  const one = boundedText(1, 1);
  const two = boundedText(2, 2);
  for (const wire of [false, true]) {
    for (const text of ["a", "é", "😀"])
      expect(one(text, wire)).toBe(text);
    for (const text of ["", "ab", "e\u0301", "😀a"])
      expect(() => one(text, wire)).toThrow(/BoundedText/);
    for (const text of ["e\u0301", "😀a", "😀😀"])
      expect(two(text, wire)).toBe(text);
    expect(() => two("😀", wire)).toThrow(/BoundedText/);
  }
  const title = boundedText(1, 128);
  expect(title("😀".repeat(128))).toBe("😀".repeat(128));
  expect(() => title("😀".repeat(129))).toThrow(/BoundedText/);
});

it("retains conservative string memory charging independently of code-point lengths", () => {
  const text = boundedText(null, null);
  expect(() => text("😀".repeat(2_097_153))).toThrow();
});
