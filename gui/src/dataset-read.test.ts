import { afterEach, describe, expect, it, vi } from "vitest";
import { ExactNumber } from "./exact-json";
import {
  DatasetReadError, datasetFailure, datasetQuery, decodeDatasetRead, lastOrdinal, previousStart, recordsAfter, withinSnapshot,
} from "./dataset-read";
import { Engine } from "./engine";
import { decodeDatasetReference, type DatasetReference } from "./presentation/dataset";

/* Synthetic snapshots only: invented identifiers and digests that name no real store. */

const BEYOND_SAFE = "9007199254740993"; // 2^53 + 1: a float would read it as ...992.
const reference = (records = BEYOND_SAFE): DatasetReference => decodeDatasetReference({
  store: "00000000-0000-4000-8000-0000000000a1", dataset: "00000000-0000-4000-8000-0000000000b2",
  generation: "3", manifest: "00000000-0000-4000-8000-0000000000c3",
  manifestDigest: `sha256:${"0123456789abcdef".repeat(4)}`, manifestBytes: "512",
  schemaDigest: `sha256:${"fedcba9876543210".repeat(4)}`, records, authorizationGeneration: "1",
})!;
const TEXT = { kind: "primitive", name: "TEXT" };
const row = (ordinal: string, label = `row ${ordinal}`) => ({
  ordinal, sourceStart: ordinal, sourceEnd: (BigInt(ordinal) + 1n).toString(),
  value: { type: { kind: "record", name: "SyntheticRow", fields: [{ name: "label", type: TEXT }] }, provenance: {}, data: { label } },
});
/** `page: null` builds an inspection reply with no page at all; an object overrides page fields. */
const reply = (over: Record<string, unknown> = {}, page: Record<string, unknown> | null = {}) => ({
  reference: { ...reference() }, lifecycle: "sealed", protected: false, persistence: "Durable", segmentBytes: "18446744073709551615",
  ...(page === null ? {} : { page: { first: "9007199254740990", next: "9007199254740992", rows: [row("9007199254740990"), row("9007199254740991")], extentExhausted: false, limitedBy: null, cursor: "c2Vjb25k", ...page } }),
  ...over,
});

describe("dataset page decoding", () => {
  it("keeps every ordinal, span and byte size as its exact decimal string", () => {
    const read = decodeDatasetRead(reply(), reference(), 2)!;
    expect(read.segmentBytes).toBe("18446744073709551615");
    expect(read.page!.rows.map(it => [it.ordinal, it.sourceEnd])).toEqual([["9007199254740990", "9007199254740991"], ["9007199254740991", "9007199254740992"]]);
    expect(recordsAfter(read.page!.next, BEYOND_SAFE)).toBe("1");
    expect(previousStart(BEYOND_SAFE, 50)).toBe("9007199254740943");
    expect(previousStart("20", 50)).toBe("0");
    expect(lastOrdinal(BEYOND_SAFE)).toBe("9007199254740992");
    expect(withinSnapshot("9007199254740992", BEYOND_SAFE)).toBe(true);
    expect(withinSnapshot(BEYOND_SAFE, BEYOND_SAFE)).toBe(false);
    for (const alias of ["01", "+1", "1e3", "1.0", " 1", "-0"]) expect(withinSnapshot(alias, BEYOND_SAFE)).toBe(false);
  });

  it("keeps each row's own type and exact payload numbers", () => {
    const exact = new ExactNumber("9007199254740993");
    const raw = reply({}, { rows: [{ ...row("9007199254740990"), value: { type: { kind: "primitive", name: "INT" }, provenance: {}, data: exact } }], next: "9007199254740991" });
    const read = decodeDatasetRead(raw, reference(), 2)!;
    expect(read.page!.rows[0]!.value.data).toBe(exact);
    expect(read.page!.rows[0]!.value.type).toEqual({ kind: "primitive", name: "INT" });
  });

  it("refuses replies that are not one coherent page of the shown snapshot", () => {
    const refused: [string, unknown, number | undefined, DatasetReference?][] = [
      ["numeric ordinal", reply({}, { first: 9007199254740990 }), 2],
      ["gap between rows", reply({}, { rows: [row("9007199254740990"), row("9007199254740992")] }), 2],
      ["next disagrees with rows", reply({}, { next: "9007199254740993" }), 2],
      ["more rows than asked for", reply(), 1],
      ["beyond the committed count", reply({ reference: reference("9007199254740991") }), 2, reference("9007199254740991")],
      ["cursor on an exhausted extent", reply({}, { extentExhausted: true }), 2],
      ["no cursor while more remain", reply({}, { cursor: null }), 2],
      ["different snapshot", reply({ reference: { ...reference(), generation: "4" } }), 2],
      ["unknown lifecycle", reply({ lifecycle: "finished" }), 2],
      ["surplus field", reply({ extra: true }), 2],
      ["span ends before it starts", reply({}, { rows: [{ ...row("9007199254740990"), sourceStart: "9", sourceEnd: "8" }, row("9007199254740991")] }), 2],
      ["row value without a type", reply({}, { rows: [{ ...row("9007199254740990"), value: { provenance: {}, data: 1 } }, row("9007199254740991")] }), 2],
      ["page on an inspection", reply(), undefined],
      ["missing page on a page read", reply({}, null), 2],
    ];
    for (const [name, raw, limit, expected] of refused) expect(decodeDatasetRead(raw, expected ?? reference(), limit), name).toBeUndefined();
    expect(decodeDatasetRead(reply({}, null), reference(), undefined)?.lifecycle).toBe("sealed");
  });

  it("accepts the read-only prefix lifecycle beside the existing ones", () => {
    expect(decodeDatasetRead(reply({ lifecycle: "prefix" }, null), reference(), undefined)?.lifecycle).toBe("prefix");
    expect(decodeDatasetRead(reply({ lifecycle: "interrupted" }, null), reference(), undefined)?.lifecycle).toBe("interrupted");
    for (const refused of ["Prefix", "prefixed", "earlier"]) expect(decodeDatasetRead(reply({ lifecycle: refused }, null), reference(), undefined), refused).toBeUndefined();
  });

  it("builds only the queries the server accepts", () => {
    expect(datasetQuery("/runs/0/events", { from: BEYOND_SAFE }, 50)).toBe(`select=%2Fruns%2F0%2Fevents&from=${BEYOND_SAFE}&limit=50`);
    expect(datasetQuery("", { cursor: "c2Vjb25k" }, 10)).toBe("cursor=c2Vjb25k&limit=10");
    expect(datasetQuery("/a")).toBe("select=%2Fa&inspect=true");
    expect(() => datasetQuery("a", { from: "0" }, 10)).toThrow();
    expect(() => datasetQuery("", { from: "007" }, 10)).toThrow();
    expect(() => datasetQuery("", { from: "0" }, 0)).toThrow();
    expect(() => datasetQuery("", { from: "0" }, 101)).toThrow();
    expect(() => datasetQuery("", { cursor: "not a cursor" }, 10)).toThrow();
  });

  it("separates busy, withdrawal, refusal, missing, limits and corruption", async () => {
    const failure = (status: number, code: string, retryable = false) => datasetFailure(new Response(JSON.stringify({ error: { code, message: `synthetic ${code}`, retryable } }), { status }));
    expect(await failure(503, "DATASET_READ_BUSY", true)).toMatchObject({ kind: "busy", retryable: true });
    expect(await failure(503, "DATASET_RETIREMENT_BUSY")).toMatchObject({ kind: "busy", retryable: false });
    expect(await failure(410, "DATASET_WITHDRAWN")).toMatchObject({ kind: "withdrawn", message: "Access withdrawn" });
    expect(await failure(403, "DATASET_ACCESS_REFUSED")).toMatchObject({ kind: "withdrawn", message: "Access withdrawn" });
    expect(await failure(404, "DATASET_MISSING")).toMatchObject({ kind: "missing" });
    expect(await failure(413, "DATASET_READ_LIMIT")).toMatchObject({ kind: "limit" });
    expect(await failure(500, "DATASET_READ_FAILED")).toMatchObject({ kind: "failed" });
    expect(await failure(409, "DATASET_SESSION_CHANGED")).toMatchObject({ kind: "session" });
    // Withdrawal is recognised by status even without a readable body, and says nothing about the data.
    expect(await datasetFailure(new Response("<html>", { status: 410 }))).toMatchObject({ kind: "withdrawn", message: "Access withdrawn" });
  });
});

describe("Engine.readDataset", () => {
  afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });
  const engine = (generation = "g1") => Object.assign(new Engine(), { generation }) as Engine;
  const ok = () => new Response(JSON.stringify(reply()), { status: 200 });
  const busy = (retryable: boolean) => new Response(JSON.stringify({ error: { code: "DATASET_READ_BUSY", message: "synthetic busy", retryable } }), { status: 503 });

  it("reads with the exact session header and retries only a retryable busy refusal", async () => {
    vi.useFakeTimers();
    const fetch = vi.fn().mockResolvedValueOnce(busy(true)).mockResolvedValueOnce(ok());
    vi.stubGlobal("fetch", fetch);
    const reading = engine().readDataset("handle 1", "g1", reference(), "/events", { from: "9007199254740990" }, 2, new AbortController().signal);
    await vi.runAllTimersAsync();
    const read = await reading;
    expect(read.page!.first).toBe("9007199254740990");
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(fetch.mock.calls[0]![0]).toBe("/datasets/handle%201?select=%2Fevents&from=9007199254740990&limit=2");
    expect(fetch.mock.calls[0]![1].headers["X-Wes-Session"]).toBe("g1");
    expect(fetch.mock.calls[0]![1].method).toBeUndefined();
  });

  it("does not retry a busy refusal the server did not mark retryable", async () => {
    const fetch = vi.fn().mockResolvedValue(busy(false));
    vi.stubGlobal("fetch", fetch);
    await expect(engine().readDataset("h", "g1", reference(), "", { from: "0" }, 2, new AbortController().signal)).rejects.toMatchObject({ kind: "busy", retryable: false });
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("drops a reply that arrives after the session changed", async () => {
    const reader = engine();
    let answer!: (response: Response) => void;
    vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(resolve => { answer = resolve; })));
    const reading = reader.readDataset("h", "g1", reference(), "", { from: "0" }, 2, new AbortController().signal);
    await Promise.resolve();
    Object.assign(reader, { generation: "g2" });
    answer(ok());
    await expect(reading).rejects.toBeInstanceOf(DatasetReadError);
    await expect(reading).rejects.toMatchObject({ kind: "session" });
  });

  it("refuses to read for a session other than the current one", async () => {
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);
    await expect(engine("g2").readDataset("h", "g1", reference(), "", { from: "0" }, 2, new AbortController().signal)).rejects.toMatchObject({ kind: "session" });
    expect(fetch).not.toHaveBeenCalled();
  });
});
