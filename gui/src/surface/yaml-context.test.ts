import { schema } from "./yaml-schema-fixture.test-support";
import { describe, expect, it } from "vitest";
import { completionsAt } from "./yaml-complete";
import { indentAt, newlineAt, readMode } from "./yaml-mode";
import { yamlLocation, type YamlContext } from "./yaml-schema";
const vocabulary = { names: ["Text", "Int", "Decimal", "Bool", "Record", "Invoice"] };
const complete = (source: string, context: YamlContext = "env") => completionsAt(source, source.length, vocabulary, 50, context, schema).map(c => c.text);
const enter = (source: string, context: YamlContext = "env") => {
  const edit = newlineAt(source, source.length, context, schema);
  return source.slice(0, edit.from) + edit.insert;
};

describe("context-specific YAML package completion", () => {
  it("recognizes package after version in the environment buffer", () => {
    expect(complete("version: 1\npa")).toEqual(["package"]);
    expect(complete("version: 1\n")).toEqual(["package", "targets", "environments"]);
    expect(complete("pa", "types")).toEqual([]);
    expect(complete("", "types")).toEqual(["version", "types", "iterators"]);
  });
  it("treats prototype names as ordinary YAML map keys", () => {
    for (const name of ["constructor", "toString", "__proto__"]) {
      expect(complete(`environments:\n  ${name}:\n    pa`)).toEqual(["parameters"]);
      expect(complete(`${name}:\n  pa`)).toEqual([]);
    }
  });
  it("offers the engine's Boolean choices", () => {
    expect(complete("environments:\n  demo:\n    protected: ")).toEqual(["true", "false"]);
  });
  it("offers accepted map keys in incomplete nested and flow env structures", () => {
    expect(complete("environments:\n  dev:\n    par")).toEqual(["parameters"]);
    expect(complete("environments:\n  dev:\n    parameters:\n      port:\n        type: ")).toEqual(["Text", "Int", "Bool"]);
    expect(complete("environments: {dev: {parameters: {port: {type: B")).toEqual(["Bool"]);
    expect(complete("environments:\n  dev:\n    imports:\n      api:\n        source:\n          kind: process\n          ")).toEqual(["bin"]);
    expect(complete("targets:\n  host:\n    kind: local\n    ")).toEqual(["cwd", "env"]);
    expect(complete("environments:\n  dev:\n    extends:\n      track: l")).toEqual(["latest"]);
  });
  it("completes block and flow contract fields, wrappers and iterators", () => {
    expect(complete("types:\n  Row:\n    base: Record\n    f", "types")).toEqual(["fields"]);
    expect(complete("types:\n  Row:\n    fields:\n      invoice: Inv", "types")).toEqual(["Invoice"]);
    expect(complete("types:\n  Row:\n    fields:\n      invoice:\n        op", "types")).toEqual(["optional"]);
    expect(complete("types:\n  Row:\n    fields:\n      data: 'List<Inv", "types")).toEqual(["Invoice"]);
    expect(complete("iterators:\n  Lines:\n    mode: regex-", "types")).toEqual(["regex-split"]);
  });
  it("resumes flow completion after a comment on the preceding line", () => {
    expect(complete("targets: { host: { kind: local, # note\n  c")).toEqual(["cwd"]);
    expect(complete("targets:\n  host:\n    # note\n    ki")).toEqual(["kind"]);
  });
  it("narrows environment target shapes by their discriminator", () => {
    expect(complete("targets:\n  host:\n    kind: local\n    ")).toEqual(["cwd", "env"]);
    expect(complete("environments:\n  demo:\n    imports:\n      api:\n        source: {kind: builtin, ")).toEqual(["name"]);
  });
  it("does not confuse comments, quoted values, freeform maps or literal strings with schema", () => {
    for (const source of ["# pa", "package: pa", 'package: "pa', "environments:\n  dev:\n    config:\n      type: In", "environments:\n  dev:\n    owner: x # pa"]) expect(complete(source)).toEqual([]);
    const mode = readMode("environments:\n  dev:\n    config:\n      type: private-value", vocabulary, "env", schema);
    expect(mode.diagnostics).toEqual([]);
    expect(mode.spans.filter(s => s.role === "mono-bad")).toEqual([]);
  });
  it("suppresses schema hints throughout multiline quoted strings", () => {
    expect(complete('package: "first\n  pa')).toEqual([]);
    expect(complete("package: 'first\n  pa")).toEqual([]);
    expect(complete('package: "first\n  last"\nta')).toEqual(["targets"]);
    expect(complete("package: |2-\n  pa")).toEqual([]);
  });
  it("replaces the entire token even with the caret in its middle", () => {
    const source = "version: 1\npackag";
    expect(yamlLocation(source, 14, "env", schema)).toMatchObject({ from: 11, to: 17, prefix: "pac" });
  });
});

describe("schema-aware YAML Enter", () => {
  it("keeps scalar values at the same level and opens object values", () => {
    expect(enter("package: ")).toBe("package: \n");
    expect(enter("targets: ")).toBe("targets: \n  ");
    expect(enter("targets: # hosts")).toBe("targets: # hosts\n  ");
    expect(enter("environments:\n  dev:\n    imports: ")).toBe("environments:\n  dev:\n    imports: \n      ");
  });
  it("opens and continues actual lists without turning named maps into lists", () => {
    expect(enter("environments:\n  dev:\n    hide:\n      imports: ")).toBe("environments:\n  dev:\n    hide:\n      imports: \n        - ");
    expect(enter("environments:\n  dev:\n    hide:\n      imports:\n        - old")).toContain("- old\n        - ");
    expect(enter("types:\n  T:\n    enum:", "types")).toContain("enum:\n      - ");
    expect(enter("types:\n  T:\n    enum:\n      - hello", "types")).toContain("- hello\n      - ");
    expect(enter("  - ")).toBe("  \n");
  });
  it("uses schema indentation for Tab and aligns closing flow brackets", () => {
    const indent = (source: string) => indentAt(source, source.lastIndexOf("\n") + 1, readMode(source, vocabulary, "env", schema), "env", schema);
    expect(indent("package: \n")).toBe(0);
    expect(indent("targets: \n")).toBe(2);
    expect(indent("targets: {\n  host: {kind: local}\n}")).toBe(0);
  });
  it("preserves literal text, tabs, blank lines and flow continuation", () => {
    expect(enter("package: |-")).toBe("package: |-\n  ");
    expect(enter("package: |\n  thing:")).toBe("package: |\n  thing:\n  ");
    expect(enter("package: |\n  - prose")).toBe("package: |\n  - prose\n  ");
    expect(enter("package: |\n  - ")).toBe("package: |\n  - \n  ");
    expect(enter("targets:\n\tlocal:")).toBe("targets:\n\tlocal:\n\t  ");
    expect(enter("targets:\n\n")).toBe("targets:\n\n\n");
    expect(enter("targets: {")).toBe("targets: {\n  ");
    expect(enter("targets: {\n  a: {kind: local},")).toBe("targets: {\n  a: {kind: local},\n  ");
    expect(newlineAt("package: demo", 4, "env")).toEqual({ from: 4, insert: "\n" });
  });
});
