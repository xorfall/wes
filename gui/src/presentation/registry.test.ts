import { describe, expect, it } from "vitest";
import type { TypeShape } from "../protocol";
import { prepareSync } from "./prepare";
import { present, runText } from "./present";
import { parseEntry, Registry, type HomeFile } from "./registry";
import type { Context } from "./types";

/* Synthetic entries and values only. */

const INT: TypeShape = { kind: "primitive", name: "INT" };
const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const DECIMAL: TypeShape = { kind: "primitive", name: "DECIMAL" };
const opt = (element: TypeShape): TypeShape => ({ kind: "option", element });
const sample: TypeShape = {
  kind: "record", name: "DockerStatsSample",
  fields: [
    { name: "container", type: TEXT }, { name: "sequence", type: INT }, { name: "timestamp_ns", type: opt(INT) },
    { name: "cpu_percent", type: opt(DECIMAL) }, { name: "memory_usage_bytes", type: opt(INT) },
  ],
};
const list: TypeShape = { kind: "list", element: sample };
const data = [1, 2, 3].map((at) => ({
  container: "f".repeat(64), sequence: at, timestamp_ns: { kind: "some", value: 1_700_000_000_000_000_000 + at * 1_000_000_000 },
  cpu_percent: { kind: "some", value: 0.25 }, memory_usage_bytes: { kind: "some", value: 1_953_792 },
}));
const context: Context = { mode: "preview", columns: 118, lines: 6, density: "normal", locale: "en-GB", timeZone: "UTC" };

const STATS_ENTRY = `version: 1
type: DockerStatsSample
applies: [record, list]
fields:
  sequence: Int
  timestamp_ns: Option<Int>
  cpu_percent: Option<Decimal>
  memory_usage_bytes: Option<Int>
present:
  kind: table
  columns: [sequence, timestamp_ns, cpu_percent, memory_usage_bytes]
  formats:
    timestamp_ns: {kind: time, unit: ns, epoch: unix}
    memory_usage_bytes: size
`;
const home = (text: string): HomeFile[] => [{ name: "docker-stats-sample.yaml", text }];
const table = (registry: Registry) => present({ prepared: prepareSync({ type: list, data }), context, registry });
const columnsOf = (registry: Registry) => {
  const root = table(registry).root;
  return root.kind === "table" ? root.columns.map((column) => column.name) : [];
};

describe("registry entries", () => {
  it("should_ChangeColumns_When_ADataHomeEntryNamesTheType", () => {
    // Arrange
    const registry = Registry.core().withHome(home(STATS_ENTRY));
    // Act
    const shown = table(registry);
    // Assert
    expect(shown.root.kind === "table" && shown.root.columns.map((column) => column.name)).toEqual(["sequence", "timestamp_ns", "cpu_percent", "memory_usage_bytes"]);
    expect(shown.root.kind === "table" && runText(shown.root.rows[0]!.flat())).toContain("22:13:21.000");
    expect(shown.root.kind === "table" && runText(shown.root.rows[0]!.flat())).toContain("1.95 MB");
    expect(shown.offers).not.toContain("chart");
    expect(shown.root.more?.columns).toContain("container");
  });

  it("should_GiveTheSameResult_When_TheDirectoryIsCopiedIntoAFreshDataHome", () => {
    // Arrange
    const copied = home(STATS_ENTRY).map((file) => ({ ...file }));
    // Act
    const original = table(Registry.core().withHome(home(STATS_ENTRY)));
    const fresh = table(Registry.core().withHome(copied));
    // Assert
    expect(fresh).toEqual(original);
  });

  it("should_IgnoreTheEntryWithATailNotice_When_ItNamesAnUnknownKind", () => {
    // Arrange
    const registry = Registry.core().withHome(home(STATS_ENTRY.replace("kind: table", "kind: sankey")));
    // Act
    const shown = table(registry);
    // Assert
    expect(shown.root.kind).toBe("table");
    expect(shown.root.kind === "table" && shown.root.columns.map((column) => column.name)).toContain("container");
    expect(shown.notices).toContain("presentation docker-stats-sample.yaml: unknown kind: sankey");
  });

  it("should_KeepTheLastValidEntry_When_AnUpdateBreaksTheFile", () => {
    // Arrange
    const valid = Registry.core().withHome(home(STATS_ENTRY));
    // Act
    const broken = valid.withHome(home(STATS_ENTRY.replace("unit: ns", "unit: fortnights")));
    // Assert
    expect(table(broken).root).toEqual(table(valid).root);
    expect(broken.problems[0]?.message).toMatch(/unit ns, us, ms or s/);
  });

  it("should_RejectUnitsGuessedFromNames_When_ATimeFormatHasNoUnit", () => {
    expect(() => parseEntry(STATS_ENTRY.replace("{kind: time, unit: ns, epoch: unix}", "time"), "home:x.yaml")).toThrow(/formatter|unit/);
  });

  it("should_TakeTheStructuralRule_When_TheDeclaredFieldsDoNotMatch", () => {
    // Arrange
    const registry = Registry.core().withHome(home(STATS_ENTRY.replace("sequence: Int", "sequence: Text")));
    // Act / Assert
    expect(columnsOf(registry)).toContain("container");
  });

  it("should_ParseOnlyData_When_AFileCarriesATag", () => {
    expect(() => parseEntry("!!js/function 'return 1'", "home:evil.yaml")).toThrow();
  });

  it("should_UseCoreAgain_When_ADataHomeOverrideIsRemoved", () => {
    const registry = Registry.core();
    const http: TypeShape = { kind: "record", name: "HttpResponse", fields: [
      { name: "status", type: INT }, { name: "version", type: TEXT },
      { name: "headers", type: { kind: "list", element: { kind: "record", name: "HttpHeader", fields: [{ name: "name", type: TEXT }, { name: "value", type: TEXT }] } } },
      { name: "body", type: { kind: "primitive", name: "BYTES" } },
    ] };
    expect(registry.match(http)?.entry.kind).toBe("http");
    const overridden = registry.withHome([{name:"response.yaml",text:"version: 1\ntype: HttpResponse\nfields: {status: Int}\npresent: {kind: fields}\n"}]);
    expect(overridden.match(http)?.entry.kind).toBe("fields");
    expect(overridden.match(http)?.entry.origin).toBe("home:response.yaml");
    expect(overridden.withHome([]).match(http)?.entry.kind).toBe("http");
  });
});

describe("structural entries and the type format", () => {
  it("should_MatchARecordWithoutAName_When_AStarEntryDeclaresItsFields", () => {
    // Arrange: a data-home entry for an unnamed record, matched by the fields it declares
    const registry = Registry.core().withHome([{ name: "probe.yaml", text: "version: 1\ntype: '*'\napplies: [record]\nfields:\n  probe: Text\n  expression: Text\npresent:\n  kind: fields\n  formats:\n    expression: type\n" }]);
    const data = { probe: "p1", expression: "{ a: Int }" };
    // Act / Assert: matched by keys; a record missing a declared field is not; lists never are
    expect(registry.match({ kind: "unknown" }, data)?.entry.formats.expression).toEqual({ kind: "type" });
    expect(registry.match({ kind: "unknown" }, { probe: "p1" })).toBeUndefined();
    expect(registry.match({ kind: "list", element: { kind: "unknown" } }, [data])).toBeUndefined();
  });

  it("should_RefuseAStarEntryWithoutFields_And_AcceptTheTypeFormat", () => {
    // Arrange / Act / Assert
    expect(() => parseEntry("version: 1\ntype: '*'\npresent:\n  kind: fields\n", "t")).toThrow(/declare the fields/);
    const entry = parseEntry("version: 1\ntype: Shape\nfields:\n  expression: Text\npresent:\n  kind: fields\n  formats:\n    expression: type\n", "t");
    expect(entry.formats.expression).toEqual({ kind: "type" });
  });
});
