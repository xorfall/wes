import { describe, expect, it } from "vitest";
import { bundledPackage, readLanguage } from "./language";
import { completions } from "./calc-complete";
import { lineText } from "./MonoLine";
import { gutterMark, indentAt, INDENT, lineAt, MODE_ROLES, pairAt, readMode, wordAt } from "./calc-mode";
import { promptCompletion } from "./prompt-complete";
import { emptyAliases } from "../aliases";
import { emptyCatalogue } from "../vocabulary";

/** The package the engine serves, read as the client reads it. Nothing here names a word itself. */
const language = readLanguage(bundledPackage, "engine");
const names = ["orders", "revenue"];

/** Representative nested program for editor highlighting and indentation checks. */
const program = [
  ":calc {",
  '  const paid = $orders.filter(o => o.status == "paid");',
  "  if (paid !== none) return paid.count();",
  "  return paid.reduce((t, o) => t + o.total, 0.0);",
  "} > revenue",
].join("\n");

const mode = readMode(program, language);
/** The role a source offset takes, which is what a decoration would paint there. */
const roleAt = (source: string, text: string, from = 0) => {
  const at = source.indexOf(text, from);
  const read = readMode(source, language);
  return read.spans.find((span) => span.from <= at && at < span.to)?.role;
};

describe("the mode generated from the language package", () => {
  /*
   * A word is a keyword because the language package declares it.
   *
   * The eleven are not listed here — they are read back out of the package — so a package that
   * gains a twelfth colours it without anybody editing this, and one that drops a word stops
   * colouring it. That is the whole point of generating the mode.
   */
  it("should_ColourEveryKeywordThePackageDeclares_When_TheModeIsGenerated", () => {
    const keywords = language.keywords();
    expect(keywords).toHaveLength(11);
    for (const keyword of keywords) {
      expect(roleAt(`:calc { ${keyword} x; }`, keyword, 7), keyword).toBe("mono-meta");
    }
  });

  it("should_LeaveJavaScriptsOwnWordsAsPlainNames_When_TheyAreNotThePackages", () => {
    // Colouring these would be a lie about what the engine accepts, and a lie that only shows up
    // when the command is run.
    for (const word of ["switch", "class", "new", "typeof", "async", "null", "undefined", "NaN"]) {
      expect(roleAt(`:calc { ${word} x; }`, word, 7), word).toBe("mono-ink");
    }
  });

  it("should_NeverCallItValid_When_TheSourceUsesSomethingTheLanguageLacks", () => {
    for (const absent of ["!==", "===", "%", "**", "??", "++", "+="]) {
      const source = `:calc { a ${absent} b; }`;
      const read = readMode(source, language);
      expect(roleAt(source, absent, 7), absent).toBe("mono-bad");
      expect(read.diagnostics.map((it) => it.message), absent).toContain(`no ${absent} in this language`);
    }
  });

  it("should_SayWhatTheLanguageHasInstead_When_ItRefusesAToken", () => {
    const wrong = readMode(program, language).diagnostics;
    expect(wrong).toHaveLength(1);
    expect(wrong[0]!.message).toBe("no !== in this language");
    // The token itself, because it is how wide the caret row under the block is.
    expect(wrong[0]!.text).toBe("!==");
    expect(lineText(wrong[0]!.hint))
      .toBe("inequality is !=, and absence is none — read it with isSome()");
  });

  it("should_MakeAReferenceItsOwnToken_When_ADollarLeadsIt", () => {
    expect(roleAt(program, "$orders")).toBe("mono-ref");
    // The name after `>` is the result being named, and reads as a reference too.
    expect(roleAt(program, "revenue", program.indexOf("} >"))).toBe("mono-ref");
    // A bare word is not a reference, and an operation is not a name.
    expect(roleAt(program, "paid")).toBe("mono-ink");
    expect(roleAt(program, "filter")).toBe("mono-provider");
    // A lambda's parameters are scaffolding, and a field is a field.
    expect(roleAt(program, "status")).toBe("mono-param");
  });

  it("should_CarryEveryRoleItsThemeMustAnswerFor_When_TheRolesAreListed", () => {
    const used = new Set(mode.spans.map((span) => span.role).filter((role) => role !== undefined));
    for (const role of used) expect(MODE_ROLES, role).toContain(role);
  });
});

describe("the diagnostic the editor shows", () => {
  it("should_PointAtTheTokensOwnColumn_When_ASourceHasAMistake", () => {
    const wrong = mode.diagnostics[0]!;
    const line = program.split("\n")[wrong.line]!;
    expect(wrong.line).toBe(2);
    expect(line.slice(wrong.column, wrong.column + (wrong.to - wrong.from))).toBe("!==");
    expect(program.slice(wrong.from, wrong.to)).toBe("!==");
  });

  it("should_MarkTheLineInTheGutter_When_ThatLineCarriesOne", () => {
    expect(mode.marked).toEqual([2]);
    expect(lineText(gutterMark(2, mode.marked))).toBe("  3 ●");
    expect(lineText(gutterMark(1, mode.marked))).toBe("  2  ");
    // The mark is the bad role, because it is the one thing on the line that is wrong.
    expect(gutterMark(2, mode.marked)[2]?.role).toBe("mono-bad");
  });

  it("should_SayNothing_When_TheSourceIsWhatTheLanguageAccepts", () => {
    const fine = readMode(':calc { return $orders.count(); } > total', language);
    expect(fine.diagnostics).toEqual([]);
    expect(fine.marked).toEqual([]);
  });

  it("should_CountLinesFromZero_When_AnOffsetIsAsked", () => {
    expect(lineAt(program, 0)).toBe(0);
    expect(lineAt(program, program.indexOf("!=="))).toBe(2);
    expect(lineAt(program, program.length)).toBe(4);
  });
});

describe("the bracket the caret is beside", () => {
  const source = ':calc { return f(g(1), "(", 2); }';

  it("should_FindThePartner_When_TheCaretIsOnEitherSideOfABracket", () => {
    const read = readMode(source, language);
    const open = source.indexOf("f(") + 1;
    const close = source.indexOf(");");
    // Before the opening bracket, and after the closing one: both are "beside" it.
    expect(pairAt(source, open, read)).toEqual({ open, close });
    expect(pairAt(source, close + 1, read)).toEqual({ open, close });
  });

  it("should_CountNesting_When_ThereIsABracketInBetween", () => {
    const read = readMode(source, language);
    const inner = source.indexOf("g(") + 1;
    expect(pairAt(source, inner, read)).toEqual({ open: inner, close: source.indexOf("1)") + 1 });
  });

  /** A bracket in a string is text: the tokeniser kept it inside one run, so it is never a pair. */
  it("should_IgnoreABracketInsideAString_When_ItLooksForThePartner", () => {
    const read = readMode(source, language);
    expect(pairAt(source, source.indexOf('"(') + 1, read)).toBeUndefined();
    // And the braces of the program itself still match across all of it.
    expect(pairAt(source, source.indexOf("{"), read)).toEqual({ open: 6, close: source.lastIndexOf("}") });
  });

  it("should_FindNothing_When_TheCaretIsNotBesideOne", () => {
    const read = readMode(source, language);
    expect(pairAt(source, source.indexOf("return") + 2, read)).toBeUndefined();
    expect(pairAt(source, 0, read)).toBeUndefined();
  });
});

describe("where a new line starts", () => {
  it("should_IndentByWhatIsStillOpen_When_ALineIsBegun", () => {
    const source = ":calc {\n";
    expect(indentAt(source, source.length, readMode(source, language))).toBe(INDENT);
    const deeper = ":calc {\n  if (x) {\n";
    expect(indentAt(deeper, deeper.length, readMode(deeper, language))).toBe(INDENT * 2);
  });

  it("should_DedentTheLineThatClosesOne_When_ItBeginsWithABracket", () => {
    const source = ":calc {\n  const x = 1;\n}";
    expect(indentAt(source, source.length, readMode(source, language))).toBe(0);
  });

  it("should_StayAtTheTop_When_NothingIsOpen", () => {
    const source = ":calc { return 1; }\n";
    expect(indentAt(source, source.length, readMode(source, language))).toBe(0);
  });

  it("should_MatchTheProgramTheCaptureDraws_When_EachLineIsAsked", () => {
    // The nested program indents its body one level and outdents its closing brace.
    const upto = (line: number) => program.split("\n").slice(0, line).join("\n") + "\n";
    expect(indentAt(upto(1), upto(1).length, readMode(upto(1), language))).toBe(INDENT);
    expect(indentAt(program, program.lastIndexOf("}"), readMode(program, language))).toBe(0);
  });
});

/*
 * One completion source, asked twice.
 *
 * The prompt and the editor offer from the same package and the same workspace names; two sources
 * would drift, and the drift would be an operation the prompt offers and the editor does not.
 */
describe("what the editor completes from", () => {
  it("should_OfferWhatThePromptOffers_When_TheSameWordIsBeingTyped", () => {
    const fromEditor = completions("fil", language, names).map((it) => it.text);
    const line = ":calc { $orders.fil";
    const fromPrompt = promptCompletion({
      line, caret: line.length, catalogue: { ...emptyCatalogue, calculation: { keywords: language.keywords(), operations: language.operations() } },
      names, aliases: emptyAliases,
    });
    expect(fromEditor).toContain("filter");
    expect(fromPrompt.items.map((it) => it.text)).toEqual(fromEditor);
  });

  it("should_CarryTheArityThePackageGives_When_AnOperationIsOffered", () => {
    expect(completions("reduce", language, [])[0])
      .toEqual({ text: "reduce", label: "reduce(fn, init)", detail: "exactly 3 args", kind: "operation" });
  });

  /** A dot ends the word: `paid.re` is completing `re`, or nothing is ever offered after a dot. */
  it("should_TakeTheWordAfterTheDot_When_AnOperationIsBeingTyped", () => {
    expect(wordAt("return paid.re", 14)).toEqual({ text: "re", from: 12 });
    expect(wordAt("$orders.fil", 11)).toEqual({ text: "fil", from: 8 });
    expect(wordAt("const paid = $or", 16)).toEqual({ text: "$or", from: 13 });
    expect(wordAt("return paid.", 12)).toEqual({ text: "", from: 12 });
    expect(completions(wordAt("return paid.re", 14).text, language, names).map((it) => it.text))
      .toContain("reduce");
  });

  it("should_OfferOnlyTheWorkspacesNames_When_TheWordStartsWithADollar", () => {
    expect(completions("$", language, names).map((it) => it.text)).toEqual(["$orders", "$revenue"]);
  });
});
