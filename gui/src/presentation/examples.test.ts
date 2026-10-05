import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import type { TypeShape } from "../protocol";
import { prepareSync } from "./prepare";
import { present, runText, tailOf } from "./present";
import { parseEntry, Registry } from "./registry";

/*
 * The runnable example's actual files (examples/presentations), validated and applied to a
 * synthetic list of samples: invented container ids and numbers, never a user's workspace.
 */
const read = (name: string) => readFileSync(new URL(`../../../examples/presentations/${name}`, import.meta.url), "utf8");
const INT: TypeShape = { kind: "primitive", name: "INT" };
const DECIMAL: TypeShape = { kind: "primitive", name: "DECIMAL" };
const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const opt = (element: TypeShape): TypeShape => ({ kind: "option", element });
const sample: TypeShape = { kind: "record", name: "DockerStatsSample", fields: [
  { name: "container", type: TEXT }, { name: "sequence", type: INT }, { name: "timestamp_ns", type: opt(INT) },
  { name: "cpu_percent", type: opt(DECIMAL) }, { name: "memory_usage_bytes", type: opt(INT) }, { name: "memory_limit_bytes", type: opt(INT) },
  { name: "memory_percent", type: opt(DECIMAL) }, { name: "online_cpus", type: opt(INT) }, { name: "cpu_unavailable", type: opt(TEXT) },
] };
const some = (value: unknown) => ({ kind: "some", value });
const samples = [1, 2, 3].map((at) => ({
  container: "a".repeat(64), sequence: at, timestamp_ns: some(1_700_000_000_000_000_000 + at * 1_004_000_000),
  cpu_percent: some(at / 10), memory_usage_bytes: some(1_953_792), memory_limit_bytes: some(67_108_864),
  memory_percent: some(2.9), online_cpus: some(4), cpu_unavailable: { kind: "none" },
}));
const files = [{ name: "docker-stats-sample.yaml", text: read("docker-stats-sample.yaml") }, { name: "unknown-kind.yaml", text: read("unknown-kind.yaml") }];

describe("examples/presentations", () => {
  it("should_ValidateTheStatsEntryAndRejectTheUnknownKind_When_TheExampleFilesAreRead", () => {
    expect(parseEntry(files[0]!.text, "home:docker-stats-sample.yaml").kind).toBe("table");
    expect(() => parseEntry(files[1]!.text, "home:unknown-kind.yaml")).toThrow("unknown kind: sankey");
  });

  it("should_ShowTheExpectedTable_When_TheDirectoryIsCopiedIntoADataHome", () => {
    // Arrange
    const registry = Registry.core().withHome(files);
    // Act
    const shown = present({
      prepared: prepareSync({ type: { kind: "list", element: sample }, data: samples }), registry,
      context: { mode: "preview", columns: 118, lines: 6, density: "normal", locale: "en-GB", timeZone: "UTC" },
    });
    // Assert — the outcome the README promises.
    expect(shown.root.kind === "table" && shown.root.columns.map((column) => column.name))
      .toEqual(["sequence", "timestamp_ns", "cpu_percent", "memory_usage_bytes", "memory_percent", "online_cpus"]);
    expect(shown.root.kind === "table" && runText(shown.root.rows[0]!.flat())).toMatch(/\d\d:\d\d:\d\d\.\d{3}.*1\.95 MB/);
    expect(runText(tailOf(shown))).toContain("not shown: container, memory_limit_bytes, cpu_unavailable");
    expect(shown.offers).not.toContain("chart");
    expect(registry.noticesFor({ kind: "record", name: "DockerLogTail", fields: [] })).toEqual(["presentation unknown-kind.yaml: unknown kind: sankey"]);
  });
});
