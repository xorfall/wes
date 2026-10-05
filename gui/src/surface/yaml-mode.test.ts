import { schema } from "./yaml-schema-fixture.test-support";
import { describe, expect, it } from "vitest";
import { lineText } from "./MonoLine";
import type { TypeVocabulary } from "./yaml-highlight";
import { gutterMark, indentAt, lineAt, pairAt, readMode, wordAt } from "./yaml-mode";

const vocabulary: TypeVocabulary = { names: ["Text", "Int", "Decimal", "Record", "MonitorHealth", "MonitorRequest"] };

describe("gutter and mistake lines", () => {
  it("should_MarkTheLineAnUnknownTypeIsOn_When_ReadModeSeesOne", () => {
    const source = "types:\n  T:\n    base: Imaginary";
    const mode = readMode(source, vocabulary, "types", schema);
    expect(mode.marked).toEqual([2]);
    expect(lineText(gutterMark(2, mode.marked))).toBe("  3 ●");
    expect(lineText(gutterMark(0, mode.marked))).toBe("  1  ");
  });
});

describe("bracket pairing", () => {
  it("should_FindTheFlowMappingsPartner_When_TheCaretIsBesideEitherBrace", () => {
    const source = "types:\n  T:\n    fields: {state: Text}";
    const mode = readMode(source, vocabulary, "types", schema);
    const open = source.indexOf("{");
    const close = source.indexOf("}");
    expect(pairAt(source, open, mode)).toEqual({ open, close });
    expect(pairAt(source, close + 1, mode)).toEqual({ open, close });
  });
});

describe("indentation", () => {
  const at = (source: string) => indentAt(source, source.length, readMode(source, vocabulary, "types", schema));

  it("should_OpenOneLevel_When_ThePreviousLineIsABlockKeyWithNoInlineValue", () => {
    expect(at("types:\n")).toBe(2);
    expect(at("types:\n  T:\n")).toBe(4);
  });

  it("should_KeepTheSameIndent_When_ThePreviousLineHasAnInlineValue", () => {
    expect(at("types:\n  T:\n    base: Record\n")).toBe(4);
  });

  it("should_IndentUnderADash_When_ThePreviousLineIsASequenceItem", () => {
    expect(at("types:\n  T:\n    enum:\n      - example\n")).toBe(8);
  });

  it("should_AddALevelPerOpenFlowBracket_When_TheLineBeginsInsideOne", () => {
    expect(at("types:\n  T:\n    fields: {\n")).toBe(6);
  });

  it("should_SkipBlankLines_When_LookingForThePreviousContent", () => {
    expect(at("types:\n\n\n")).toBe(2);
  });
});

describe("the word being completed", () => {
  it("should_StopAtTheGenericsOpeningAngle_When_TypingInsideOne", () => {
    const source = "    inputs: {data: 'List<Mo";
    const { text, from } = wordAt(source, source.length);
    expect(text).toBe("Mo");
    expect(from).toBe(source.length - 2);
  });

  it("should_StopAtTheColon_When_TypingAKeysValue", () => {
    const source = "    base: Rec";
    expect(wordAt(source, source.length).text).toBe("Rec");
  });

  it("should_BeEmpty_When_TheCaretFollowsPunctuation", () => {
    expect(wordAt("fields: {", 9).text).toBe("");
  });
});

describe("the line a source offset is on", () => {
  it("should_CountFromZero_When_ReadingBackOutOfTheSource", () => {
    const source = "a\nb\nc";
    expect(lineAt(source, 0)).toBe(0);
    expect(lineAt(source, 2)).toBe(1);
    expect(lineAt(source, 4)).toBe(2);
  });
});
