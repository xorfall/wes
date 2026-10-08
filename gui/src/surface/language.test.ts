import { describe, expect, it, beforeEach } from "vitest";
import { bundledPackage, forgetLanguage, language, readLanguage, type LanguagePackage, type OperationSpec } from "./language";

const fixture: LanguagePackage = {
  language: "calc",
  version: 1,
  lexical: { identifier: "unicode", strings: "quoted", comments: "slash", numbers: "exact" },
  statements: {
    const: "binding", let: "binding-mut", function: "function", if: "if", else: "else",
    while: "while", for: "for", of: "of", return: "return", break: "break", continue: "continue",
  },
  operators: {
    "||": { operation: "or", precedence: 1 },
    "==": { operation: "eq", precedence: 3 },
    "!=": { operation: "ne", precedence: 3 },
    "<": { operation: "lt", precedence: 4 },
  },
  operations: {
    map: { operation: "map", min: 2, max: 2, method: true },
    filter: { operation: "filter", min: 2, max: 2, method: true },
    reduce: { operation: "reduce", min: 3, max: 3, method: true },
    range: { operation: "range", min: 1, max: 3, method: false },
  },
  source: "version: 1\n",
};

function served(body: unknown, ok = true): typeof fetch {
  return (async () => ({ ok, json: async () => body })) as unknown as typeof fetch;
}

describe("the language package", () => {
  beforeEach(forgetLanguage);

  it("should_ReportElevenKeywords_When_ThePackageDeclaresThem", () => {
    const read = readLanguage(fixture, "engine");
    expect(read.keywords()).toHaveLength(11);
    expect(read.keywords()).toContain("const");
    expect(read.keywords()).toContain("continue");
  });

  it("should_ReportArityOfThree_When_AskedForReduce", () => {
    expect(readLanguage(fixture, "engine").arity("reduce")).toEqual({ min: 3, max: 3 });
  });

  it("should_ReportARange_When_AnOperationAcceptsSeveralArities", () => {
    expect(readLanguage(fixture, "engine").arity("range")).toEqual({ min: 1, max: 3 });
  });

  it("should_ReportNothing_When_ThePackageDoesNotNameTheOperation", () => {
    expect(readLanguage(fixture, "engine").arity("group_by")).toBeUndefined();
  });

  it("should_AllowAMethodOnlyWhereThePackageDeclaresOne_When_AskedAboutReceivers", () => {
    const read = readLanguage(fixture, "engine");
    expect(read.method("filter")).toBe(true);
    expect(read.method("range")).toBe(false);
    expect(read.method("group_by")).toBe(false);
    expect(read.method("toString")).toBe(false);
    // A served spec without the declaration is function-only; nothing infers it from the name.
    const undeclared = { operation: "filter", min: 2, max: 2 } as unknown as OperationSpec;
    const older = readLanguage({ ...fixture, operations: { ...fixture.operations, filter: undeclared } }, "engine");
    expect(older.method("filter")).toBe(false);
    expect(older.arity("filter")).toEqual({ min: 2, max: 2 });
  });

  it("should_OfferLongerOperatorsFirst_When_ListingOperators", () => {
    const operators = readLanguage(fixture, "engine").operators();
    expect(operators.indexOf("!=")).toBeLessThan(operators.indexOf("<"));
  });

  it("should_ReadTheEnginesPackage_When_TheRouteAnswers", async () => {
    const read = await language(served(fixture));
    expect(read.origin).toBe("engine");
    expect(read.arity("reduce")).toEqual({ min: 3, max: 3 });
  });

  it("should_AskOnce_When_SeveralCallersWantThePackage", async () => {
    let asked = 0;
    const counting = (async () => { asked += 1; return { ok: true, json: async () => fixture }; }) as unknown as typeof fetch;
    await Promise.all([language(counting), language(counting)]);
    await language(counting);
    expect(asked).toBe(1);
  });

  it("should_FallBackToTheBundledPackage_When_TheEngineHasNoRoute", async () => {
    const read = await language(served("not found", false));
    expect(read.origin).toBe("bundled");
    expect(read.keywords()).toHaveLength(11);
    expect(read.arity("reduce")).toEqual({ min: 3, max: 3 });
  });

  it("should_FallBackToTheBundledPackage_When_TheRouteAnswersSomethingElse", async () => {
    const read = await language(served({ hello: "world" }));
    expect(read.origin).toBe("bundled");
  });

  it("should_FallBackToTheBundledPackage_When_TheRequestFails", async () => {
    const refused = (async () => { throw new Error("offline"); }) as unknown as typeof fetch;
    expect((await language(refused)).origin).toBe("bundled");
  });

  it("should_NameOnlyWhatTheEngineAccepts_When_ReadingTheBundledPackage", () => {
    const read = readLanguage(bundledPackage, "bundled");
    for (const absent of ["group_by", "sort_desc", "sum"]) expect(read.arity(absent)).toBeUndefined();
    expect(read.operators()).not.toContain("|");
    expect(read.arity("filter")).toEqual({ min: 2, max: 2 });
  });
});
