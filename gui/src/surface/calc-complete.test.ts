import { describe, expect, it } from "vitest";
import { completions, completionsAt, signature } from "./calc-complete";
import { highlightCalc } from "./calc-highlight";
import { bundledPackage, readLanguage, type LanguagePackage, type OperationSpec } from "./language";

/*
 * Synthetic operation tables over the bundled grammar. Method permission comes only from each spec's
 * `method`, so nothing here depends on whether the bundled copy has been regenerated yet.
 */
const base: Record<string, OperationSpec> = {
  filter: { operation: "filter", min: 2, max: 2, method: true },
  take: { operation: "take", min: 2, max: 2, method: true },
  first: { operation: "take", min: 2, max: 2, method: true },
  text: { operation: "text", min: 1, max: 1, method: false },
  "iter.lines": { operation: "iter-lines", min: 1, max: 1, method: false },
  "iter.regexSplit": { operation: "iter-regex-split", min: 2, max: 2, method: false },
};
const pack = (operations: Record<string, OperationSpec>) =>
  readLanguage({ ...bundledPackage, operations } as LanguagePackage, "engine");
const older = pack(base);
const language = pack({
  ...base,
  regexTest: { operation: "regex-test", min: 2, max: 2, method: false },
  stripAnsi: { operation: "strip-ansi", min: 1, max: 1, method: false },
});
const names = ["raw", "status"];
const at = (source: string, read = language) => completionsAt(source, source.length, read, names, 100);
const texts = (source: string, read = language) => at(source, read).candidates.map((it) => it.text);

describe("text operations in the editor", () => {
  it("should_OfferThemAsFunctionsWithPlaceholders_When_ThePackageDeclaresThem", () => {
    expect(completions("regex", language, names)).toContainEqual(
      { text: "regexTest", label: "regexTest(…, …)", detail: "exactly 2 args", kind: "operation" });
    expect(completions("strip", language, names)).toContainEqual(
      { text: "stripAnsi", label: "stripAnsi(…)", detail: "exactly 1 arg", kind: "operation" });
  });

  it("should_NotOfferThem_When_ThePackageDoesNot", () => {
    expect(completions("regex", older, names).map((it) => it.text)).not.toContain("regexTest");
    expect(texts(":calc { return strip", older)).not.toContain("stripAnsi");
  });

  it("should_OfferOnlyDeclaredMethodsAfterAReceiver_When_TheyAreFunctionSyntaxOnly", () => {
    expect(texts(":calc { return $raw.str")).toEqual([]);
    expect(texts(":calc { const line = 'x'; return line.reg")).toEqual([]);
    // The result's `text` is a field to read, not the `text` conversion.
    expect(texts(":calc { return stripAnsi($raw).te")).toEqual([]);
    expect(texts(":calc { return [$raw][0].str")).toEqual([]);
    // Declared methods, aliases included, are offered after a receiver, without names or functions.
    expect(texts(":calc { return stripAnsi($raw).spans.fil")).toEqual(["filter"]);
    expect(texts(":calc { return $raw.fi")).toEqual(["filter", "first"]);
    expect(at(":calc { return $raw.").candidates.every((it) => it.kind === "operation" && language.method(it.text))).toBe(true);
  });

  it("should_TreatAMissingDeclarationAsFunctionOnly_When_ASpecOmitsMethod", () => {
    const undeclared = { operation: "take", min: 2, max: 2 } as unknown as OperationSpec;
    const read = pack({ ...base, take: undeclared });
    expect(texts(":calc { return $raw.ta", read)).toEqual([]);
    expect(completions("take", read, [])[0]!.label).toBe("take(…, n)");
  });

  it("should_OfferThemByNameAndWorkspaceNamesByDollar_When_TypedAtTheStartOfAnExpression", () => {
    const source = ":calc { return regexTest($raw, '^err') || reg";
    expect(at(source).from).toBe(source.length - 3);
    // Package order: the namespaced member answers to its last part as well.
    expect(texts(source)).toEqual(["iter.regexSplit", "regexTest"]);
    expect(texts(":calc { return stripAnsi($r")).toEqual(["$raw"]);
  });

  it("should_OfferNothing_When_TheCaretIsInsideAnOpenStringOrComment", () => {
    expect(texts(":calc { return regexTest($raw, 'strip")).toEqual([]);
    expect(texts(':calc { return regexTest($raw, "^reg')).toEqual([]);
    expect(texts(":calc { return regexTest($raw, '$ra")).toEqual([]);
    expect(texts(":calc { // strip")).toEqual([]);
    expect(texts(":calc { /* reg")).toEqual([]);
  });

  it("should_OfferCode_When_AStringOrCommentClosedBeforeTheCaret", () => {
    expect(texts(":calc { const p = 'x'; /* c */ return reg")).toContain("regexTest");
    expect(texts(":calc { const p = 'it\\'s'; return reg")).toContain("regexTest");
    expect(texts(":calc { const p = 'x'+reg")).toContain("regexTest");
    expect(texts(":calc { /* c */reg")).toContain("regexTest");
    expect(texts(":calc { // note\n  return reg")).toContain("regexTest");
  });

  it("should_OfferOnlyTheNamespacesMembersWithoutRepeatingIt_When_AfterIter", () => {
    const source = ":calc { return iter.li";
    expect(at(source)).toEqual({
      from: source.length - 2,
      candidates: [{ text: "lines", label: "iter.lines(…)", detail: "exactly 1 arg", kind: "operation" }],
    });
    expect(texts(":calc { return iter.reg")).toEqual(["regexSplit"]);
    expect(texts(":calc { return iter.str")).toEqual([]);
  });
});

describe("the signature label", () => {
  it("should_NotChangeTheParameterTable_When_LabelsAreBuiltRepeatedly", () => {
    // `take` names one parameter; asking for more places than that must not grow the shared table.
    for (let round = 0; round < 3; round += 1) {
      expect(signature("take", "take", { min: 2, max: 4 }, true)).toBe("take(n, …, …)");
      expect(signature("take", "take", { min: 2, max: 4 }, false)).toBe("take(…, …, …, n)");
    }
    expect(signature("take", "take", { min: 2, max: 2 }, true)).toBe("take(n)");
  });
});

describe("colouring after a dot", () => {
  const roleAfterDot = (source: string, word: string) => {
    const span = highlightCalc(source, language).spans.find((it) => source.slice(it.from, it.to) === word && source[it.from - 1] === ".");
    return span?.role;
  };
  it("should_ColourOnlyDeclaredMethodsAsOperations_When_TheyFollowAReceiver", () => {
    expect(roleAfterDot(":calc { return stripAnsi($raw).text; }", "text")).toBe("mono-param");
    expect(roleAfterDot(":calc { return $raw.regexTest('x'); }", "regexTest")).toBe("mono-param");
    expect(roleAfterDot(":calc { return $raw.filter(x => x); }", "filter")).toBe("mono-provider");
    expect(roleAfterDot(":calc { return iter.lines($raw); }", "lines")).toBe("mono-provider");
  });
});
