import { describe, expect, it } from "vitest";
import { acceptInto, expand, expandCaret, focusOn, UNFOCUSED } from "./focus";

const focus = (template: string) => {
  const read = focusOn(template);
  if (typeof read === "string") {
    throw new Error(read);
  }
  return read;
};

describe("reading a template", () => {
  it("should_PutWhatYouTypeThere_When_TheTemplateHasAHole", () => {
    expect(expand(focus('sh run cmd:"_"'), "ls -la")).toBe('sh run cmd:"ls -la"');
  });

  /** Appending is a hole in the last position, not a second mechanism. */
  it("should_AppendIt_When_TheTemplateHasNoHole", () => {
    expect(expand(focus("sh run"), 'cmd:"ls"')).toBe('sh run cmd:"ls"');
  });

  it("should_MeanTheSameThing_When_TheHoleIsWrittenAtTheEnd", () => {
    expect(expand(focus("sh run _"), 'cmd:"ls"')).toBe(expand(focus("sh run"), 'cmd:"ls"'));
  });

  /**
   * Two holes is somebody meaning two different things; filling only the first would build a command
   * they did not write, silently, every time.
   */
  it("should_RefuseItAndCount_When_ThereIsMoreThanOneHole", () => {
    expect(focusOn('sh run cmd:"grep _ my_file"')).toContain("only one can be the hole");
    expect(focusOn('sh run cmd:"grep _ my_file"')).toContain("2");
  });

  it("should_TakeIt_When_TheOnlyHoleIsInsideAWord", () => {
    expect(expand(focus('sh run cmd:"a_b"'), "X")).toBe('sh run cmd:"aXb"');
  });

  it("should_SayWhatToWrite_When_NothingWasGiven", () => {
    expect(focusOn("   ")).toContain("say what to focus on");
  });

  it("should_IgnoreTheSpaceAround_When_ReadingATemplate", () => {
    expect(expand(focus('  sh run cmd:"_"  '), "ls")).toBe('sh run cmd:"ls"');
  });
});

describe("focused on nothing", () => {
  /** The unfocused console is the same arithmetic, not a branch around it. */
  it("should_ChangeNothing_When_ThereIsNoTemplate", () => {
    expect(expand(UNFOCUSED, "lab-sensors read")).toBe("lab-sensors read");
    expect(expandCaret(UNFOCUSED, 7)).toBe(7);
  });

  it("should_SpliceItPlainly_When_ASuggestionIsAcceptedUnfocused", () => {
    expect(acceptInto(UNFOCUSED, "lab-sens", 8, 0, "lab-sensors"))
      .toEqual({ text: "lab-sensors", caret: 11 });
  });
});

describe("putting a suggestion back", () => {
  /** The whole word is inside what you typed: shift the offset and splice. */
  it("should_ShiftTheOffset_When_TheWordIsEntirelyYours", () => {
    const on = focus("sh run _");

    expect(acceptInto(on, 'cmd:"ls" tim', 12, expandCaret(on, 9), "timeout:PT5S"))
      .toEqual({ text: 'cmd:"ls" timeout:PT5S', caret: 21 });
  });

  /**
   * The word begins inside the template — completing `$re` in `sensor:_` finds the word `sensor:$re`.
   * The template's characters must be dropped off the front of the suggestion, and never re-typed.
   */
  it("should_DropWhatTheTemplateGave_When_TheWordStartsInIt", () => {
    const on = focus("lab-sensors read sensor:_");
    const at = on.template.indexOf("sensor:");

    expect(acceptInto(on, "$re", 3, at, "sensor:$readings"))
      .toEqual({ text: "$readings", caret: 9 });
  });

  it("should_KeepWhatComesAfterTheCaret_When_AcceptingMidLine", () => {
    const on = focus("sh run _");

    expect(acceptInto(on, "tim rest", 3, expandCaret(on, 0), "timeout:PT5S"))
      .toEqual({ text: "timeout:PT5S rest", caret: 12 });
  });

  /** A suggestion shorter than the template's contribution would slice past the end. */
  it("should_NotSliceBelowZero_When_TheOffsetsDisagree", () => {
    const on = focus("lab-sensors read sensor:_");

    expect(acceptInto(on, "x", 1, 0, "ab")).toEqual({ text: "", caret: 0 });
  });
});

describe("separating completed command words", () => {
  it("adds a space and places the caret ready for the next argument", () => {
    expect(acceptInto(UNFOCUSED, ":im", 3, 0, ":import", true))
      .toEqual({ text: ":import ", caret: 8 });
    expect(acceptInto(focus(":import _"), "sp", 2, 8, "spec", true))
      .toEqual({ text: "spec ", caret: 5 });
  });
  it("keeps parameter colons adjacent to values even when asked to separate", () => {
    expect(acceptInto(UNFOCUSED, ":import spec fi", 15, 13, "file:", true))
      .toEqual({ text: ":import spec file:", caret: 18 });
  });
  it("reuses a following separator without changing the rest of the command", () => {
    expect(acceptInto(UNFOCUSED, ":im spec", 3, 0, ":import", true))
      .toEqual({ text: ":import spec", caret: 8 });
    expect(acceptInto(UNFOCUSED, ":im\nsomething", 3, 0, ":import", true))
      .toEqual({ text: ":import\nsomething", caret: 7 });
  });
  it("preserves suffix text and does not add spaces inside a fixed quoted template", () => {
    expect(acceptInto(focus('sh run cmd:"_"'), "ls", 2, 12, "ls", true))
      .toEqual({ text: "ls", caret: 2 });
    expect(acceptInto(UNFOCUSED, "imXYZ", 2, 0, "import", true))
      .toEqual({ text: "importXYZ", caret: 6 });
  });
});
