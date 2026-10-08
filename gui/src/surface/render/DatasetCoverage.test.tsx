import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "../../engine";
import { DatasetReadError, decodeDatasetRead, type DatasetCoverage, type DatasetPosition, type DatasetRead, type DatasetStream } from "../../dataset-read";
import { decodeDatasetReference, type DatasetReference } from "../../presentation/dataset";
import type { StoredValue, TypeShape } from "../../protocol";
import { ValueBlock } from "./ValueBlock";
import { coverageLine } from "./DatasetBrowser";
import { lineText } from "../MonoLine";

/*
 * Synthetic analysis trees only: invented identities, digests, offsets and excerpt bytes that name no
 * real store, source or analysis.
 */

vi.mock("../Cell", async (original) => ({ ...(await original<typeof import("../Cell")>()), useBlockReport: () => () => undefined }));
afterEach(() => { vi.restoreAllMocks(); });

const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const ROW: TypeShape = { kind: "record", name: "SyntheticOutput", fields: [{ name: "label", type: TEXT }] };
const FIELDS = ["kind", "reason", "recordOrdinal", "sourceStart", "sourceEnd", "delimiterStart", "delimiterEnd", "unterminated",
  "reasonStart", "reasonEnd", "excerpt", "excerptStart", "excerptTruncated"];
const REJECTION: TypeShape = { kind: "record", name: "ScanRejection", fields: FIELDS.map(name => ({ name,
  type: name === "unterminated" || name === "excerptTruncated" ? { kind: "primitive", name: "BOOL" } : name === "excerpt" ? { kind: "primitive", name: "BYTES" } : TEXT })) };
const at = (generation: string, records: string): DatasetReference => decodeDatasetReference({
  store: "00000000-0000-4000-8000-0000000000a1", dataset: "00000000-0000-4000-8000-0000000000b2",
  generation, manifest: `00000000-0000-4000-8000-0000000000${generation.padStart(2, "0")}`, manifestDigest: `sha256:${"0123456789abcdef".repeat(4)}`, manifestBytes: "512",
  schemaDigest: `sha256:${"fedcba9876543210".repeat(4)}`, records, authorizationGeneration: "1",
})!;

function skipped(ordinal: number, record: string, start: bigint, content: bigint, delimiter: bigint, reason = "invalid_utf8") {
  const end = start + content, delimiterEnd = end + delimiter, kept = content < 4n ? content : 4n;
  return { ordinal: String(ordinal), sourceStart: start.toString(), sourceEnd: delimiterEnd.toString(), value: { type: REJECTION, provenance: {}, data: {
    kind: "rejected", reason, recordOrdinal: record, sourceStart: start.toString(), sourceEnd: end.toString(),
    delimiterStart: end.toString(), delimiterEnd: delimiterEnd.toString(), unterminated: delimiter === 0n,
    reasonStart: start.toString(), reasonEnd: (start + 1n).toString(), excerpt: btoa("ÿ".repeat(Number(kept))),
    excerptStart: start.toString(), excerptTruncated: kept < content,
  } } };
}
/** Source records 2, 5 and 9 refused by framing: 7, 4 and 5 original bytes, the last unterminated. */
const SKIPPED = [skipped(0, "2", 10n, 6n, 1n), skipped(1, "5", 40n, 3n, 1n, "raw_limit"), skipped(2, "9", 90n, 5n, 0n)];
const COVERAGE: DatasetCoverage = { records: "3", inputBytes: "16", lastOrdinal: "9", through: "95", excerptBytes: "4", segmentBytes: "512" };

/** A head inspection of `ref`, with its own coverage. */
function headOf(ref: DatasetReference, coverage: DatasetCoverage): DatasetRead {
  return { reference: ref, stream: "outputs", lifecycle: "open", protected: false, persistence: "Durable", segmentBytes: "4096", coverage };
}
/** An outputs page of `ref`, carrying `coverage` unless the tree has none (`null`). */
function outputs(ref: DatasetReference, first: string, limit: number, coverage: DatasetCoverage | null = COVERAGE): DatasetRead {
  const start = BigInt(first), total = BigInt(ref.records), end = total < start + BigInt(limit) ? total : start + BigInt(limit);
  const rows = [];
  for (let n = start; n < end; n++) rows.push({ ordinal: n.toString(), sourceStart: n.toString(), sourceEnd: n.toString(), value: { type: ROW, provenance: {}, data: { label: `output ${n}` } } });
  const exhausted = end === total;
  return { reference: ref, stream: "outputs", lifecycle: "open", protected: false, persistence: "Durable", segmentBytes: "4096", ...(coverage ? { coverage } : {}),
    page: { first, next: end.toString(), rows, extentExhausted: exhausted, limitedBy: null, cursor: exhausted ? null : `c${end}` } };
}
/** Skipped records `first` up to `count` of them, decoded by the real reader from a synthetic reply. */
function skippedPage(ref: DatasetReference, first: number, count: number, limit: number): DatasetRead {
  const rows = SKIPPED.slice(first, first + count), next = first + rows.length, exhausted = next === SKIPPED.length;
  const raw = { reference: { ...ref }, stream: "coverage", lifecycle: "open", protected: false, persistence: "Durable", segmentBytes: "4096", coverage: COVERAGE,
    page: { first: String(first), next: String(next), rows, extentExhausted: exhausted, limitedBy: exhausted ? null : "bytes", cursor: exhausted ? null : `s${next}` } };
  return decodeDatasetRead(raw, ref, limit, "coverage")!;
}

type Read = { readonly ref: DatasetReference; readonly position: DatasetPosition; readonly limit: number; readonly stream: DatasetStream };
type Pending<T> = { readonly args: T; resolve: (read: DatasetRead) => void; reject: (error: unknown) => void; readonly signal: AbortSignal };
function fakeEngine() {
  const reads: Pending<Read>[] = [];
  const heads: Pending<null>[] = [];
  const extents: Pending<{ extent: DatasetReference; position: DatasetPosition; limit: number }>[] = [];
  const promise = <T,>(list: Pending<T>[], args: T, signal: AbortSignal) => new Promise<DatasetRead>((resolve, reject) => list.push({ args, resolve, reject, signal }));
  const engine = {
    readDataset: vi.fn((_h: string, _g: string, ref: DatasetReference, _s: string, position: DatasetPosition, limit: number, signal: AbortSignal, stream: DatasetStream = "outputs") =>
      promise(reads, { ref, position, limit, stream }, signal)),
    readDatasetHead: vi.fn((_h: string, _g: string, _o: DatasetReference, _shown: DatasetReference, _s: string, signal: AbortSignal) => promise(heads, null, signal)),
    readDatasetExtent: vi.fn((_h: string, _g: string, _o: DatasetReference, extent: DatasetReference, _s: string, position: DatasetPosition, limit: number, signal: AbortSignal) =>
      promise(extents, { extent, position, limit }, signal)),
  } as unknown as Engine;
  return { engine, reads, heads, extents };
}
const flush = async () => { await act(async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); }); };
const textOf = (node: ReactTestInstance): string => node.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
const text = (tree: ReactTestRenderer) => textOf(tree.root);
const button = (tree: ReactTestRenderer, label: string) => tree.root.findAll(node => node.type === "button" && textOf(node) === label)[0];
const press = (tree: ReactTestRenderer, label: string) => act(() => button(tree, label)!.props.onClick());
const grid = (tree: ReactTestRenderer) => tree.root.findByProps({ role: "grid" });
const goTo = (tree: ReactTestRenderer, ordinal: string) => {
  act(() => tree.root.findByProps({ "aria-label": "First ordinal to read" }).props.onChange({ target: { value: ordinal } }));
  act(() => tree.root.findByType("form").props.onSubmit({ preventDefault() {} }));
};

let mounts = 0;
const block = (engine: Engine, value: StoredValue, handle: string, mode: "preview" | "expanded" | "window") =>
  <ValueBlock engine={engine} value={value} stored={{ handle, generation: "g1" }} cacheKey={`g1:${handle}`} mode={mode} inCell={false} />;
/** Mounted on its own stored result and Reading the saved snapshot's first outputs page. */
async function reading(original: DatasetReference, coverage: DatasetCoverage | null = COVERAGE, mode: "preview" | "expanded" | "window" = "window") {
  const fake = fakeEngine();
  const value: StoredValue = { type: { kind: "dataset", element: ROW }, provenance: {}, data: { kind: "dataset", reference: { ...original } } };
  const handle = `stored-coverage-${++mounts}`;
  let tree!: ReactTestRenderer;
  act(() => { tree = create(block(fake.engine, value, handle, mode), { createNodeMock: () => ({ scrollTop: 0, scrollHeight: 0, getBoundingClientRect: () => ({ height: 480 }) }) }); });
  fake.reads[0]!.resolve(outputs(original, "0", fake.reads[0]!.args.limit, coverage)); await flush();
  return { ...fake, tree, update: (next: "preview" | "expanded" | "window") => act(() => tree.update(block(fake.engine, value, handle, next))) };
}

describe("offering skipped records", () => {
  it("offers nothing without forensic coverage, and says the count when there is one", async () => {
    const plain = await reading(at("3", "20"), null);
    expect(button(plain.tree, "skipped records")).toBeUndefined();
    expect(text(plain.tree)).not.toContain("skipped");
    act(() => plain.tree.unmount());

    const forensic = await reading(at("3", "20"));
    expect(text(forensic.tree)).toContain("3 records skipped · 16 original bytes · forensic framing of the saved snapshot · excerpts keep at most 4 bytes");
    expect(button(forensic.tree, "outputs")!.props["aria-pressed"]).toBe(true);
    expect(button(forensic.tree, "skipped records")!.props["aria-pressed"]).toBe(false);
    // Nothing is derived from the counts: no interpreted or accepted record count.
    expect(text(forensic.tree)).not.toMatch(/interpreted records|accepted|17 records/);
    act(() => forensic.tree.unmount());
  });

  it("words a single and an empty skip exactly", () => {
    expect(lineText(coverageLine({ ...COVERAGE, records: "1", inputBytes: "1" }))).toContain("1 record skipped · 1 original byte");
    expect(lineText(coverageLine({ records: "0", inputBytes: "0", lastOrdinal: null, through: null, excerptBytes: "1", segmentBytes: "0" })))
      .toBe("forensic framing · no records skipped in the saved snapshot");
  });
});

describe("paging skipped records", () => {
  it("pages a tree with no outputs by the coverage's own count and ordinals", async () => {
    const original = at("3", "0");
    const { tree, reads } = await reading(original, COVERAGE, "preview");
    expect(text(tree)).toContain("no records in this snapshot");
    press(tree, "skipped records");
    expect(reads[1]!.args).toEqual({ ref: original, position: { from: "0" }, limit: 10, stream: "coverage" });
    reads[1]!.resolve(skippedPage(original, 0, 2, 10)); await flush();
    expect(grid(tree).props["aria-label"]).toBe("Skipped records");
    expect(text(tree)).toContain("skipped records 0–1 of 3 · 1 after · saved snapshot, generation 3");
    expect(text(tree)).toContain("page limited by bytes");
    // Coverage ordinal, then the source record's own ordinal, the framing reason and what the excerpt keeps.
    expect(text(tree)).toContain("excerpt 4 of 6 B · truncated");
    expect(text(tree)).toContain("raw limit");
    expect(text(tree)).toContain("# is the skipped-record ordinal · record is the source record's ordinal");
    // Never followed, never captured; the descriptor's empty snapshot is still what Keep or Pin names.
    expect(button(tree, "follow")!.props.disabled).toBe(true);
    expect(text(tree)).toContain("skipped records of the result's saved snapshot · follow reads outputs only");
    expect(button(tree, "capture shown prefix…")).toBeUndefined();
    expect(text(tree)).toContain("Keep or Pin retains exactly this empty snapshot");

    press(tree, "next ›");
    expect(reads[2]!.args).toMatchObject({ position: { cursor: "s2" }, stream: "coverage" });
    reads[2]!.resolve(skippedPage(original, 2, 1, 10)); await flush();
    expect(text(tree)).toContain("skipped records 2–2 of 3 · 2 before");
    expect(text(tree)).toContain("excerpt 4 of 5 B · truncated · unterminated");
    expect(text(tree)).toContain("end of skipped records in the saved snapshot · dataset still open");

    goTo(tree, "3");
    expect(text(tree)).toContain("enter a skipped-record ordinal from 0 to 2");
    expect(reads).toHaveLength(3);
    goTo(tree, "1");
    expect(reads[3]!.args).toMatchObject({ position: { from: "1" }, stream: "coverage" });
    reads[3]!.resolve(skippedPage(original, 1, 2, 10)); await flush();
    // Opening a row shows the whole typed record with both ordinals apart.
    act(() => tree.root.findAll(node => node.props.role === "row" && node.props.tabIndex === 0)[0]!.props.onClick());
    expect(text(tree)).toContain("skipped record 1 · source record 5 · source 40–44 · refused by native framing: raw limit · not interpreted · excerpt 3 of 3 B");
    expect(tree.root.findByProps({ "aria-label": "Skipped record 1" })).toBeDefined();
    act(() => tree.unmount());
  });

  it("drops the other stream's rows and late replies, and keeps only same-stream rows while busy", async () => {
    const original = at("3", "20");
    const { tree, reads } = await reading(original);
    goTo(tree, "5");
    const lateOutputs = reads[1]!;
    press(tree, "skipped records");
    const firstSkipped = reads[2]!;
    expect(lateOutputs.signal.aborted).toBe(true);
    expect(firstSkipped.args.stream).toBe("coverage");
    // Nothing of the outputs stays drawn, not even as a labelled previous page.
    expect(text(tree)).not.toContain("output 0");
    expect(text(tree)).toContain("reading…");
    lateOutputs.resolve(outputs(original, "5", 100)); await flush();
    expect(text(tree)).not.toContain("output 5");
    firstSkipped.reject(new DatasetReadError("busy", 503, "DATASET_READ_BUSY", "synthetic busy", true)); await flush();
    expect(text(tree)).toContain("readers busy · synthetic busy");
    expect(text(tree)).not.toContain("previous page shown");
    expect(text(tree)).not.toContain("output ");

    press(tree, "read again");
    reads[3]!.resolve(skippedPage(original, 0, 3, 100)); await flush();
    expect(text(tree)).toContain("skipped records 0–2 of 3");
    // Busy now keeps the skipped rows: they are the stream shown.
    goTo(tree, "1");
    reads[4]!.reject(new DatasetReadError("busy", 503, "DATASET_READ_BUSY", "synthetic busy", true)); await flush();
    expect(text(tree)).toContain("readers busy · previous page shown");
    expect(text(tree)).toContain("invalid UTF-8");

    // Back to outputs at their own position; a late skipped reply is never drawn there.
    goTo(tree, "2");
    const lateSkipped = reads[5]!;
    press(tree, "outputs");
    expect(lateSkipped.signal.aborted).toBe(true);
    expect(reads[6]!.args).toMatchObject({ position: { from: "5" }, stream: "outputs" });
    expect(text(tree)).not.toContain("invalid UTF-8");
    lateSkipped.resolve(skippedPage(original, 2, 1, 100)); await flush();
    expect(text(tree)).not.toContain("invalid UTF-8");
    reads[6]!.resolve(outputs(original, "5", 100)); await flush();
    expect(text(tree)).toContain("records 5–19 of 20");
    expect(grid(tree).props["aria-label"]).toBe("Dataset records");
    act(() => tree.unmount());
  });

  it("keeps the stream and reads it at the new tier's page size after a size change", async () => {
    const original = at("3", "20");
    const { tree, reads, update } = await reading(original, COVERAGE, "expanded");
    press(tree, "skipped records");
    reads[1]!.resolve(skippedPage(original, 0, 3, 50)); await flush();
    update("preview");
    const last = reads.at(-1)!;
    expect(last.args).toMatchObject({ stream: "coverage", limit: 10, position: { from: "0" } });
    last.resolve(skippedPage(original, 0, 3, 10)); await flush();
    expect(button(tree, "skipped records")!.props["aria-pressed"]).toBe(true);
    expect(text(tree)).toContain("skipped records 0–2 of 3");
    act(() => tree.unmount());
  });
});

describe("geometry", () => {
  it("keeps the same controls and row area in both streams at the smallest tier", async () => {
    const original = at("3", "20");
    const { tree, reads } = await reading(original, COVERAGE, "preview");
    const controls = () => tree.root.findByProps({ role: "toolbar" }).findAll(node => node.type === "button" || node.type === "input").length;
    const before = { controls: controls(), rows: grid(tree).props.style["--dataset-rows"] };
    expect(before.rows).toBe(5);
    press(tree, "skipped records");
    expect({ controls: controls(), rows: grid(tree).props.style["--dataset-rows"] }).toEqual(before);
    reads[1]!.resolve(skippedPage(original, 0, 3, 10)); await flush();
    expect({ controls: controls(), rows: grid(tree).props.style["--dataset-rows"] }).toEqual(before);
    // The count and the stream choice sit beneath the rows, in document order after the grid.
    const order = tree.root.findAll(node => node.props.role === "grid" || node.props["aria-label"] === "Skipped records of the saved snapshot").map(node => node.props.role ?? "coverage");
    expect(order).toEqual(["grid", "coverage"]);
    act(() => tree.unmount());
  });
});

describe("follow and withdrawal", () => {
  it("reads skipped records from the saved snapshot after Follow drew a newer prefix", async () => {
    const original = at("3", "20");
    const { tree, reads, heads, extents } = await reading(original);
    press(tree, "follow");
    const grown = at("4", "130");
    // The newer prefix has more skipped records of its own; they are not the saved snapshot's.
    const later: DatasetCoverage = { ...COVERAGE, records: "5", inputBytes: "30", lastOrdinal: "120", through: "900" };
    heads[0]!.resolve(headOf(grown, later)); await flush();
    const tail = extents[0]!;
    tail.resolve(outputs(grown, (tail.args.position as { from: string }).from, tail.args.limit, later)); await flush();
    expect(text(tree)).toContain("showing newer committed records: generation 4, 130 records");
    expect(text(tree)).toContain("3 records skipped · 16 original bytes");
    expect(text(tree)).not.toContain("5 records skipped");

    press(tree, "skipped records");
    // Following stopped; no further head is inspected.
    expect(heads).toHaveLength(1);
    expect(heads[0]!.signal.aborted).toBe(true);
    expect(reads.at(-1)!.args).toMatchObject({ ref: original, stream: "coverage", position: { from: "0" } });
    expect(extents).toHaveLength(1);
    reads.at(-1)!.resolve(skippedPage(original, 0, 3, 100)); await flush();
    expect(text(tree)).not.toContain("showing newer committed records");
    expect(text(tree)).toContain("saved snapshot, generation 3");

    // Outputs return to the newer extent Follow drew, read again as that extent.
    press(tree, "outputs");
    expect(extents).toHaveLength(2);
    expect(extents[1]!.args.extent).toEqual(grown);
    extents[1]!.resolve(outputs(grown, (extents[1]!.args.position as { from: string }).from, 100)); await flush();
    expect(text(tree)).toContain("showing newer committed records: generation 4, 130 records");
    act(() => tree.unmount());
  });

  it("clears both streams and every skipped count when the stored result is withdrawn", async () => {
    const original = at("3", "0");
    const { tree, reads } = await reading(original);
    press(tree, "skipped records");
    reads[1]!.resolve(skippedPage(original, 0, 3, 100)); await flush();
    goTo(tree, "1");
    reads[2]!.reject(new DatasetReadError("withdrawn", 410, "DATASET_WITHDRAWN", "Access withdrawn", false)); await flush();
    const shown = text(tree);
    expect(shown).toContain("Access withdrawn");
    for (const gone of ["skipped", "16 original", "invalid UTF-8", "excerpt", "records", "generation", "ScanRejection", "outputs"]) expect(shown, gone).not.toContain(gone);
    act(() => tree.unmount());
  });

  it("keeps no skipped count for a missing result", async () => {
    const original = at("3", "0");
    const { tree, reads } = await reading(original);
    press(tree, "skipped records");
    reads[1]!.reject(new DatasetReadError("missing", 404, "DATASET_MISSING", "synthetic missing", false)); await flush();
    expect(text(tree)).toContain("stored result unavailable");
    expect(text(tree)).not.toContain("records skipped");
    act(() => tree.unmount());
  });
});
