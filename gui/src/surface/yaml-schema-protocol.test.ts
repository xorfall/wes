import { describe, expect, it, vi } from "vitest";
import { parseYamlSchema, type YamlSchema } from "./yaml-schema-protocol";
import { loadYamlSchema } from "./yaml-schema-loader";
import { completionsAt } from "./yaml-complete";
import { highlightYaml } from "./yaml-highlight";
import { newlineAt } from "./yaml-mode";

const synthetic = (): YamlSchema => ({
  language: "yaml", version: 1, roots: { env: "root", types: "root" },
  typeConstructors: [{ name: "Future", parameters: ["T"] }],
  definitions: {
    root: { kind: "object", fields: {
      future: { schema: "variant", required: false },
      values: { schema: "list", required: false },
      expression: { schema: "type", required: false },
    } },
    variant: { kind: "discriminated", field: "kind", variants: { alpha: "alpha", beta: "beta" } },
    alpha: { kind: "object", fields: {
      kind: { schema: "a", required: true }, payload: { schema: "text", required: false },
      left: { schema: "text", required: false }, right: { schema: "text", required: false },
    }, exclusive: [{ fields: ["left", "right"], min: 0, max: 1 }] },
    beta: { kind: "object", fields: { kind: { schema: "b", required: true }, payload: { schema: "number", required: false } } },
    a: { kind: "scalar", type: "Text", choices: ["alpha"] },
    b: { kind: "scalar", type: "Text", choices: ["beta"] },
    text: { kind: "scalar", type: "Text" }, number: { kind: "scalar", type: "Int" },
    list: { kind: "list", items: "text" }, type: { kind: "scalar", type: "TypeExpression" },
  },
});
const vocabulary = { names: ["Text", "Invoice"] };
const complete = (source: string, schema = synthetic()) => completionsAt(source, source.length, vocabulary, 50, "env", schema);

describe("the backend schema protocol", () => {
  it("uses new backend fields, variants, expected types, constructors and list indentation without client tables", () => {
    const schema = parseYamlSchema(synthetic());
    expect(complete("f", schema)).toMatchObject([{ text: "future", detail: "Object" }]);
    expect(complete("future:\n  p", schema)).toMatchObject([{ text: "payload", detail: "Text | Int" }]);
    expect(complete("future:\n  kind: beta\n  p", schema)).toMatchObject([{ text: "payload", detail: "Int" }]);
    expect(complete("future:\n  kind: alpha\n  left: chosen\n  r", schema)).toEqual([]);
    expect(complete("future:\n  kind: alpha\n  left", schema)).toMatchObject([{ text: "left", detail: "Text" }]);
    expect(complete("expression: F", schema)).toMatchObject([{ text: "Future", label: "Future<T>", detail: "TypeExpression" }]);
    expect(newlineAt("values:", 7, "env", schema).insert).toBe("\n  - ");
    expect(highlightYaml("future: {kind: beta}", vocabulary, "env", schema).mistakes).toEqual([]);
    expect(highlightYaml("expression: 'Future<Invoice>'", vocabulary, "env", schema).mistakes).toEqual([]);
  });

  it("rejects unknown versions/kinds, broken refs, cycles and malformed metadata", () => {
    expect(() => parseYamlSchema({ ...synthetic(), version: 2 })).toThrow();
    expect(() => parseYamlSchema({ ...synthetic(), definitions: { root: { kind: "future" } } })).toThrow();
    expect(() => parseYamlSchema({ ...synthetic(), definitions: { root: { kind: "list", items: "missing" } } })).toThrow();
    expect(() => parseYamlSchema({ ...synthetic(), definitions: { root: { kind: "list", items: "root" } } })).toThrow();
    expect(() => parseYamlSchema({ ...synthetic(), typeConstructors: [{ name: "Future", parameters: null }] })).toThrow();
    expect(() => parseYamlSchema(null)).toThrow();
    expect(() => parseYamlSchema({ ...synthetic(), definitions: { root: { kind: "scalar", type: "Text", choices: [""] } } })).not.toThrow();
  });

  it("offers no stale grammar and emits no declaration errors while schema is absent", () => {
    expect(completionsAt("pa", 2, vocabulary, 50, "env")).toEqual([]);
    expect(highlightYaml("anything: {kind: future}", vocabulary, "env").mistakes).toEqual([]);
  });

  it("fetches afresh after failure and passes cancellation to the current engine request", async () => {
    const controller = new AbortController();
    const fetch = vi.fn().mockResolvedValueOnce({ ok: false }).mockResolvedValueOnce({ ok: true, json: async () => synthetic() });
    vi.stubGlobal("fetch", fetch);
    try {
      await expect(loadYamlSchema(controller.signal)).rejects.toThrow();
      await expect(loadYamlSchema(controller.signal)).resolves.toEqual(synthetic());
      expect(fetch).toHaveBeenCalledTimes(2);
      expect(fetch).toHaveBeenLastCalledWith("/language/yaml", { signal: controller.signal, cache: "no-store" });
    } finally { vi.unstubAllGlobals(); }
  });
});
