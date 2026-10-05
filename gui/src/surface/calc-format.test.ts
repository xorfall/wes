import { describe, expect, it } from "vitest";
import { formatCommand } from "./calc-format";

describe("formatting a command for reading", () => {
  it("should_OpenTheCalcBodyStatementByStatement_When_ItWasWrittenOnOneLine", () => {
    // Arrange
    const source = ':calc { return range(1, 101).map(i => ({ id: i, name: "item-" + text(i), even: rem(i, 2) == 0 })); } > hundred';
    // Act
    const formatted = formatCommand(source);
    // Assert
    expect(formatted).toBe([
      ":calc {",
      "  return range(1, 101).map(i => ({",
      "    id: i,",
      '    name: "item-" + text(i),',
      "    even: rem(i, 2) == 0",
      "  }));",
      "} > hundred",
    ].join("\n"));
  });

  it("should_KeepStringsCommentsAndEmptyObjects_When_TheBodyHoldsThem", () => {
    // Arrange
    const source = ':calc {\n\tconst label = "two  spaces, {not a brace}"; // note, kept\n\n  return { a: label, b: {} };\n} > label';
    // Act
    const formatted = formatCommand(source);
    // Assert
    expect(formatted).toBe([
      ":calc {",
      '  const label = "two  spaces, {not a brace}"; // note, kept',
      "  return {",
      "    a: label,",
      "    b: {}",
      "  };",
      "} > label",
    ].join("\n"));
  });

  it("should_LeaveOtherCommandsAndTheRestOfAPipelineAlone_When_OnlyOneStageIsCalc", () => {
    // Arrange
    const source = 'http request url:"http://127.0.0.1:9/x" body:{ "a": 1 }\n| :calc { return input.status; } > status';
    // Act / Assert
    expect(formatCommand(source)).toBe('http request url:"http://127.0.0.1:9/x" body:{ "a": 1 }\n| :calc {\n  return input.status;\n} > status');
    expect(formatCommand("docker ps")).toBe("docker ps");
    expect(formatCommand(":calc { return 1;")).toBe(":calc { return 1;");
  });
});

it("preserves the pure assertion and typed definition header", () => {
  const source = ":def scale(input: Int) -> Int as :calc pure { return input * 2; }";
  expect(formatCommand(source)).toBe(":def scale(input: Int) -> Int as :calc pure {\n  return input * 2;\n}");
});
