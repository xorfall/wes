import { afterEach, describe, expect, it, vi } from "vitest";
import { datasetQuery, decodeDatasetHead, decodeDatasetRead } from "./dataset-read";
import { Engine } from "./engine";
import { decodeDatasetReference, type DatasetReference } from "./presentation/dataset";
import type { TypeShape } from "./protocol";

/*
 * Synthetic forensic coverage only: invented identifiers, digests, offsets and excerpt bytes that
 * describe no real source, store or analysis.
 */

const U64_MAX = "18446744073709551615";
const reference = (records = "0"): DatasetReference => decodeDatasetReference({
  store: "00000000-0000-4000-8000-0000000000a1", dataset: "00000000-0000-4000-8000-0000000000b2",
  generation: "3", manifest: "00000000-0000-4000-8000-0000000000c3",
  manifestDigest: `sha256:${"0123456789abcdef".repeat(4)}`, manifestBytes: "512",
  schemaDigest: `sha256:${"fedcba9876543210".repeat(4)}`, records, authorizationGeneration: "1",
})!;
const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const FIELDS = ["kind", "reason", "recordOrdinal", "sourceStart", "sourceEnd", "delimiterStart", "delimiterEnd", "unterminated",
  "reasonStart", "reasonEnd", "excerpt", "excerptStart", "excerptTruncated"];
const REJECTION: TypeShape = { kind: "record", name: "ScanRejection", fields: FIELDS.map(name => ({ name,
  type: name === "unterminated" || name === "excerptTruncated" ? { kind: "primitive", name: "BOOL" } : name === "excerpt" ? { kind: "primitive", name: "BYTES" } : TEXT })) };

/** One skipped record: `content` bytes from `start`, then `delimiter` bytes; an excerpt of at most `bound`. */
function skipped(ordinal: string, record: string, start: bigint, content: bigint, delimiter: bigint, bound = 4n) {
  const end = start + content, delimiterEnd = end + delimiter, kept = content < bound ? content : bound;
  return { ordinal, sourceStart: start.toString(), sourceEnd: delimiterEnd.toString(), value: { type: REJECTION, provenance: {}, data: {
    kind: "rejected", reason: "invalid_utf8", recordOrdinal: record, sourceStart: start.toString(), sourceEnd: end.toString(),
    delimiterStart: end.toString(), delimiterEnd: delimiterEnd.toString(), unterminated: delimiter === 0n,
    reasonStart: start.toString(), reasonEnd: (start + 1n).toString(), excerpt: btoa("ÿ".repeat(Number(kept))),
    excerptStart: start.toString(), excerptTruncated: kept < content,
  } } };
}
/** Three skipped records of 7, 4 and 5 original bytes: source records 2, 5 and 9, the last unterminated. */
const ROWS = [skipped("0", "2", 10n, 6n, 1n), skipped("1", "5", 40n, 3n, 1n), skipped("2", "9", 90n, 5n, 0n)];
const COVERAGE = { records: "3", inputBytes: "16", lastOrdinal: "9", through: "95", excerptBytes: "4", segmentBytes: "512" };

const inspection = (over: Record<string, unknown> = {}) => ({
  reference: { ...reference() }, stream: "outputs", lifecycle: "sealed", protected: false, persistence: "Durable", segmentBytes: "0", coverage: COVERAGE, ...over,
});
const coveragePage = (rows: readonly unknown[] = ROWS, page: Record<string, unknown> = {}, over: Record<string, unknown> = {}) => inspection({
  stream: "coverage", page: { first: "0", next: String(rows.length), rows, extentExhausted: true, limitedBy: null, cursor: null, ...page }, ...over,
});
/** `raw` without `key`: absent, not present as undefined. */
const without = (raw: Record<string, unknown>, key: string) => Object.fromEntries(Object.entries(raw).filter(([name]) => name !== key));
/** `ROWS` with row `at`'s record fields patched. */
const patched = (at: number, data: Record<string, unknown>, wrapper: Record<string, unknown> = {}): unknown[] =>
  ROWS.map((row, index) => index !== at ? row : { ...row, ...wrapper, value: { ...row.value, data: { ...row.value.data, ...data } } });

describe("forensic coverage decoding", () => {
  it("reads coverage beside outputs and never counts it as the Dataset's records", () => {
    const read = decodeDatasetRead(inspection(), reference())!;
    expect(read.stream).toBe("outputs");
    expect(read.coverage).toEqual(COVERAGE);
    expect(read.reference.records).toBe("0");
    expect(decodeDatasetRead(without(inspection(), "coverage"), reference())).not.toHaveProperty("coverage");
    // A head inspection is of outputs and may carry the head's coverage.
    expect(decodeDatasetHead(inspection(), reference(), reference())?.coverage).toEqual(COVERAGE);
  });

  it("accepts an empty forensic tree and exact counts at the top of the unsigned range", () => {
    const empty = { records: "0", inputBytes: "0", lastOrdinal: null, through: null, excerptBytes: "4096", segmentBytes: "0" };
    expect(decodeDatasetRead(inspection({ coverage: empty }), reference())?.coverage).toEqual(empty);
    expect(decodeDatasetRead(coveragePage([], {}, { coverage: empty }), reference(), 10, "coverage")?.page?.rows).toEqual([]);
    const top = skipped("0", "18446744073709551614", 18446744073709551610n, 3n, 2n, 1n);
    const at = { records: "1", inputBytes: "5", lastOrdinal: "18446744073709551614", through: U64_MAX, excerptBytes: "1", segmentBytes: U64_MAX };
    const read = decodeDatasetRead(coveragePage([top], {}, { coverage: at }), reference(), 1, "coverage")!;
    expect(read.page!.rows[0]!.rejection).toMatchObject({ recordOrdinal: "18446744073709551614", delimiterEnd: U64_MAX, excerptSize: "1", excerptTruncated: true, unterminated: false });
  });

  it("refuses coverage that is not one consistent count", () => {
    const refused: [string, Record<string, unknown>][] = [
      ["no excerpt bound", { excerptBytes: "0" }], ["excerpt bound beyond 4096", { excerptBytes: "4097" }],
      ["fewer bytes than records", { inputBytes: "2" }], ["more bytes than the end byte", { inputBytes: "96" }],
      ["last record too early for the count", { lastOrdinal: "1" }], ["zero end byte", { through: "0" }],
      ["unknown last record", { lastOrdinal: null }], ["unknown end byte", { through: null }],
      ["numeric count", { records: 3 }], ["leading zero", { inputBytes: "016" }], ["beyond u64", { segmentBytes: "18446744073709551616" }],
      ["surplus key", { interpreted: "0" }], ["empty with bytes", { records: "0", lastOrdinal: null, through: null }],
      ["empty with a last record", { records: "0", inputBytes: "0", through: null }],
    ];
    for (const [name, over] of refused) expect(decodeDatasetRead(inspection({ coverage: { ...COVERAGE, ...over } }), reference()), name).toBeUndefined();
    const { segmentBytes: _omitted, ...missing } = COVERAGE;
    expect(decodeDatasetRead(inspection({ coverage: missing }), reference())).toBeUndefined();
  });

  it("refuses a reply of another stream, without a stream, or with both recording and coverage", () => {
    expect(decodeDatasetRead(without(inspection(), "stream"), reference())).toBeUndefined();
    expect(decodeDatasetRead(inspection({ stream: "coverage" }), reference())).toBeUndefined();
    expect(decodeDatasetRead(inspection({ stream: "Outputs" }), reference())).toBeUndefined();
    expect(decodeDatasetRead(coveragePage(), reference(), 10)).toBeUndefined();
    expect(decodeDatasetRead({ ...coveragePage(), stream: "outputs" }, reference(), 10, "coverage")).toBeUndefined();
    // Skipped records are only read where an analysis tree has coverage.
    expect(decodeDatasetRead(without(coveragePage([]), "coverage"), reference(), 10, "coverage")).toBeUndefined();
    const recording = { run: "00000000-0000-4000-8000-0000000000e1", epoch: reference().dataset, first: "1", acceptedThrough: "0", committedThrough: "0", pending: "0", rejected: "0", termination: "natural" };
    expect(decodeDatasetRead({ ...without(inspection(), "coverage"), recording }, reference())?.recording).toBeDefined();
    expect(decodeDatasetRead(inspection({ recording }), reference())).toBeUndefined();
  });

  it("bounds a skipped-record page by the coverage's count, not the descriptor's", () => {
    // No outputs, three skipped records: the page is bounded by three.
    const read = decodeDatasetRead(coveragePage(), reference("0"), 10, "coverage")!;
    expect(read.page!.rows.map(row => [row.ordinal, row.rejection!.recordOrdinal])).toEqual([["0", "2"], ["1", "5"], ["2", "9"]]);
    const partial = coveragePage(ROWS.slice(0, 2), { next: "2", extentExhausted: false, cursor: "c2", limitedBy: "bytes" });
    expect(decodeDatasetRead(partial, reference("0"), 10, "coverage")?.page?.cursor).toBe("c2");
    const refused: [string, unknown, DatasetReference?][] = [
      ["exhausted before the coverage ends", coveragePage(ROWS.slice(0, 2), { next: "2" })],
      ["cursor at the coverage end", coveragePage(ROWS, { extentExhausted: false, cursor: "c3" })],
      ["beyond the coverage count", coveragePage([...ROWS, skipped("3", "12", 120n, 1n, 1n)])],
      // The descriptor's own count is not the bound either way.
      ["descriptor count used as bound", coveragePage(ROWS.slice(0, 2), { next: "2" }, { reference: { ...reference("2") } }), reference("2")],
      ["gap between coverage ordinals", coveragePage([ROWS[0], ROWS[2]], { next: "2", extentExhausted: false, cursor: "c2" })],
    ];
    for (const [name, raw, expected] of refused) expect(decodeDatasetRead(raw, expected ?? reference("0"), 10, "coverage"), name).toBeUndefined();
    expect(decodeDatasetRead(coveragePage(), reference("0"), 2, "coverage")).toBeUndefined();
  });

  it("reads each skipped record exactly as native framing wrote it", () => {
    const [first, , last] = decodeDatasetRead(coveragePage(), reference(), 10, "coverage")!.page!.rows;
    expect(first!.rejection).toEqual({
      reason: "invalid_utf8", recordOrdinal: "2", sourceStart: "10", sourceEnd: "16", delimiterStart: "16", delimiterEnd: "17", unterminated: false,
      reasonStart: "10", reasonEnd: "11", excerpt: btoa("ÿ".repeat(4)), excerptSize: "4", excerptTruncated: true,
    });
    expect(first!.sourceEnd).toBe("17");
    expect(last!.rejection).toMatchObject({ unterminated: true, delimiterStart: "95", delimiterEnd: "95", excerptSize: "4", excerptTruncated: true });
    // The typed value stays the generic record it is.
    expect(first!.value.type).toEqual(REJECTION);
  });

  it("refuses a skipped record that is not one native framing rejection", () => {
    const refused: [string, unknown[]][] = [
      ["another kind", patched(0, { kind: "accepted" })],
      ["unknown reason", patched(0, { reason: "parse_error" })],
      ["noncanonical offset", patched(0, { reasonEnd: "011" })],
      ["numeric offset", patched(0, { recordOrdinal: 2 })],
      ["delimiter apart from content", patched(0, { delimiterStart: "17", delimiterEnd: "18" }, { sourceEnd: "18" })],
      ["terminated record flagged unterminated", patched(0, { unterminated: true })],
      ["empty delimiter not flagged", patched(2, { unterminated: false })],
      ["reason outside the content", patched(0, { reasonEnd: "17" })],
      ["empty reason span", patched(0, { reasonEnd: "10" })],
      ["excerpt elsewhere", patched(0, { excerptStart: "11" })],
      ["excerpt shorter than the bound", patched(0, { excerpt: btoa("ÿÿÿ") })],
      ["untruncated excerpt flagged truncated", patched(1, { excerptTruncated: true })],
      ["truncated excerpt not flagged", patched(0, { excerptTruncated: false })],
      ["excerpt not base64", patched(0, { excerpt: "////-" })],
      ["unpadded excerpt", patched(0, { excerpt: "/w" })],
      ["row range not content to delimiter end", patched(0, {}, { sourceEnd: "16" })],
      ["row range starting elsewhere", patched(0, {}, { sourceStart: "9" })],
      ["record before its coverage ordinal", patched(1, { recordOrdinal: "0" })],
      ["record repeated", patched(1, { recordOrdinal: "2" })],
      ["overlapping the previous record", patched(1, { sourceStart: "15", reasonStart: "15", reasonEnd: "16", excerptStart: "15", sourceEnd: "18", delimiterStart: "18", delimiterEnd: "19", excerptTruncated: false }, { sourceStart: "15", sourceEnd: "19" })],
      ["no room for the records after it", patched(1, { recordOrdinal: "9" })],
      ["final record not the last one counted", patched(2, { recordOrdinal: "8" })],
      ["beyond the end byte", patched(2, { sourceEnd: "96", delimiterStart: "96", delimiterEnd: "96", excerptTruncated: true }, { sourceEnd: "96" })],
      ["surplus field", patched(0, { message: "synthetic" })],
    ];
    for (const [name, rows] of refused) expect(decodeDatasetRead(coveragePage(rows), reference(), 10, "coverage"), name).toBeUndefined();
    const { excerpt: _excerpt, ...partial } = ROWS[0]!.value.data;
    expect(decodeDatasetRead(coveragePage([{ ...ROWS[0]!, value: { ...ROWS[0]!.value, data: partial } }, ...ROWS.slice(1)] as never), reference(), 10, "coverage")).toBeUndefined();
    const renamed = { ...ROWS[0]!, value: { ...ROWS[0]!.value, type: { ...REJECTION, name: "SyntheticRow" } } };
    expect(decodeDatasetRead(coveragePage([renamed, ...ROWS.slice(1)]), reference(), 10, "coverage")).toBeUndefined();
  });

  it("refuses a skipped record whose declared field types are not the native primitives", () => {
    const retyped = (name: string, type: unknown) => {
      const shape = { ...REJECTION, fields: REJECTION.kind === "record" ? REJECTION.fields.map(field => field.name === name ? { name, type } : field) : [] };
      return [{ ...ROWS[0]!, value: { ...ROWS[0]!.value, type: shape } }, ...ROWS.slice(1)];
    };
    const refused: [string, unknown[]][] = [
      ["Text excerpt", retyped("excerpt", TEXT)],
      ["optional Bytes excerpt", retyped("excerpt", { kind: "option", element: { kind: "primitive", name: "BYTES" } })],
      ["Int offset", retyped("sourceStart", { kind: "primitive", name: "INT" })],
      ["Bytes offset", retyped("excerptStart", { kind: "primitive", name: "BYTES" })],
      ["Text flag", retyped("unterminated", TEXT)],
      ["meta reason", retyped("reason", { kind: "meta", name: "RejectionReason" })],
      ["record kind", retyped("kind", { kind: "record", name: "Kind", fields: [] })],
      ["lowercase primitive name", retyped("excerptTruncated", { kind: "primitive", name: "bool" })],
    ];
    for (const [name, rows] of refused) expect(decodeDatasetRead(coveragePage(rows), reference(), 10, "coverage"), name).toBeUndefined();
  });

  it("accepts only canonical, bounded standard base64 excerpts", () => {
    /** One skipped record of `content` bytes from 0 with a one-byte delimiter, its excerpt replaced. */
    const lone = (content: bigint, excerpt: string) => {
      const row = skipped("0", "0", 0n, content, 1n);
      const coverage = { records: "1", inputBytes: (content + 1n).toString(), lastOrdinal: "0", through: (content + 1n).toString(), excerptBytes: "4", segmentBytes: "512" };
      return decodeDatasetRead(coveragePage([{ ...row, value: { ...row.value, data: { ...row.value.data, excerpt } } }], {}, { coverage }), reference(), 10, "coverage");
    };
    const accepted: [bigint, string][] = [[1n, "AA=="], [1n, "/w=="], [1n, "QA=="], [2n, "AAA="], [2n, "//8="], [2n, "AAQ="], [3n, "////"]];
    for (const [content, excerpt] of accepted) expect(lone(content, excerpt)?.page?.rows[0]?.rejection?.excerpt, excerpt).toBe(excerpt);
    const refused: [string, bigint, string][] = [
      ["nonzero pad bits before ==", 1n, "AB=="], ["low pad bit before ==", 1n, "/x=="], ["high pad bit before ==", 1n, "AI=="],
      ["nonzero pad bits before =", 2n, "AAB="], ["pad bit before =", 2n, "//9="], ["second pad bit before =", 2n, "AAC="],
      ["url-safe alphabet", 3n, "__-_"], ["padding mid-text", 2n, "A=A="], ["triple padding", 1n, "A==="],
      ["longer than any bounded excerpt", 3n, "A".repeat(5468)],
    ];
    for (const [name, content, excerpt] of refused) expect(lone(content, excerpt), name).toBeUndefined();
  });
});

describe("stream queries", () => {
  afterEach(() => { vi.unstubAllGlobals(); });

  it("names only a non-default stream and never inspects a skipped-record head", () => {
    expect(datasetQuery("", { from: "0" }, 10)).toBe("from=0&limit=10");
    expect(datasetQuery("/data", { from: "0" }, 10, undefined, "coverage")).toBe("select=%2Fdata&stream=coverage&from=0&limit=10");
    expect(datasetQuery("", { cursor: "Y3Vyc29y" }, 10, undefined, "coverage")).toBe("stream=coverage&cursor=Y3Vyc29y&limit=10");
    expect(datasetQuery("", undefined, undefined, undefined, "coverage")).toBe("stream=coverage&inspect=true");
    expect(() => datasetQuery("", undefined, undefined, { kind: "head" }, "coverage")).toThrow();
    expect(() => datasetQuery("", { from: "0" }, 10, undefined, "rejections" as never)).toThrow();
  });

  it("reads skipped records over the same route and refuses a reply of the other stream", async () => {
    const engine = Object.assign(new Engine(), { generation: "g1" }) as Engine;
    const fetch = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify(coveragePage()), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(inspection({ page: { first: "0", next: "0", rows: [], extentExhausted: true, limitedBy: null, cursor: null } })), { status: 200 }));
    vi.stubGlobal("fetch", fetch);
    const read = await engine.readDataset("stored 1", "g1", reference(), "/data", { from: "0" }, 10, new AbortController().signal, "coverage");
    expect(read.page!.rows).toHaveLength(3);
    expect(fetch.mock.calls[0]![0]).toBe("/datasets/stored%201?select=%2Fdata&stream=coverage&from=0&limit=10");
    expect(fetch.mock.calls[0]![1].headers["X-Wes-Session"]).toBe("g1");
    await expect(engine.readDataset("stored 1", "g1", reference(), "/data", { from: "0" }, 10, new AbortController().signal, "coverage"))
      .rejects.toMatchObject({ kind: "invalid", code: "DATASET_REPLY_INVALID" });
  });
});
