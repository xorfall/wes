import { describe, expect, it } from "vitest";
import { commandLine, commandSegments } from "./command-line";
import { promptLine } from "./Prompt";
import { highlightCalc } from "./calc-highlight";
import { bundledPackage, readLanguage } from "./language";
import { lineText } from "./MonoLine";

const language = readLanguage(bundledPackage, "bundled");
describe("calc command colors", () => {
  it.each([
    ":calc { return [1, 2, 3]; }",
    ":calc{return [1,2,3];}",
    ":calc {return [-1,-2.5,3.25];}",
    ":calc { return [[1,2],[3.5,-4]]; }",
    "  @trace :calc {\treturn [1, 2]; } > numbers  ",
  ])("uses the source tokenizer in prompt and cell preview: %s", source => {
    const expected = highlightCalc(source, language).spans.map(span => ({ text: source.slice(span.from, span.to), ...(span.role ? { role: span.role } : {}) }));
    expect(commandSegments(source, language)).toEqual(expected);
    expect(promptLine(source, language)).toEqual(expected);
    expect(commandLine(source, undefined, language).slice(1)).toEqual(expected);
    expect(lineText(commandSegments(source))).toBe(source);
    for (const segment of commandSegments(source).filter(segment => /^\d/.test(segment.text))) {
      expect(segment.role).toBe("mono-literal");
    }
    expect(commandSegments(source)).toContainEqual({ text: "[", role: "mono-faint" });
  });

  it("uses the engine's language metadata for both prompt and preview", () => {
    const engine = readLanguage({ ...bundledPackage, operations: { ...bundledPackage.operations, fixtureOp: { operation: "fixtureOp", min: 1, max: 1, method: false } } }, "engine");
    for (const segments of [promptLine(":calc fixtureOp([1])", engine), commandLine(":calc fixtureOp([1])", undefined, engine)]) {
      expect(segments).toContainEqual({ text: "fixtureOp", role: "mono-provider" });
    }
  });

  it("preserves provider command source and roles", () => {
    const source = 'fixture read count:3 text:"two  spaces" > result';
    const segments = commandSegments(source);
    expect(lineText(segments)).toBe(source);
    expect(segments).toContainEqual({ text: "fixture", role: "mono-provider" });
    expect(segments).toContainEqual({ text: "read", role: "mono-provider" });
    expect(segments).toContainEqual({ text: "count:", role: "mono-param" });
    expect(segments).toContainEqual({ text: "3", role: "mono-literal" });
    expect(segments).toContainEqual({ text: "result", role: "mono-ref" });
  });
});

describe("unabridged multiline source", () => {
  const sources = [
    ':calc {\n  const first = 1;\n\n  return [first, 2];\n} > numbers\n\n',
    '@trace :calc {\r\n\treturn "iki  boşluk";\r\n} > text\r\n',
    'fixture read\n\tcount:3\n  text:"two  spaces"\n> result\n',
    'sh run cmd:"first\n\n  second\n\tthird"\n',
    'fixture first\n| fixture second\n| :calc {\n  return input;\n} > answer',
    '\n\tfixture read count:3\n\n',
    '',
    '\n\n',
  ];
  it.each(sources)("retains every character and line in cells: %j", source => {
    expect(lineText(commandSegments(source))).toBe(source);
    expect(lineText(commandLine(source))).toBe("❯ " + source);
    expect(lineText(commandLine(source, new Date(2026, 8, 23, 9, 16)))).toBe("09:16  ❯ " + source);
  });
  it("recognizes parameter/literal boundaries after line breaks", () => {
    const segments = commandSegments('fixture read\n  count:3\r\n\ttext:"two  spaces"');
    expect(segments).toContainEqual({ text: "read", role: "mono-provider" });
    expect(segments).toContainEqual({ text: "count:", role: "mono-param" });
    expect(segments).toContainEqual({ text: "3", role: "mono-literal" });
    expect(segments).toContainEqual({ text: "\r\n\t" });
  });
});
