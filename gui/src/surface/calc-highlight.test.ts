import { describe, expect, it } from "vitest";
import { bundledPackage, readLanguage } from "./language";
import { highlightCalc, mistakeRows } from "./calc-highlight";
import { lineText, type MonoRole } from "./MonoLine";

const language = readLanguage(bundledPackage, "engine");

const roles = (source: string): Record<string, MonoRole | undefined> => {
  const found: Record<string, MonoRole | undefined> = {};
  for (const line of highlightCalc(source, language).lines) {
    for (const segment of line) found[segment.text] = segment.role;
  }
  return found;
};
const roleOf = (source: string, text: string) => roles(source)[text];
const mistakesIn = (source: string) => highlightCalc(source, language).mistakes;

describe("what the package says is a keyword", () => {
  it("should_ColourTheElevenKeywords_When_TheyAreTheLanguages", () => {
    for (const keyword of ["const", "let", "function", "if", "else", "while", "for", "of", "return", "break", "continue"]) {
      expect(roleOf(`${keyword} x`, keyword), keyword).toBe("mono-meta");
    }
  });

  it("should_NeverClaimAJavaScriptKeywordIsOne_When_ThePackageDoesNotHaveIt", () => {
    // Colouring these as keywords would be a lie about what the engine accepts.
    for (const word of ["switch", "try", "class", "new", "typeof", "async", "await", "var", "do", "throw"]) {
      expect(roleOf(`${word} x`, word), word).toBe("mono-ink");
    }
  });

  it("should_NeverClaimNullIsAValue_When_AbsenceIsNone", () => {
    for (const word of ["null", "undefined", "NaN"]) {
      expect(roleOf(`const x = ${word};`, word), word).toBe("mono-ink");
    }
    expect(roleOf("const x = none;", "none")).toBe("mono-meta");
  });
});

describe("what the package says is an operator", () => {
  it("should_ColourTheOperatorsThePackageHas_When_TheyAreWritten", () => {
    for (const operator of ["==", "!=", "<=", ">=", "&&", "||", "+", "-", "*", "/"]) {
      expect(roleOf(`a ${operator} b`, operator), operator).toBe("mono-faint");
    }
  });

  it("should_MarkAnOperatorTheLanguageHasNot_When_ItLooksLikeJavaScript", () => {
    for (const operator of ["===", "!==", "%", "**", "??", "?.", "++", "+=", "&", "|", "^", "~"]) {
      expect(roleOf(`a ${operator} b`, operator), operator).toBe("mono-bad");
    }
  });

  it("should_SayWhatTheLanguageHasInstead_When_AnOperatorIsWrong", () => {
    const mistake = mistakesIn("  if (paid !== none) return paid.count();")[0]!;
    expect(mistake.text).toBe("!==");
    expect(mistake.said).toBe("no !== in this language");
    expect(lineText([...mistake.hint])).toBe("inequality is !=, and absence is none — read it with isSome()");
  });

  it("should_PointAtTheRemainderOperation_When_PercentIsWritten", () => {
    expect(lineText([...mistakesIn("a % b")[0]!.hint])).toBe("the remainder is rem(a, b)");
  });

  it("should_MarkTheTernary_When_ItIsWritten", () => {
    const marks = mistakesIn("const x = a ? b : c;");
    expect(marks.map((mistake) => mistake.said)).toContain("no ternary in this language");
  });

  it("should_TakeTheLongestOperatorFirst_When_TheyShareCharacters", () => {
    const line = highlightCalc("a != b", language).lines[0]!;
    expect(line.map((segment) => segment.text)).toEqual(["a", " ", "!=", " ", "b"]);
  });
});

describe("what the package says is a literal", () => {
  it("should_ColourAQuotedStringAndANumber_When_TheyAreWritten", () => {
    expect(roleOf('const x = "paid";', '"paid"')).toBe("mono-literal");
    expect(roleOf("const x = 0.0;", "0.0")).toBe("mono-literal");
  });

  it("should_NeverClaimATemplateLiteralIsOne_When_ItIsWritten", () => {
    const source = "const x = `hello`;";
    expect(roleOf(source, "`hello`")).toBe("mono-bad");
    expect(mistakesIn(source)[0]!.said).toBe("no template literals in this language");
    expect(lineText([...mistakesIn(source)[0]!.hint])).toBe('strings are quoted, and only quoted: "like this"');
  });

  it("should_SayAStringIsNeverClosed_When_ItIsNot", () => {
    expect(mistakesIn('const x = "open;')[0]!.said).toBe("this string is never closed");
  });
});

describe("comments", () => {
  it("should_QuietALineAndABlockComment_When_TheyAreClosed", () => {
    expect(roleOf("// a note\nconst x = 1;", "// a note")).toBe("mono-faint");
    expect(roleOf("/* a note */ const x = 1;", "/* a note */")).toBe("mono-faint");
  });

  it("should_MakeAnUnclosedBlockCommentARealDiagnostic_When_ItIsNeverClosed", () => {
    const mistake = mistakesIn("const x = 1; /* and then")[0]!;
    expect(mistake.said).toBe("this block comment is never closed");
    expect(lineText([...mistake.hint])).toBe("close it with */");
  });
});

describe("references, operations and fields", () => {
  it("should_ReadAReferenceAsItsOwnToken_When_ItStartsWithADollar", () => {
    expect(roleOf("$orders.count();", "$orders")).toBe("mono-ref");
  });

  it("should_ColourAnOperationTheEngineHas_When_ItIsCalledMethodStyle", () => {
    expect(roleOf("$orders.filter(o => o.status == \"paid\");", "filter")).toBe("mono-provider");
    expect(roleOf("$orders.count();", "count")).toBe("mono-provider");
  });

  it("should_ColourAFieldAsAParameter_When_ThePackageDoesNotNameIt", () => {
    expect(roleOf('$orders.filter(o => o.status == "paid");', "status")).toBe("mono-param");
  });

  it("should_ColourANamespacedOperation_When_ItIsOneOfTheIterOnes", () => {
    expect(roleOf("$text.iter.lines();", "lines")).toBe("mono-provider");
  });

  it("should_QuietALambdasParameters_When_TheyAreScaffolding", () => {
    const line = highlightCalc("$rows.reduce((t, o) => t + o.total, 0.0);", language).lines[0]!;
    const byText = Object.fromEntries(line.map((segment) => [segment.text, segment.role]));
    expect(byText["t"]).toBe("mono-faint");
    expect(byText["o"]).toBe("mono-faint");
    expect(byText["total"]).toBe("mono-param");
  });
});

describe("arity, counted while typing", () => {
  it("should_SayNothing_When_AnOperationHasTheArgumentsItTakes", () => {
    // Method style spends the receiver: filter takes two, and this gives it two.
    expect(mistakesIn('$orders.filter(o => o.status == "paid");')).toEqual([]);
    expect(mistakesIn("$orders.count();")).toEqual([]);
    expect(mistakesIn("$rows.reduce((t, o) => t + o.total, 0.0);")).toEqual([]);
  });

  it("should_SayWhatThePackageAllows_When_AnOperationHasTooFew", () => {
    const mistake = mistakesIn("$rows.reduce(fn);")[0]!;
    expect(mistake.said).toBe("reduce takes exactly 3 args, not 2");
    expect(lineText([...mistake.hint])).toBe("the package says exactly 3 args");
  });

  it("should_SayWhatThePackageAllows_When_AnOperationHasTooMany", () => {
    expect(mistakesIn("$rows.count(1, 2);")[0]!.said).toBe("count takes exactly 1 arg, not 3");
  });

  it("should_AllowARange_When_ThePackageGivesOne", () => {
    // range is one to three, and a call gives the receiver plus its arguments.
    expect(mistakesIn("$n.range();")).toEqual([]);
    expect(mistakesIn("$n.range(1, 2);")).toEqual([]);
    expect(mistakesIn("$n.range(1, 2, 3);")[0]!.said).toBe("range takes 1 to 3 args, not 4");
  });
});

describe("the mistake row", () => {
  const source = "  if (paid !== none) return paid.count();";

  it("should_StartInTheSameColumnAsTheToken_When_TheCaretsAreDrawn", () => {
    const mistake = mistakesIn(source)[0]!;
    expect(mistake.column).toBe(11);
    const rows = mistakeRows(mistake);
    expect(lineText(rows.carets)).toBe(`${" ".repeat(11)}^^^`);
    // The carets cover exactly the token, and the source has the token at that column.
    expect(source.slice(mistake.column, mistake.column + mistake.text.length)).toBe("!==");
  });

  it("should_CountTheColumnWithinItsOwnLine_When_TheSourceHasSeveralLines", () => {
    const mistake = mistakesIn(`:calc {\n${source}\n}`)[0]!;
    expect(mistake.line).toBe(1);
    expect(mistake.column).toBe(11);
  });

  it("should_SayWhatIsWrongBesideTheCarets_When_TheRowsAreDrawn", () => {
    const rows = mistakeRows(mistakesIn(source)[0]!);
    expect(lineText(rows.said)).toBe(
      "no !== in this language   inequality is !=, and absence is none — read it with isSome()",
    );
    expect(rows.said[0]!.role).toBe("mono-bad");
  });
});

describe("the editor's own program", () => {
  const program = [
    ":calc {",
    '  const paid = $orders.filter(o => o.status == "paid");',
    "  if (paid !== none) return paid.count();",
    "  return paid.reduce((t, o) => t + o.total, 0.0);",
    "} > revenue",
  ].join("\n");

  it("should_ReadBackEveryCharacter_When_TheProgramIsHighlighted", () => {
    const { lines } = highlightCalc(program, language);
    expect(lines.map((line) => lineText([...line])).join("\n")).toBe(program);
  });

  it("should_FindOnlyTheOneMistakeTheDesignPutsThere_When_TheProgramIsRead", () => {
    const mistakes = highlightCalc(program, language).mistakes;
    expect(mistakes.map((mistake) => mistake.text)).toEqual(["!=="]);
    expect(mistakes[0]!.line).toBe(2);
    expect(mistakes[0]!.column).toBe(11);
  });

  it("should_ColourTheProgramTheWayTheCaptureDoes_When_ItIsRead", () => {
    const found = roles(program);
    expect(found[":calc"]).toBe("mono-meta");
    expect(found["const"]).toBe("mono-meta");
    expect(found["paid"]).toBe("mono-ink");
    expect(found["$orders"]).toBe("mono-ref");
    expect(found["filter"]).toBe("mono-provider");
    expect(found["status"]).toBe("mono-param");
    expect(found['"paid"']).toBe("mono-literal");
    expect(found["none"]).toBe("mono-meta");
    expect(found["revenue"]).toBe("mono-ref");
    expect(found[">"]).toBe("mono-dim");
  });
});
