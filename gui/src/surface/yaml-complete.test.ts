import { describe, expect, it } from "vitest";
import { completionsAt as complete } from "./yaml-complete";
import type { TypeVocabulary } from "./yaml-highlight";

import { schema } from "./yaml-schema-fixture.test-support";
const completionsAt = (source: string, caret: number, vocabulary: TypeVocabulary) => complete(source, caret, vocabulary, 50, "types", schema);

const vocabulary: TypeVocabulary = { names: ["Text", "Int", "Decimal", "Record", "MonitorHealth", "MonitorRequest"] };

describe("completing a type expression", () => {
  it("should_OfferTheWorkspacesLoadedTypes_When_TheCaretIsInsideAFieldsShorthandValue", () => {
    const source = "types:\n  T:\n    fields: {a: Mo";
    const candidates = completionsAt(source, source.length, vocabulary);
    expect(candidates.map((c) => c.text)).toEqual(["MonitorHealth", "MonitorRequest"]);
    expect(candidates.every((c) => c.kind === "type")).toBe(true);
  });

  it("should_OfferTheWorkspacesLoadedTypes_When_TheCaretIsInsideAnIteratorInput", () => {
    const source = "iterators:\n  V:\n    input: Mon";
    expect(completionsAt(source, source.length, vocabulary).map((c) => c.text)).toEqual([
      "MonitorHealth",
      "MonitorRequest",
    ]);
  });

  it("should_OfferConstructorsToo_When_TheyMatchWhatIsTyped", () => {
    const source = "types:\n  T:\n    base: Li";
    const candidates = completionsAt(source, source.length, vocabulary);
    expect(candidates.map((c) => c.text)).toEqual(["List"]);
    expect(candidates[0]!.label).toBe("List<T>");
  });

  it("should_OfferNothing_When_TheCaretIsNotInAnyExpectedPosition", () => {
    const source = "types:\n  T:\n    fields: {a: Text}, comment: he";
    expect(completionsAt(source, source.length, vocabulary)).toEqual([]);
  });
});

describe("completing an iterator mode", () => {
  it("should_OfferOnlyTheEnginesKnownKinds_When_TheCaretIsInsideIteratorMode", () => {
    const source = "iterators:\n  V:\n    mode: li";
    const candidates = completionsAt(source, source.length, vocabulary);
    expect(candidates.map((c) => c.text)).toEqual(["lines"]);
    expect(candidates[0]!.kind).toBe("keyword");
  });
});
