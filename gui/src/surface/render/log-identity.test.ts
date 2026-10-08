import { describe, expect, it } from "vitest";
import { declaredItemKeys, logShape } from "./log-identity";
import { logRows } from "./LogView";
import { ExactNumber } from "../../exact-json";
import { parseEntry, Registry } from "../../presentation/registry";
import type { StoredValue } from "../../protocol";

/** A synthetic build log keyed by a nested digest and an exact ordinal. */
const ENTRY = `version: 1
type: BuildLogLine
applies: [list]
fields:
  source: SourceRef
  ordinal: Int
  text: Text
present:
  kind: log
  mapping:
    key: [source.digest, ordinal]
    text: text
`;
const mapping = parseEntry(ENTRY, "home:build-log.yaml").log!;
const type = { kind: "list", element: { kind: "record", name: "BuildLogLine", fields: [
  { name: "source", type: { kind: "record", name: "SourceRef", fields: [] } },
  { name: "ordinal", type: { kind: "primitive", name: "INT" } },
  { name: "text", type: { kind: "primitive", name: "TEXT" } },
] } } as StoredValue["type"];
const line = (digest: unknown, ordinal: unknown, text = "synthetic line") => ({ source: { digest }, ordinal, text });
const list = (...data: unknown[]): StoredValue => ({ type, provenance: {}, data });
const docker = (...data: unknown[]): StoredValue => ({ type: { kind: "list", element: { kind: "record", name: "DockerLogEvent", fields: [] } }, provenance: {}, data });
const event = (sequence: unknown, container = "fixture") => ({ container, sequence, received_at_ns: new ExactNumber("1790955000000000000"), timestamp_ns: null, stream: "stdout", text: "synthetic", partial: false, lossy: false, line_truncated: false });

describe("declared item identity", () => {
  it("should_ReadTheSameNestedExactKey_When_TheLogViewAndTheLiveWindowReadOneMappedValue", () => {
    // Arrange
    const big = "90071992547409930001";
    const value = list(line("d1", new ExactNumber(big)), line({ kind: "some", value: "d2" }, new ExactNumber("1")));
    // Act
    const keys = declaredItemKeys(value, mapping);
    // Assert
    expect([...keys!.values()]).toEqual([JSON.stringify(["d1", big]), JSON.stringify(["d2", "1"])]);
    expect([...keys!.values()]).toEqual(logRows(value, mapping).map((row) => row.key));
  });

  it("should_KeepAnItemsKey_When_ItsPositionChangesBetweenSamples", () => {
    // Arrange
    const first = list(line("d1", new ExactNumber("7")), line("d1", new ExactNumber("8")));
    const next = list(line("d1", new ExactNumber("8")), line("d1", new ExactNumber("9")));
    // Act
    const before = declaredItemKeys(first, mapping)!, after = declaredItemKeys(next, mapping)!;
    // Assert
    expect(after.get(0)).toBe(before.get(1));
  });

  it("should_HaveNoItemKeys_When_AnyMappedIdentityIsMissingMalformedOrRepeated", () => {
    // Arrange
    const valid = line("d1", new ExactNumber("1"));
    const malformed = [
      list(valid, { ordinal: new ExactNumber("2"), text: "no source" }),
      list(valid, line({ kind: "none" }, new ExactNumber("2"))),
      list(valid, line({ nested: "record" }, new ExactNumber("2"))),
      list(valid, line("d1", [2])),
      list(valid, ["not", "a", "record"]),
      list(valid, null),
      list(valid, line("d1", new ExactNumber("1"), "a repeated key")),
    ];
    // Act
    const keys = malformed.map((value) => declaredItemKeys(value, mapping));
    // Assert
    expect(keys).toEqual(malformed.map(() => undefined));
  });

  it("should_HaveNoItemKeys_When_AListHasNeitherABuiltinContractNorAMapping", () => {
    // Arrange
    const value = list(line("d1", new ExactNumber("1")));
    // Act
    const keys = declaredItemKeys(value);
    // Assert
    expect(keys).toBeUndefined();
  });

  it("should_ShareDockerIdentityWithTheLogView_When_SequencesAreExactAndRefuseNegativeOrRepeatedOnes", () => {
    // Arrange
    const exact = docker(event(new ExactNumber("123456789012345678901")), event(8));
    // Act
    const keys = declaredItemKeys(exact);
    // Assert
    expect([...keys!.values()]).toEqual(logRows(exact).map((row) => row.key));
    expect(keys!.get(0)).toBe(JSON.stringify(["fixture", "123456789012345678901"]));
    expect(declaredItemKeys(docker(event(-1)))).toBeUndefined();
    expect(declaredItemKeys(docker(event("7")))).toBeUndefined();
    expect(declaredItemKeys(docker(event(1), event(1)))).toBeUndefined();
  });

  it("should_FindTheSameLogShapeAsTheRegistry_When_TheEntryMatchesExactly", () => {
    // Arrange
    const registry = Registry.core().withHome([{ name: "build-log.yaml", text: ENTRY }]);
    const renamed = { ...type, element: { ...(type as { element: object }).element, fields: [{ name: "text", type: { kind: "primitive", name: "TEXT" } }] } } as StoredValue["type"];
    // Act
    const shapes = [logShape(list(), registry), logShape({ ...list(), type: renamed }, registry), logShape(docker(), registry), logShape(list(), Registry.core())];
    // Assert
    expect(shapes).toEqual([{ mapping }, undefined, {}, undefined]);
  });
});
