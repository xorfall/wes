import { schema } from "./yaml-schema-fixture.test-support";
import { completionsAt } from "./yaml-complete";
import { describe, expect, it } from "vitest";
import { highlightYaml as highlight, type TypeVocabulary } from "./yaml-highlight";
import type { MonoRole } from "./MonoLine";

const highlightYaml = (source: string, vocabulary: TypeVocabulary) => highlight(source, vocabulary, "types", schema);

const vocabulary: TypeVocabulary = {
  names: ["Text", "Int", "Decimal", "Bool", "Record", "MonitorHealth", "MonitorRequest", "List", "Map", "Option", "Iter"],
};

/** The role covering the `at`-th occurrence of `needle` in `source`, from the tokeniser's spans. */
function roleAt(source: string, needle: string, at = 0): MonoRole | undefined {
  let from = -1;
  for (let seen = -1; seen < at; seen += 1) from = source.indexOf(needle, from + 1);
  const { spans } = highlightYaml(source, vocabulary);
  return spans.find((span) => span.from <= from && from < span.to)?.role;
}

const EXAMPLE = [
  "version: 1",
  "types:",
  "  MonitorRequest:",
  "    base: Record",
  "    fields: {seq: Int, second: Text, route: Text, durationMs: Int, status: Int}",
  "  MonitorHealth:",
  "    base: Record",
  "    fields: {state: Text, count: Int, errors: Int, errorPercent: Decimal, meanMs: Decimal, maxMs: Int, last: Int}",
  "  RequestBatch:",
  "    base: Record",
  "    fields: {data: 'List<MonitorRequest>'}",
  "iterators:",
  "  RequestItems:",
  "    input: 'List<MonitorRequest>'",
  "    output: 'Iter<MonitorRequest>'",
  "    mode: items",
].join("\n");

describe("the root document", () => {
  it("should_ColourTheCurrentRootKeys_When_TheyIntroduceASection", () => {
    for (const key of ["version", "types", "iterators"]) {
      expect(roleAt(`${key}:\n  x: 1`, key), key).toBe("mono-meta");
    }
  });

  it("should_LeaveAnUnknownRootKeyAsPlainText_When_TheSchemaDoesNotNameIt", () => {
    expect(roleAt("profiles:\n  x: 1", "profiles")).toBe("mono-ink");
  });
});

describe("a type declaration", () => {
  it("should_ColourTheDeclaredNamePlainly_When_ItIsUnderTypes", () => {
    expect(roleAt("types:\n  MonitorHealth:\n    base: Record", "MonitorHealth")).toBe("mono-ink");
  });

  it("should_ColourTheAllowedAttributeKeys_When_TheyAreUnderATypeName", () => {
    for (const key of ["base", "enum", "min", "max", "minLength", "maxLength", "minItems", "maxItems", "pattern", "fields"]) {
      expect(roleAt(`types:\n  T:\n    ${key}: 1`, key), key).toBe("mono-param");
    }
  });

  it("should_ResolveTheBaseAsATypeExpression_When_ItNamesAKnownType", () => {
    expect(roleAt("types:\n  Customer:\n    base: Record", "Record")).toBe("mono-ref");
  });

  it("should_MarkAnUnknownBase_When_TheWorkspaceHasNoSuchType", () => {
    const source = "types:\n  Customer:\n    base: Imaginary";
    expect(roleAt(source, "Imaginary")).toBe("mono-bad");
    const [mistake] = highlightYaml(source, vocabulary).mistakes;
    expect(mistake?.said).toBe("unknown type: Imaginary");
  });

  it("should_ResolveEveryFieldsShorthandValue_When_FieldsIsAFlowMapping", () => {
    const source = "types:\n  MonitorHealth:\n    fields: {state: Text, count: Int, errorPercent: Decimal}";
    for (const type of ["Text", "Int", "Decimal"]) expect(roleAt(source, type)).toBe("mono-ref");
    // The field names themselves are attribute-shaped, not type references.
    expect(roleAt(source, "state")).toBe("mono-param");
  });

  it("should_MarkAnUnknownFieldType_When_TheShorthandNamesNothingResolvable", () => {
    const source = "types:\n  T:\n    fields: {name: Nope}";
    expect(roleAt(source, "Nope")).toBe("mono-bad");
  });

  it("should_ResolveGenericConstructors_When_TheirArgumentsAreAllKnown", () => {
    const source = "types:\n  T:\n    fields: {tags: 'List<Text>'}";
    expect(roleAt(source, "List<Text>")).toBe("mono-ref");
  });

  it("should_RejectAGenericWithAnUnknownArgument_When_TheInnerTypeDoesNotResolve", () => {
    const source = "types:\n  T:\n    fields: {tags: 'List<Nope>'}";
    expect(roleAt(source, "List<Nope>")).toBe("mono-bad");
  });

  it("should_ReadTheExplicitFieldFormsOwnKeys_When_AFieldIsAMapping", () => {
    const source = "types:\n  T:\n    fields: {name: {type: Text, optional: true}}";
    expect(roleAt(source, "type:")).toBe("mono-param");
    expect(roleAt(source, "Text")).toBe("mono-ref");
    expect(roleAt(source, "optional")).toBe("mono-param");
    expect(roleAt(source, "true")).toBe("mono-meta");
  });
});

describe("an iterator declaration", () => {
  it("colours iterator keys and resolves input and output contracts", () => {
    const source = "iterators:\n  Items:\n    input: 'List<MonitorHealth>'\n    output: 'Iter<MonitorHealth>'\n    mode: items";
    for (const key of ["input", "output", "mode"]) expect(roleAt(source, key)).toBe("mono-param");
    expect(roleAt(source, "List<MonitorHealth>")).toBe("mono-ref");
    expect(roleAt(source, "Iter<MonitorHealth>")).toBe("mono-ref");
  });
  it("offers and recognises the engine's current modes", () => {
    const source = "iterators:\n  Items:\n    mode: ";
    const modes = completionsAt(source, source.length, vocabulary, 50, "types", schema);
    expect(modes.length).toBeGreaterThan(0);
    for (const { text: mode } of modes) expect(roleAt(source + mode, mode)).toBe("mono-ref");
  });
  it("marks unsupported iterator modes", () => {
    const source = "iterators:\n  Items:\n    mode: missing";
    expect(roleAt(source, "missing")).toBe("mono-bad");
    expect(highlightYaml(source, vocabulary).mistakes[0]?.said).toBe("unexpected value: missing");
  });
  it("leaves description text literal", () => {
    expect(roleAt("types:\n  T:\n    description: '{{.data.state}}'", "{{.data.state}}")).toBe("mono-literal");
  });
});

describe("scalars shared by both sections", () => {
  it("should_ColourAQuotedString_When_ItIsClosed", () => {
    expect(roleAt("types:\n  T:\n    description: 'hi'", "'hi'")).toBe("mono-literal");
  });

  it("should_MarkAnUnclosedString_When_TheQuoteNeverEnds", () => {
    const source = "types:\n  T:\n    description: 'hi";
    const [mistake] = highlightYaml(source, vocabulary).mistakes;
    expect(mistake?.said).toBe("this string is never closed");
  });

  it("should_ColourANumber_When_ItStandsAlone", () => {
    expect(roleAt("types:\n  T:\n    minItems: 3", "3")).toBe("mono-literal");
  });

  it("should_ColourAComment_When_ALineIsOneOrEndsInOne", () => {
    expect(roleAt("# a note\ntypes:", "# a note")).toBe("mono-faint");
  });
});

describe("the pasted example", () => {
  const { mistakes } = highlightYaml(EXAMPLE, vocabulary);

  it("should_ResolveEveryTypeAndKindInIt_When_TheVocabularyHasEveryNameItUses", () => {
    expect(mistakes).toEqual([]);
  });

  it("should_ColourBothDeclaredTypeNames_When_TheyIntroduceARecord", () => {
    expect(roleAt(EXAMPLE, "MonitorRequest", 0)).toBe("mono-ink"); // the declaration
    expect(roleAt(EXAMPLE, "MonitorHealth", 0)).toBe("mono-ink");
  });

  it("should_ResolveTheQuotedGenericInputShorthand_When_AFieldAsksForAList", () => {
    expect(roleAt(EXAMPLE, "'List<MonitorRequest>'")).toBe("mono-ref");
  });


});

it("tokenizes a large synthetic nested document without rescanning every prefix", () => {
  const source = "types:\n" + Array.from({ length: 4000 }, (_, index) =>
    `  Row${index}:\n    base: Record\n    fields: {label: Text, count: Int}\n`).join("");
  const start = performance.now();
  const result = highlightYaml(source, vocabulary);
  expect(result.mistakes).toEqual([]);
  expect(result.spans.at(-1)?.to).toBe(source.length);
  expect(result.lines.flatMap(line => line.map(segment => segment.text)).join("")).toBe(source.replaceAll("\n", ""));
  // A broad smoke limit catches accidental quadratic prefix parsing, not machine speed.
  expect(performance.now() - start).toBeLessThan(5000);
});

it("respects numeric and Boolean values instead of rejecting equivalent spellings", () => {
  const source = "version: +1\nenvironments:\n  demo:\n    protected: TRUE\n    config:\n      label: free text";
  expect(highlight(source, vocabulary, "env", schema).mistakes).toEqual([]);
});
