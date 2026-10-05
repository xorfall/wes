import { describe, expect, it } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { Editor, MAX_LINES } from "./Editor";
import { wordAt } from "./calc-mode";
import { bundledPackage, readLanguage } from "./language";
import { candidateLine, completions, completionSource, signature } from "./calc-complete";
import { lineText } from "./MonoLine";

const language = readLanguage(bundledPackage, "engine");
const names = ["orders", "revenue", "totals"];

const program = [
  ":calc {",
  '  const paid = $orders.filter(o => o.status == "paid");',
  "  if (paid !== none) return paid.count();",
  "  return paid.reduce((t, o) => t + o.total, 0.0);",
  "} > revenue",
].join("\n");

function textOf(node: { children: readonly unknown[] }): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : textOf(child as { children: readonly unknown[] })))
    .join("");
}

function draw(props: Partial<Parameters<typeof Editor>[0]> = {}): ReactTestRenderer {
  let tree: ReactTestRenderer | undefined;
  act(() => { tree = create(<Editor source={program} language={language} {...props} />); });
  return tree!;
}

const linesOf = (tree: ReactTestRenderer, name: string) =>
  tree.root.findAllByType("pre").filter((pre) => String(pre.props.className).includes(name)).map(textOf);

describe("the grown prompt", () => {
  it("should_NumberEveryLineInAGutter_When_TheProgramIsDrawn", () => {
    const tree = draw();
    expect(linesOf(tree, "editor-gutter")).toEqual(["  1", "  2", "  3", "  4", "  5"]);
    act(() => tree.unmount());
  });

  it("should_DrawTheProgramAsItWasWritten_When_ItIsHighlighted", () => {
    const tree = draw();
    expect(linesOf(tree, "editor-line").join("\n")).toBe(program);
    act(() => tree.unmount());
  });

  it("should_StopAtTwelveLines_When_TheProgramIsLonger", () => {
    const long = Array.from({ length: 20 }, (_, at) => `const x${at} = ${at};`).join("\n");
    const tree = draw({ source: long });
    expect(linesOf(tree, "editor-line")).toHaveLength(MAX_LINES);
    act(() => tree.unmount());
  });
});

describe("the mistake row", () => {
  /*
   * Under the block, in the code's own column.
   *
   * A row between two lines would push the drawing away from the field laid over it — the prompt
   * and the editor are one element now — so the row sits below the lines and keeps the column,
   * which is the part that says which token is meant.
   */
  it("should_PointAtTheOffendingTokensColumn_When_TheProgramHasOne", () => {
    const tree = draw();
    expect(linesOf(tree, "editor-carets")).toEqual([`${" ".repeat(11)}^^^`]);
    // Nothing is inserted between the lines: the gutter still counts 1..5 without a gap.
    expect(linesOf(tree, "editor-gutter")).toEqual(["  1", "  2", "  3", "  4", "  5"]);
    act(() => tree.unmount());
  });

  it("should_SayWhatTheLanguageHasInstead_When_ATokenIsWrong", () => {
    const tree = draw();
    expect(linesOf(tree, "editor-said")).toEqual([
      "no !== in this language   inequality is !=, and absence is none — read it with isSome()",
    ]);
    act(() => tree.unmount());
  });

  it("should_DrawNoRow_When_TheProgramHasNoMistake", () => {
    const tree = draw({ source: "acme orders.list" });
    expect(linesOf(tree, "editor-carets")).toEqual([]);
    act(() => tree.unmount());
  });
});

describe("what completion offers", () => {
  it("should_OfferOnlyThePackagesOperationsAndTheWorkspacesNames_When_AskedForCandidates", () => {
    const offered = completions("", language, names, 100);
    const operations = offered.filter((candidate) => candidate.kind === "operation").map((candidate) => candidate.text);
    expect(operations).toEqual(language.operations());
    expect(offered.filter((candidate) => candidate.kind === "name").map((candidate) => candidate.text))
      .toEqual(["$orders", "$revenue", "$totals"]);
    // Nothing JavaScript would offer and the engine would refuse.
    // `length` is the package's own; `toString` and the rest are JavaScript's and are not offered.
    for (const absent of ["toString", "Math", "console", "group_by", "sum", "sort_desc"]) {
      expect(offered.map((candidate) => candidate.text)).not.toContain(absent);
    }
  });

  it("should_CarryTheArityThePackageGives_When_AnOperationIsOffered", () => {
    const offered = completions("filter", language, []);
    expect(offered[0]).toEqual({ text: "filter", label: "filter(fn)", detail: "exactly 2 args", kind: "operation" });
  });

  it("should_ReadAsTheCaptureDoes_When_TheListIsDrawn", () => {
    expect(signature("filter", "filter", { min: 2, max: 2 })).toBe("filter(fn)");
    expect(signature("map", "map", { min: 2, max: 2 })).toBe("map(fn)");
    expect(signature("concat", "concat", { min: 2, max: 2 })).toBe("concat(other)");
    expect(signature("sortBy", "sort-by", { min: 2, max: 2 })).toBe("sortBy(selector)");
    expect(signature("reduce", "reduce", { min: 3, max: 3 })).toBe("reduce(fn, init)");
    expect(signature("take", "take", { min: 2, max: 2 })).toBe("take(n)");
    expect(signature("count", "count", { min: 1, max: 1 })).toBe("count()");
    expect(signature("iter.lines", "iter-lines", { min: 1, max: 1 })).toBe("iter.lines()");
  });

  it("should_FallBackToAPlaceholder_When_NobodyHasNamedThatSemanticsParameters", () => {
    expect(signature("mystery", "not-a-semantics", { min: 3, max: 3 })).toBe("mystery(…, …)");
  });

  it("should_KeepASpaceBeforeTheArity_When_TheSignatureIsLongerThanTheColumn", () => {
    const call = completions("call", language, [])[0]!;
    expect(lineText(candidateLine(call, false))).toBe("  call(provider, capability) · exactly 3 args");
  });

  it("should_LeadTheChosenRowWithACaret_When_TheListIsDrawn", () => {
    const candidate = completions("filter", language, [])[0]!;
    expect(lineText(candidateLine(candidate, true))).toBe("› filter(fn)          · exactly 2 args");
    expect(lineText(candidateLine(candidate, false))).toBe("  filter(fn)          · exactly 2 args");
  });

  it("should_OfferOnlyNames_When_WhatIsTypedStartsWithADollar", () => {
    expect(completions("$or", language, names).map((candidate) => candidate.text)).toEqual(["$orders"]);
  });

  it("should_SayWhereTheCandidatesCameFrom_When_TheListIsDrawn", () => {
    expect(lineText(completionSource(language))).toBe("from the workspace's calc/default.yaml — no JS globals");
    expect(lineText(completionSource(readLanguage(bundledPackage, "bundled"))))
      .toBe("from the workspace's calc/default.yaml — bundled copy, no JS globals");
  });

  it("should_TakeTheWordAtTheCaret_When_CompletingInTheMiddleOfALine", () => {
    expect(wordAt("$orders.fil", 11)).toEqual({ text: "fil", from: 8 });
    expect(wordAt("const paid = fil", 16)).toEqual({ text: "fil", from: 13 });
    expect(wordAt("const paid = ", 13)).toEqual({ text: "", from: 13 });
  });
});
