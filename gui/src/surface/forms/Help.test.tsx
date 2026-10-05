import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import fixture from "../../../../examples/readable-help/display.json";
import type { FormValue } from "./form";
import { prepareSync } from "../../presentation/prepare";
import { present } from "../../presentation/present";
import { Registry } from "../../presentation/registry";
import { HelpPreview, readHelp, MAX_HELP_CHARS, MAX_HELP_ROWS } from "./Help";
import { ValueView } from "../../views/Result";
import type { StoredValue } from "../../protocol";

const captures = fixture as unknown as Record<string, FormValue>;
const kindOf = (value: FormValue) => present({
  prepared: prepareSync(value), registry: Registry.core(),
  context: { mode: "window", columns: 100, lines: 4000, density: "normal", locale: "en-GB", timeZone: "UTC" },
}).root;
const printed = (value: FormValue, truncated = false) => {
  const root = kindOf(value);
  expect(root.kind === "custom" && root.name).toBe("help");
  const model = readHelp(value);
  expect(model.truncated).toBe(truncated);
  return renderToStaticMarkup(<HelpPreview model={model} />);
};

describe("help documents from the actual engine display envelope", () => {
  it("shows group commands and descriptions instead of kind and child counts", () => {
    const markup = printed(captures.workspace!);
    expect(markup).toContain(":workspace load");
    expect(markup).toContain("Open a saved workspace without executing its work.");
    expect(markup).toContain(":workspace policy");
    expect(markup).toContain(":workspace save");
    expect(markup).not.toMatch(/\{kind\}|\[3\]|Invocation|Operands/);
    expect(printed(captures.node!)).toContain(":node cancel");
    expect(printed(captures.env!)).toContain("Delete a retired, unreferenced environment");
  });
  it("shows syntax, examples, parameters and provider constraints without another click", () => {
    expect(printed(captures.calc!)).toContain("return [1,2,3].map");
    expect(printed(captures.stream!)).toContain(":stream");
    expect(printed(captures.fork!)).toContain(":fork");
    expect(printed(captures.inspect!)).toContain(":inspect $result");
    const provider = printed(captures["http request"]!);
    expect(provider).toContain("required");
    expect(provider).toContain("UNSAFE");
    expect(provider).not.toContain("Rules"); // The actual HTTP provider declares no rules.
    const data = captures["http request"]!.data as Record<string, unknown>;
    const withRule = { ...captures["http request"]!, data: { ...data,
      invocation: { ...(data.invocation as object), rules: ["format must be one of json, text"] } } };
    expect(printed(withRule)).toContain("format must be one of json, text");
    expect(printed(captures["list templates"]!)).not.toContain("capabilities");
    // Structured shapes are formatted directly; ordinary provider help stays complete.
    for (const value of Object.values(captures)) {
      expect(renderToStaticMarkup(<ValueView value={value as StoredValue} />)).toContain(printed(value));
    }
  });
  it("preserves ordinary record previews and handles malformed/empty help defensively", () => {
    expect(kindOf({ ...captures.workspace!, type: { kind: "unknown" } }).kind).toBe("fields");
    expect(kindOf({ ...captures.workspace!, data: { path: 42, children: null } }).kind).toBe("fields");
    const empty = { ...captures.workspace!, data: { path: "empty", children: [], invocation: { kind: "none" } } };
    expect(printed(empty)).not.toMatch(/Invocation|More help|kind/);
    expect(printed({ ...empty, data: { path: "broken", children: [null, 42, {}], invocation: { parameters: [null, 7], extra: null } } })).not.toContain("undefined");
  });
  it("bounds enormous, deeply nested and cyclic metadata and states truncation", () => {
    const cycle: Record<string, unknown> = {}; cycle.self = cycle;
    for (const extra of [cycle, Array(100000).fill("detail"), "x".repeat(MAX_HELP_CHARS * 2)]) {
      const model = readHelp({ ...captures.workspace!, data: { path: "huge", children: [], invocation: { extra } } });
      expect(model.rows.length).toBeLessThanOrEqual(MAX_HELP_ROWS);
      expect(model.rows.reduce((n, r) => n + r.label.length + r.text.length, 0)).toBeLessThanOrEqual(MAX_HELP_CHARS);
      expect(model.truncated).toBe(true);
      expect(renderToStaticMarkup(<HelpPreview model={model} />)).toContain("Help display limit reached");
    }
  });
});


it("groups scalar operations and multiline examples under one label", () => {
  const model = readHelp(captures.calc!);
  const operations = model.rows.filter(row => row.label === "Operations");
  expect(operations).toHaveLength(1);
  expect(operations[0]!.text).toContain("some, isSome");
  const examples = model.rows.filter(row => row.label === "Examples");
  expect(examples).toHaveLength(1);
  expect(model.rows.some(row => row.label === "Example")).toBe(false);
  expect(examples[0]!.text).toContain("return [1,2,3].map(x => x*2)");
  expect(examples[0]!.text).toContain("\n// comment\n");
});


it("shows operation arguments, return semantics and example source from engine help", () => {
  const matches = readHelp(captures["calc iter.matches"]!);
  expect(matches.rows.find(row => row.label === "Usage")?.text).toBe("iter.matches(text: Text, pattern: Text) -> Iter<Text>");
  expect(matches.rows.find(row => row.label === "Returns")?.text).toBe("Iter<Text>");
  expect(matches.rows.find(row => row.label === "text:")?.text).toBe("Text · required");
  expect(matches.rows.find(row => row.label === "Examples")?.text).toContain("collect(iter.matches");
  expect(matches.truncated).toBe(false);
  const capturesHelp = readHelp(captures["calc iter.captures"]!);
  expect(capturesHelp.rows.find(row => row.label === "Behavior")?.text).toContain("full match is excluded");
});


it("describes lexical shadowing and the vocabulary that remains reserved", () => {
  const names = readHelp(captures.calc!).rows.find(row => row.label === "Names")?.text;
  expect(names).toContain("nearest lexical binding wins");
  expect(names).toContain("Receiver methods");
  expect(names).toContain("true/false/none");
  expect(names).toContain("iter namespace");
});


it("formats structured argument/result types and imported constraints", () => {
  const primitive = (name: string) => ({ kind: "primitive", name });
  const value = { ...captures.workspace!, data: { provider: "inventory", path: "list", children: [], invocation: {
    parameters: [
      { name: "limit", type: primitive("INT"), required: true, constraints: ["number: 1..50 (inclusive)"] },
      { name: "status", type: primitive("TEXT"), required: false, choices: ["active", "paused"] },
      { name: "ids", type: { kind: "option", element: { kind: "list", element: primitive("TEXT") } } },
    ],
    result: { kind: "record", name: "Page", fields: [{ name: "items", type: { kind: "list", element: primitive("TEXT") } }] },
  } } };
  const model = readHelp(value);
  expect(model.truncated).toBe(false);
  expect(model.rows.find(row => row.label === "limit:")?.text).toBe("Int · required");
  expect(model.rows.find(row => row.label === "status:")?.text).toBe("Text · optional");
  expect(model.rows.find(row => row.label === "ids:")?.text).toBe("Option<List<Text>> · optional");
  expect(model.rows.find(row => row.label === "Constraints")?.text).toBe("number: 1..50 (inclusive)");
  expect(model.rows.find(row => row.label === "Allowed values")?.text).toBe("active, paused");
  expect(model.rows.find(row => row.label === "Result")?.text).toBe("Page");
  expect(model.rows.find(row => row.label === "Result.items")?.text).toBe("List<Text>");
  expect(printed(value)).not.toContain("Fields · Type · Kind");
});


it("treats malformed primitive names as text without reading object prototypes", () => {
  const value = { ...captures.workspace!, data: { path: "malformed", children: [], invocation: {
    result: { kind: "primitive", name: "__proto__" }, parameters: [{ name: "value", type: { kind: "primitive", name: "constructor" } }],
  } } };
  expect(printed(value)).toContain("__proto__");
  expect(readHelp(value).rows.find(row => row.label === "value:")?.text).toBe("constructor · optional");
});
