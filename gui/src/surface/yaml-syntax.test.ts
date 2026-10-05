import { describe, expect, it } from "vitest";
import { yamlSyntaxProblem } from "./yaml-syntax";

describe("the local syntax-only submission gate", () => {
  it.each(["types: [", "types:\n\tRow: {}", "types: {}\ntypes: {}", "types: {}\n---\nviews: {}"])("rejects invalid syntax: %s", source => {
    expect(yamlSyntaxProblem(source)).toBeTruthy();
  });
  it.each([
    "types:\n  Later: {base: NotInVocabulary}",
    "unknownField: validYaml",
    'text: "Türkçe İı şğ\\n\\\"alıntı\\\""\r\n',
    "description: |\n  first\n  second\n",
    "value: !unknown-tag text",
    "value: &anchor text\nother: *anchor",
  ])("leaves package semantics to the backend: %s", source => {
    expect(yamlSyntaxProblem(source)).toBeUndefined();
  });
  it("bounds decoded UTF-8 input without expanding aliases", () => {
    expect(yamlSyntaxProblem("ş".repeat(524_289))).toContain("1 MiB");
    expect(yamlSyntaxProblem("a: &a [*a]")).toBeUndefined();
  });
});
