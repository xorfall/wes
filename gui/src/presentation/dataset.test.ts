import { describe, expect, it } from "vitest";
import { ExactNumber } from "../exact-json";
import { describeType, type StoredValue, type TypeShape } from "../protocol";
import { compactType, typeOutline, typeOutlineSegments } from "../surface/result-type";
import { typedJsonSummary } from "../surface/render/JsonTree";
import { width } from "./columns";
import { datasetFacts, decodeDatasetData, decodeDatasetReference, recordsText } from "./dataset";
import { cleanType, prepareSync } from "./prepare";
import { present, runText } from "./present";
import { readableData } from "./readable";
import { Registry } from "./registry";
import { summarize } from "./summary";
import { typeLine, typeShapeOf, typeStructure } from "./type-shape";
import type { Context, PresentationNode } from "./types";

/* Synthetic descriptors only: invented identifiers and digests that name no real store. */

const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const INT: TypeShape = { kind: "primitive", name: "INT" };
const row: TypeShape = { kind: "record", name: "SyntheticRow", fields: [{ name: "label", type: TEXT }, { name: "count", type: INT }] };
const anonymous: TypeShape = { kind: "record", name: "", fields: [{ name: "label", type: TEXT }, { name: "note", type: { kind: "option", element: TEXT } }] };
const dataset = (element: TypeShape): TypeShape => ({ kind: "dataset", element });
const option = (element: TypeShape): TypeShape => ({ kind: "option", element });
const list = (element: TypeShape): TypeShape => ({ kind: "list", element });

const DIGEST_A = `sha256:${"0123456789abcdef".repeat(4)}`;
const DIGEST_B = `sha256:${"fedcba9876543210".repeat(4)}`;
const reference = (over: Record<string, unknown> = {}): Record<string, unknown> => ({
  store: "00000000-0000-4000-8000-0000000000a1",
  dataset: "00000000-0000-4000-8000-0000000000b2",
  generation: "3",
  manifest: "00000000-0000-4000-8000-0000000000c3",
  manifestDigest: DIGEST_A,
  manifestBytes: "512",
  schemaDigest: DIGEST_B,
  records: "1234567",
  authorizationGeneration: "2",
  ...over,
});
const envelope = (ref: unknown = reference()) => ({ kind: "dataset", reference: ref });

const context = (over: Partial<Context> = {}): Context => ({
  mode: "preview", columns: 118, lines: 6, density: "normal", locale: "en-GB", timeZone: "UTC", ...over,
});
const show = (value: Pick<StoredValue, "type" | "data">, over: Partial<Context> = {}) =>
  present({ prepared: prepareSync(value), context: context(over), registry: Registry.core() });
/** A descriptor row as drawn: the name column fits the longest field name, `authorizationGeneration`. */
const field = (name: string, value: string) => `${name.padEnd("authorizationGeneration".length)}  ${value}`;

function textOf(node: PresentationNode): string[] {
  switch (node.kind) {
    case "line": return [runText(node.runs)];
    case "fields": return node.rows.flatMap((it) => {
      const inner = textOf(it.node);
      return [`${it.name.padEnd(node.nameWidth)}  ${inner[0] ?? ""}`.trimEnd(), ...inner.slice(1)];
    });
    case "table": return [
      node.columns.map((column) => column.name).join(" | "),
      ...node.rows.map((cells) => cells.map(runText).join(" | ")),
    ];
    case "empty": return [node.text];
    case "nested": return [`${node.disclosure === "folds" ? "▾" : "▸"} ${node.summary}`, ...(node.body ? textOf(node.body) : [])];
    default: return [node.kind];
  }
}

describe("decoding a dataset descriptor", () => {
  it("should_ReturnTheExactFrozenReference_When_TheDescriptorIsCanonical", () => {
    // Arrange
    const raw = envelope();
    // Act
    const decoded = decodeDatasetData(raw);
    // Assert
    expect(decoded).toEqual(reference());
    expect(Object.isFrozen(decoded)).toBe(true);
    expect(decoded).not.toBe(raw.reference);
  });

  it("should_KeepCountsExact_When_TheyExceedTheSafeIntegerRange", () => {
    // Arrange
    const beyondSafe = reference({ records: "9007199254740993" });
    const largest = reference({ records: "18446744073709551615", generation: "18446744073709551615" });
    // Act
    const beyond = decodeDatasetReference(beyondSafe)!, maximum = decodeDatasetReference(largest)!;
    // Assert
    expect(recordsText(beyond)).toBe("9 007 199 254 740 993 records");
    expect(recordsText(maximum)).toBe("18 446 744 073 709 551 615 records");
    expect(maximum.generation).toBe("18446744073709551615");
  });

  it("should_AcceptAnEmptyDataset_When_ItHasZeroCommittedRecords", () => {
    // Arrange
    const empty = reference({ records: "0" }), single = reference({ records: "1" });
    // Act
    const texts = [decodeDatasetReference(empty)!, decodeDatasetReference(single)!].map(recordsText);
    // Assert
    expect(texts).toEqual(["0 records", "1 record"]);
  });

  const withoutRecords = Object.fromEntries(Object.entries(reference()).filter(([name]) => name !== "records"));
  it.each<[string, unknown]>([
    ["a missing field", withoutRecords],
    ["an extra field", reference({ path: "/synthetic/elsewhere" })],
    ["a count sent as a JSON number", reference({ records: 12 })],
    ["a count sent as an exact JSON number", reference({ records: new ExactNumber("12345678901234567890") })],
    ["a count with a leading zero", reference({ records: "012" })],
    ["a signed count", reference({ records: "+12" })],
    ["a negative count", reference({ records: "-1" })],
    ["a fractional count", reference({ records: "1.0" })],
    ["an exponent count", reference({ records: "1e3" })],
    ["a padded count", reference({ records: " 12" })],
    ["an empty count", reference({ records: "" })],
    ["a count past the unsigned 64-bit range", reference({ records: "18446744073709551616" })],
    ["a count of 21 digits", reference({ records: "100000000000000000000" })],
    ["a zero generation", reference({ generation: "0" })],
    ["a zero manifest size", reference({ manifestBytes: "0" })],
    ["a zero authorization generation", reference({ authorizationGeneration: "0" })],
    ["an uppercase identifier", reference({ store: "00000000-0000-4000-8000-0000000000A1" })],
    ["an identifier without hyphens", reference({ dataset: "000000000000400080000000000000b2" })],
    ["a braced identifier", reference({ manifest: "{00000000-0000-4000-8000-0000000000c3}" })],
    ["a digest without its algorithm", reference({ manifestDigest: "0123456789abcdef".repeat(4) })],
    ["an uppercase digest", reference({ schemaDigest: DIGEST_B.toUpperCase() })],
    ["a short digest", reference({ schemaDigest: DIGEST_B.slice(0, -1) })],
    ["a digest of another algorithm", reference({ manifestDigest: DIGEST_A.replace("sha256", "sha512") })],
    ["an array", []],
    ["null", null],
    ["a string", "00000000-0000-4000-8000-0000000000b2"],
  ])("should_RefuseTheWholeReference_When_ItHas%s", (_case, raw) => {
    // Act
    const decoded = decodeDatasetReference(raw);
    // Assert
    expect(decoded).toBeUndefined();
  });

  it.each<[string, unknown]>([
    ["an extra envelope field", { ...envelope(), records: "1" }],
    ["another kind", { kind: "list", reference: reference() }],
    ["no reference", { kind: "dataset" }],
    ["an array", [envelope()]],
    ["a non-plain object", Object.assign(Object.create({ inherited: true }), envelope())],
  ])("should_RefuseTheEnvelope_When_ItHas%s", (_case, raw) => {
    // Act
    const decoded = decodeDatasetData(raw);
    // Assert
    expect(decoded).toBeUndefined();
  });
});

describe("Dataset types", () => {
  it("should_PrintDatasetWrappers_When_DescribingNestedOptionAndRecordElements", () => {
    // Arrange
    const shape = option(dataset(anonymous));
    // Act
    const line = typeLine(shape), whole = typeStructure(dataset(row)), described = describeType(dataset(row));
    // Assert
    expect(line).toBe("Option<Dataset<{ label: Text, note: Option<Text> }>>");
    expect(whole).toBe("Dataset<SyntheticRow {\n  label: Text\n  count: Int\n}>");
    expect(described).toBe("Dataset<SyntheticRow>");
  });

  it("should_RetainEveryWrapper_When_TheCompactTypeNarrows", () => {
    // Arrange
    const shape = option(dataset(anonymous));
    for (let columns = 1; columns <= 40; columns++) {
      // Act
      const shown = compactType(shape, columns);
      // Assert
      if (shown === "type") continue;
      expect(width(shown)).toBeLessThanOrEqual(columns);
      expect(shown.startsWith("Option<Dataset<")).toBe(true);
      expect(shown.endsWith(">>")).toBe(true);
    }
  });

  it("should_AcceptOnlyAStrictDatasetShape_When_ReadingATypeDescribedAsData", () => {
    // Arrange
    const valid = { kind: "dataset", element: { kind: "primitive", name: "TEXT" } };
    // Act
    const read = typeShapeOf(valid);
    // Assert
    expect(read).toEqual(dataset(TEXT));
    expect(typeShapeOf({ kind: "dataset" })).toBeUndefined();
    expect(typeShapeOf({ ...valid, records: "1" })).toBeUndefined();
    expect(typeShapeOf({ kind: "dataset", element: { kind: "table" } })).toBeUndefined();
  });

  it("should_KeepTheDatasetKind_When_CleaningAWireType", () => {
    // Act
    const cleaned = cleanType({ kind: "dataset", element: { kind: "record", name: "SyntheticRow", fields: [{ name: "label", type: { kind: "primitive", name: "TEXT" } }] } });
    // Assert
    expect(cleaned).toEqual(dataset({ kind: "record", name: "SyntheticRow", fields: [{ name: "label", type: TEXT }] }));
    expect(cleanType({ kind: "dataset" })).toEqual(dataset({ kind: "unknown" }));
  });

  it("should_NotAttachElementMetadata_When_TheElementIsInsideADataset", () => {
    // Arrange
    const meta = {
      version: 1 as const, contract: { name: "SyntheticRows", digest: DIGEST_A }, truncated: false,
      fields: { "/e/f:label": { contract: { name: "SyntheticLabel", digest: DIGEST_B }, kind: "text" as const } },
    };
    // Act
    const asList = typeOutline(list(row), meta), asDataset = typeOutline(dataset(row), meta);
    // Assert
    expect(asList).toContain("SyntheticLabel");
    expect(asDataset).not.toContain("SyntheticLabel");
    expect(typeOutlineSegments(dataset(row)).map(segment => segment.text).join("")).toBe(typeStructure(dataset(row)));
  });
});

describe("presenting a Dataset", () => {
  it("should_ShowTheCommittedCountAndIdentity_When_PreviewingADescriptor", () => {
    // Arrange
    const value = { type: dataset(row), data: envelope(reference({ records: "9007199254740993" })) };
    // Act
    const shown = show(value);
    // Assert
    expect(shown.root.kind).toBe("fields");
    expect(textOf(shown.root)).toEqual([
      field("records", "9 007 199 254 740 993"),
      field("generation", "3"),
      field("dataset", "00000000-0000-4000-8000-0000000000b2"),
      field("store", "00000000-0000-4000-8000-0000000000a1"),
      field("manifest", "00000000-0000-4000-8000-0000000000c3"),
      field("manifestDigest", "sha256:01234567…abcdef"),
    ]);
    expect(shown.root.more).toEqual({ fields: 3, exact: true });
    expect(shown.summary.facts.map(it => it.text)).toEqual(["9 007 199 254 740 993 records"]);
  });

  it("should_ShowEveryFieldWhole_When_Expanded", () => {
    // Arrange
    const value = { type: dataset(row), data: envelope() };
    // Act
    const shown = show(value, { mode: "expanded", lines: 20 });
    // Assert
    const lines = textOf(shown.root);
    expect(lines).toHaveLength(9);
    expect(lines).toContain(field("schemaDigest", DIGEST_B));
    expect(lines).toContain(field("manifestBytes", "512"));
    expect(lines).toContain(field("authorizationGeneration", "2"));
  });

  it("should_SayTheDescriptorIsInvalid_When_ItDoesNotValidate", () => {
    // Arrange
    const value = { type: dataset(row), data: envelope(reference({ records: "-1" })) };
    // Act
    const shown = show(value, { mode: "expanded", lines: 20 });
    // Assert
    expect(shown.root).toMatchObject({ kind: "line", runs: [{ text: "invalid dataset descriptor", tone: "bad" }] });
    expect(textOf(shown.root).join("\n")).not.toContain("00000000-0000-4000-8000-0000000000a1");
    expect(shown.summary.facts).toEqual([{ text: "invalid descriptor", tone: "warn" }]);
  });

  it("should_PresentTheDescriptor_When_AnOptionHoldsIt", () => {
    // Arrange
    const some = { type: option(dataset(row)), data: { kind: "some", value: envelope() } };
    const none = { type: option(dataset(row)), data: { kind: "none" } };
    // Act
    const held = show(some), absent = show(none);
    // Assert
    expect(textOf(held.root)[0]).toBe(field("records", "1 234 567"));
    expect(held.summary.facts.map(it => it.text)).toEqual(["1 234 567 records"]);
    expect(absent.root).toMatchObject({ kind: "empty", text: "none" });
    expect(absent.summary.facts).toEqual([]);
  });

  it("should_KeepANestedDatasetClosed_Until_ItIsOpenedByHand", () => {
    // Arrange
    const result: TypeShape = { kind: "record", name: "", fields: [{ name: "label", type: TEXT }, { name: "outputs", type: dataset(row) }] };
    const value = { type: result, data: { label: "synthetic scan", outputs: envelope(reference({ records: "3" })) } };
    // Act
    const closed = show(value, { mode: "expanded", lines: 20 });
    const opened = show(value, { mode: "expanded", lines: 20, open: new Set(["/outputs"]) });
    // Assert
    expect(textOf(closed.root)).toEqual(["label    synthetic scan", "outputs  ▸ Dataset<SyntheticRow> · 3 records"]);
    const body = textOf(opened.root);
    expect(body[1]).toBe("outputs  ▾ Dataset<SyntheticRow> · 3 records");
    expect(body[2]).toBe(field("records", "3"));
  });

  it("should_NotMatchAStarEntry_When_TheDescriptorCarriesItsDeclaredKeys", () => {
    // Arrange: a data-home entry for unnamed records whose declared fields are the envelope's keys
    const registry = Registry.core().withHome([{ name: "envelope.yaml", text: "version: 1\ntype: '*'\napplies: [record]\nfields:\n  kind: Text\n  reference: Text\npresent:\n  kind: fields\n" }]);
    const data = envelope();
    // Act
    const asDataset = registry.match(dataset(row), data), asRecord = registry.match({ kind: "unknown" }, data);
    const shown = present({ prepared: prepareSync({ type: dataset(row), data }), context: context(), registry });
    // Assert
    expect(asDataset).toBeUndefined();
    expect(asRecord).toBeDefined();
    expect(textOf(shown.root)[0]).toBe(field("records", "1 234 567"));
  });

  it("should_NotReadDescriptorsAsRows_When_AListHoldsDatasets", () => {
    // Arrange
    const value = { type: list(dataset(row)), data: [envelope(reference({ records: "1" })), envelope(reference({ records: "2" }))] };
    // Act
    const shown = show(value);
    // Assert
    expect(shown.root.kind).toBe("fields");
    expect(textOf(shown.root)).toEqual(["[0]  ▸ Dataset<SyntheticRow> · 1 record", "[1]  ▸ Dataset<SyntheticRow> · 2 records"]);
  });

  it("should_SummarizeEachDatasetCell_When_ATableRowHoldsOne", () => {
    // Arrange
    const scan: TypeShape = { kind: "record", name: "", fields: [{ name: "label", type: TEXT }, { name: "outputs", type: dataset(row) }] };
    const value = { type: list(scan), data: [
      { label: "first", outputs: envelope(reference({ records: "18446744073709551615" })) },
      { label: "second", outputs: envelope(reference({ manifestBytes: "0" })) },
    ] };
    // Act: the window's cell width keeps the full exact count on one line.
    const shown = show(value, { mode: "window", lines: 20 });
    // Assert
    expect(shown.root.kind).toBe("table");
    expect(textOf(shown.root)).toEqual([
      "label | outputs",
      "first | Dataset<SyntheticRow> · 18 446 744 073 709 551 615 records",
      "second | Dataset<SyntheticRow> · invalid descriptor",
    ]);
  });
});

describe("Dataset facts and projections", () => {
  it("should_StateTheCommittedCount_When_SummarizingForTheHeader", () => {
    // Arrange
    const data = envelope(reference({ records: "12345678901234567890" }));
    // Act
    const facts = summarize(dataset(row), data), wrapped = summarize(option(dataset(row)), { kind: "some", value: data });
    // Assert
    expect(facts).toEqual([{ text: "12 345 678 901 234 567 890 records", tone: "dim" }]);
    expect(wrapped).toEqual(facts);
    expect(summarize(dataset(row), { kind: "dataset" })).toEqual([{ text: "invalid descriptor", tone: "warn" }]);
    expect(datasetFacts(undefined)).toEqual([{ text: "invalid descriptor", tone: "warn" }]);
  });

  it("should_PassTheDescriptorThroughUnchanged_When_Preparing", () => {
    // Arrange
    const data = envelope();
    // Act
    const prepared = prepareSync({ type: dataset(row), data });
    // Assert
    expect(prepared.data).toBe(data);
    expect(prepared.type).toEqual(dataset(row));
  });

  it("should_ProjectTheDescriptorAsJson_When_Valid_And_NameItInvalid_Otherwise", () => {
    // Arrange
    const valid = envelope(), invalid = envelope(reference({ store: "not-an-identifier" }));
    // Act
    const readable = readableData(dataset(row), valid), refused = readableData(dataset(row), invalid);
    // Assert
    expect(readable).toBe(valid);
    expect(refused).toEqual({ type: "Dataset<SyntheticRow>", note: "Invalid dataset descriptor" });
  });

  it("should_SummarizeTheDescriptor_When_TheJsonTreeShowsADatasetField", () => {
    // Act
    const valid = typedJsonSummary(envelope(), dataset(row)), invalid = typedJsonSummary({ kind: "dataset", reference: [] }, dataset(row));
    // Assert
    expect(valid).toBe("Dataset<SyntheticRow> · 1 234 567 records");
    expect(invalid).toBe("Dataset<SyntheticRow> · invalid descriptor");
    expect(typedJsonSummary(undefined, dataset(row))).toBe("missing");
  });
});
