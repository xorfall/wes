import { describe, expect, it } from "vitest";
import type { StoredValue, TypeShape } from "../protocol";
import { decodeMeta, declarationPath } from "../value-meta";
import { prepareSync } from "./prepare";
import { present } from "./present";
import { parseEntry, Registry } from "./registry";
import type { Context, PresentationNode, Run } from "./types";

/* A synthetic service table: invented names and states, never a user's data. */
const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const opt = (element: TypeShape): TypeShape => ({ kind: "option", element });
const row: TypeShape = { kind: "record", name: "ServiceRow", fields: [
  { name: "id", type: TEXT }, { name: "status", type: TEXT }, { name: "previous", type: opt(TEXT) },
] };
const digest = (n: number) => `sha256:${n.toString(16).padStart(64, "0")}`;
const status = { contract: { name: "ServiceStatus", digest: digest(2) }, kind: "text", members: ["ready", "failed", "unknown"], total: 3, complete: true, tones: { ready: "ok", failed: "bad" } };
const meta = decodeMeta({ version: 1, contract: { name: "List<ServiceRow>", digest: digest(1) }, truncated: false,
  fields: { "/e/f:status": status, "/e/f:previous/o": status } })!;
const rows = [
  { id: "api", status: "ready", previous: { kind: "some", value: "failed" } },
  { id: "worker", status: "failed", previous: { kind: "none" } },
  { id: "cron", status: "unknown", previous: { kind: "some", value: "ready" } },
];
const context = (over: Partial<Context> = {}): Context => ({ mode: "expanded", columns: 118, lines: 20, density: "normal", locale: "en-GB", timeZone: "UTC", ...over });
const show = (value: Pick<StoredValue, "type" | "data" | "meta">, over: Partial<Context> = {}) =>
  present({ prepared: prepareSync(value), context: context(over), registry: Registry.core() });

function runs(node: PresentationNode): Run[] {
  if (node.kind === "table") return node.rows.flat(2);
  if (node.kind === "fields") return node.rows.flatMap(it => it.node === node ? [] : runs(it.node));
  if (node.kind === "line") return [...node.runs];
  if (node.kind === "items") return [...(node.lines?.flat() ?? node.items)];
  return [];
}
const toneOf = (all: Run[], text: string) => all.find(it => it.text === text)?.tone;

describe("declared tones", () => {
  it("maps data pointers to declaration paths through lists, records and options", () => {
    const list: TypeShape = { kind: "list", element: row };
    expect(declarationPath(list, "/2/status")).toBe("/e/f:status");
    expect(declarationPath(list, "/0/previous")).toBe("/e/f:previous/o");
    expect(declarationPath(list, "/0/missing")).toBeUndefined();
    expect(declarationPath({ kind: "record", name: "", fields: [{ name: "a/b", type: TEXT }] }, "/a~1b")).toBe("/f:a~1b");
  });

  it("tones table cells by the exact value their contract declares, and leaves the rest alone", () => {
    const table = runs(show({ type: { kind: "list", element: row }, data: rows, meta }).root);
    expect(toneOf(table, "ready")).toBe("ok");
    expect(toneOf(table, "failed")).toBe("bad");
    expect(toneOf(table, "unknown")).toBe("literal");
    expect(toneOf(table, "api")).toBe("literal");
    expect(table.filter(it => it.text === "none").every(it => it.tone === "faint")).toBe(true);
  });

  it("tones a record's fields and a list of declared scalars", () => {
    const one = runs(show({ type: row, data: rows[1], meta: decodeMeta({ ...meta, fields: { "/f:status": status } }) }).root);
    expect(toneOf(one, "failed")).toBe("bad");
    const states = runs(show({ type: { kind: "list", element: TEXT }, data: ["ready", "failed"], meta: decodeMeta({ ...meta, fields: { "/e": status } }) }).root);
    expect(states.map(it => it.tone)).toContain("ok");
    expect(states.map(it => it.tone)).toContain("bad");
  });

  it("draws nothing differently without metadata", () => {
    const table = runs(show({ type: { kind: "list", element: row }, data: rows }).root);
    expect(table.some(it => it.tone === "ok" || it.tone === "bad")).toBe(false);
  });
});

const ENTRY = `version: 2
type: ServiceRow
applies: [list]
fields: {id: Text, status: Text, previous: Option<Text>}
present:
  kind: table
  columns: [id, status, previous]
  tones:
    status: {cases: {unknown: warn, ready: ink}}
  styles: {status: badge}
  rules:
    - when: {field: id, equals: worker}
      target: {row: true}
      tone: bad
    - when: {field: status, in: [unknown]}
      target: {cell: id}
      tone: meta
      style: badge
`;

describe("presentation entries with tones, styles and rules", () => {
  it("parses version 2 and refuses what it cannot check", () => {
    const entry = parseEntry(ENTRY, "home:services.yaml");
    expect(entry.tones?.status).toEqual({ cases: { unknown: "warn", ready: "ink" }, otherwise: "inherit" });
    expect(entry.rules).toHaveLength(2);
    const bad = (change: (text: string) => string) => () => parseEntry(change(ENTRY), "home:x.yaml");
    expect(bad((t) => t.replace("version: 2", "version: 1"))).toThrow("need version: 2");
    expect(bad((t) => t.replace("unknown: warn", "unknown: red"))).toThrow("must be one of");
    expect(bad((t) => t.replace("styles: {status: badge}", "styles: {status: glow}"))).toThrow("plain or badge");
    expect(bad((t) => t.replace("equals: worker", "equals: worker, in: [a]"))).toThrow("exactly one of equals or in");
    expect(bad((t) => t.replace("target: {row: true}\n      tone: bad", "target: {row: true}\n      style: badge"))).toThrow("takes no style");
    expect(bad((t) => t.replace("field: id, equals", "field: owner, equals"))).toThrow("field owner is not declared");
    expect(bad((t) => t.replace("equals: worker", "equals: 90071992547409930"))).toThrow("quote larger");
    expect(bad((t) => t.replace("kind: table", "kind: fields"))).toThrow("are for kind: table");
    expect(bad((t) => t.replace("cases: {unknown: warn, ready: ink}", "cases: {unknown: warn, unknown: ok}"))).toThrow();
  });

  it("refuses case keys and rule values that YAML would re-spell, and keeps quoted ones exactly", () => {
    const bad = (change: (text: string) => string) => () => parseEntry(change(ENTRY), "home:x.yaml");
    for (const key of ["1.50", "90071992547409930", "~", "0x1F", "1e3"]) {
      expect(bad((t) => t.replace("unknown: warn,", `${key}: warn,`))).toThrow("quote larger or decimal numbers");
    }
    expect(bad((t) => t.replace("equals: worker", "equals: 1.0"))).toThrow("quote larger or decimal numbers");
    expect(bad((t) => t.replace("in: [unknown]", "in: [unknown, 2.50]"))).toThrow("quote larger or decimal numbers");
    expect(bad((t) => t.replace("unknown: warn,", "200: warn, \"200\": bad,"))).toThrow("twice");
    const quoted = parseEntry(ENTRY.replace("unknown: warn,", "\"1.50\": warn, 200: bad,").replace("equals: worker", "equals: \"90071992547409930\""), "home:x.yaml");
    expect(quoted.tones!.status!.cases).toEqual({ "1.50": "warn", "200": "bad", ready: "ink" });
    expect(quoted.rules![0]!.values).toEqual(["90071992547409930"]);
  });

  it("checks case keys and rule values reached through aliases as their anchors spell them", () => {
    const anchors = `anchors: {quoted: &quoted "1.50", count: &count 200, decimal: &decimal {1.50: bad}, when: &when {field: status, in: [1.0]}}\n`;
    const aliased = (change: (text: string) => string) => () => parseEntry(anchors + change(ENTRY), "home:x.yaml");
    expect(aliased((t) => t.replace("cases: {unknown: warn, ready: ink}", "cases: *decimal"))).toThrow("quote larger or decimal numbers");
    expect(aliased((t) => t.replace("{field: status, in: [unknown]}", "*when"))).toThrow("quote larger or decimal numbers");
    expect(aliased((t) => t.replace("in: [unknown]", "in: &loop [unknown, *loop]"))).toThrow("quote larger or decimal numbers");
    const kept = aliased((t) => t.replace("unknown: warn,", "*quoted : warn, *count : bad,").replace("equals: worker", "equals: *quoted"))();
    expect(kept.tones!.status!.cases).toEqual({ "1.50": "warn", "200": "bad", ready: "ink" });
    expect(kept.rules![0]!.values).toEqual(["1.50"]);
  });

  it("resolves type tones, then the entry's cases, then rules, and draws badges and row tints", () => {
    const registry = Registry.core().withHome([{ name: "services.yaml", text: ENTRY }]);
    const node = present({ prepared: prepareSync({ type: { kind: "list", element: row }, data: rows, meta }), context: context(), registry }).root;
    if (node.kind !== "table") throw new Error("expected a table");
    const at = (r: number, column: string) => node.rows[r]![node.columns.findIndex((c) => c.name === column)]!;
    expect(at(0, "status")[0]!.tone).toBe("ink");
    expect(at(1, "status")[0]!.tone).toBe("bad");
    expect(at(2, "status")[0]!.tone).toBe("warn");
    expect(at(2, "id")[0]!.tone).toBe("meta");
    expect(node.styles?.[0]?.[1]).toBe("badge");
    expect(node.styles?.[2]?.[0]).toBe("badge");
    expect(node.styles?.[0]?.[0]).toBeUndefined();
    expect(node.tints).toEqual([undefined, "bad", undefined]);
  });

  it("never matches a rule's literal NUL against an absent or missing field", () => {
    const registry = Registry.core().withHome([{ name: "services.yaml", text: ENTRY.replace("{field: id, equals: worker}", "{field: previous, equals: \"\\0\"}") }]);
    const data = [
      { id: "api", status: "ready", previous: { kind: "some", value: "\u0000" } },
      { id: "worker", status: "failed", previous: { kind: "none" } },
      { id: "cron", status: "ready" },
    ];
    const node = present({ prepared: prepareSync({ type: { kind: "list", element: row }, data }), context: context(), registry }).root;
    if (node.kind !== "table") throw new Error("expected a table");
    expect(node.tints).toEqual(["bad", undefined, undefined]);
  });
});
