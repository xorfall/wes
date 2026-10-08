import { describe, expect, it } from "vitest";
import type { TypeShape } from "../protocol";
import { parseEntry, Registry } from "./registry";

/* Synthetic entries: a log of invented build lines beside a table of invented services. */
const LOG = `version: 2
type: BuildLine
applies: [list]
fields: {source: BuildSource, ordinal: Int, text: Text, level: Option<Text>}
present:
  kind: log
  mapping:
    key: [source.id, ordinal]
    text: text
    level: level
`;

const TABLE = `version: 2
type: ServiceRow
applies: [list]
fields: {id: Text, status: Text}
present:
  kind: table
  tones:
    status: {cases: {"1.50": warn, ready: ok}}
  styles: {status: badge}
`;

const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const INT: TypeShape = { kind: "primitive", name: "INT" };
const lines: TypeShape = { kind: "list", element: { kind: "record", name: "BuildLine", fields: [
  { name: "source", type: { kind: "record", name: "BuildSource", fields: [] } }, { name: "ordinal", type: INT },
  { name: "text", type: TEXT }, { name: "level", type: { kind: "option", element: TEXT } },
] } };
const services: TypeShape = { kind: "list", element: { kind: "record", name: "ServiceRow", fields: [
  { name: "id", type: TEXT }, { name: "status", type: TEXT },
] } };

describe("log mappings and declared tones in one registry", () => {
  it("keeps a version 2 log mapping and a toned table entry side by side", () => {
    const registry = Registry.core().withHome([{ name: "build.yaml", text: LOG }, { name: "services.yaml", text: TABLE }]);
    expect(registry.problems).toEqual([]);
    expect(registry.logMapping(lines, [])).toEqual({ key: ["source.id", "ordinal"], text: "text", level: "level", flags: [] });
    const table = registry.match(services, [])!.entry;
    expect(table.log).toBeUndefined();
    expect(table.tones?.status?.cases).toEqual({ "1.50": "warn", ready: "ok" });
    expect(table.styles).toEqual({ status: "badge" });
    expect(registry.logMapping(services, [])).toBeUndefined();
  });

  it("refuses tones on a log entry, a mapping on a table entry, and undeclared fields in either", () => {
    const tonedLog = LOG + "  tones:\n    level: {cases: {error: bad}}\n";
    expect(() => parseEntry(tonedLog, "home:x.yaml")).toThrow("are for kind: table");
    expect(() => parseEntry(TABLE + "  mapping: {key: id, text: status}\n", "home:x.yaml")).toThrow("mapping is only for kind: log");
    expect(() => parseEntry(LOG.replace("text: text", "text: message"), "home:x.yaml")).toThrow("field message is not declared");
    expect(() => parseEntry(TABLE.replace("styles: {status: badge}", "styles: {owner: badge}"), "home:x.yaml")).toThrow("field owner is not declared");
  });

  it("checks written spellings only where tones or rules are declared", () => {
    expect(parseEntry(LOG.replace("fields: {", "anchors: {n: 1.50}\nfields: {"), "home:x.yaml").log?.text).toBe("text");
    expect(() => parseEntry(TABLE.replace("\"1.50\": warn", "1.50: warn"), "home:x.yaml")).toThrow("quote larger or decimal numbers");
  });
});
