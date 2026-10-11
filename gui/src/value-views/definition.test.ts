import { expect, it } from "vitest";
import { decodeContract, type ViewDefinition } from "./definition";
import { definition } from "../../../views/metric/contract";
import type { TypeShape } from "../protocol";

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

it("validates present optional fields omitted from a native contract shape", () => {
  const constraints = definition.contracts[definition.input]!.constraints;
  const tested: ViewDefinition = {
    ...definition,
    contracts: {
      ...definition.contracts,
      OptionalInput: { kind: "record", fields: {
        title: { type: "Text", optional: false },
        hint: { type: "Hint", optional: true },
        note: { type: "Note", optional: true },
      }, constraints },
      Text: { kind: "scalar", primitive: "Text", constraints },
      Hint: { kind: "scalar", primitive: "Text", constraints: { ...constraints, enum: ["compact"] } },
      Note: { kind: "option", element: "Text", constraints },
    },
  };
  const type: TypeShape = { kind: "record", name: "OptionalInput", fields: [
    { name: "title", type: { kind: "primitive", name: "TEXT" } },
  ] };
  const read = (data: unknown, shape = type) => decodeContract(tested, "OptionalInput", data, true, shape);
  expect(read({ title: "Synthetic" })).toEqual({ title: "Synthetic" });
  expect(read({ title: "Synthetic", hint: "compact", note: { kind: "some", value: "Details" } }))
    .toEqual({ title: "Synthetic", hint: "compact", note: "Details" });
  for (const data of [
    { title: "Synthetic", hint: "other" },
    { title: "Synthetic", hint: 1 },
    { title: "Synthetic", note: "untagged" },
    { hint: "compact" },
  ]) expect(() => read(data)).toThrow();
  expect(() => read({ title: "Synthetic" }, { ...type, fields: [] })).toThrow();
  for (const child of [{ kind: "primitive", name: "BYTES" }, { kind: "unknown" }] as const) {
    expect(() => read({ title: "Synthetic", hint: "compact" }, {
      ...type, fields: [...type.fields, { name: "hint", type: child }],
    })).toThrow();
  }
});
