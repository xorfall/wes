import { afterEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "../../engine";
import { DatasetReadError, type DatasetPosition, type DatasetRead } from "../../dataset-read";
import { decodeDatasetReference, datasetSelect, type DatasetReference } from "../../presentation/dataset";
import type { StoredValue, TypeShape } from "../../protocol";
import { ValueBlock } from "./ValueBlock";

/* Synthetic snapshots only: invented identifiers and digests that name no real store or producer. */

vi.mock("../Cell", async (original) => ({ ...(await original<typeof import("../Cell")>()), useBlockReport: () => () => undefined }));
afterEach(() => { vi.restoreAllMocks(); });

const BEYOND_SAFE = "9007199254740993";
const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const ROW: TypeShape = { kind: "record", name: "SyntheticRow", fields: [{ name: "label", type: TEXT }] };
const DATASET: TypeShape = { kind: "dataset", element: ROW };
const reference = (dataset = "b2", records = BEYOND_SAFE): DatasetReference => decodeDatasetReference({
  store: "00000000-0000-4000-8000-0000000000a1", dataset: `00000000-0000-4000-8000-0000000000${dataset}`,
  generation: "3", manifest: "00000000-0000-4000-8000-0000000000c3",
  manifestDigest: `sha256:${"0123456789abcdef".repeat(4)}`, manifestBytes: "512",
  schemaDigest: `sha256:${"fedcba9876543210".repeat(4)}`, records, authorizationGeneration: "1",
})!;
const descriptor = (ref = reference()) => ({ kind: "dataset", reference: { ...ref } });
const rootValue: StoredValue = { type: DATASET, provenance: {}, data: descriptor() };

/** One page of the synthetic snapshot starting at `first`, exact beyond the safe integer range. */
function page(ref: DatasetReference, first: string, limit: number): DatasetRead {
  const start = BigInt(first), end = BigInt(ref.records) < start + BigInt(limit) ? BigInt(ref.records) : start + BigInt(limit);
  const rows = [];
  for (let at = start; at < end; at++) rows.push({ ordinal: at.toString(), sourceStart: (at * 10n).toString(), sourceEnd: (at * 10n + 9n).toString(),
    value: { type: ROW, provenance: {}, data: { label: `synthetic ${at}` } } });
  const exhausted = end === BigInt(ref.records);
  return { reference: ref, stream: "outputs", lifecycle: "open", protected: false, persistence: "Durable", segmentBytes: "4096",
    page: { first, next: end.toString(), rows, extentExhausted: exhausted, limitedBy: null, cursor: exhausted ? null : `next${end}` } };
}

type Call = { handle: string; generation: string; select: string; position: DatasetPosition; limit: number; resolve: (read: DatasetRead) => void; reject: (error: unknown) => void };
function fakeEngine() {
  const calls: Call[] = [];
  const readDataset = vi.fn((handle: string, generation: string, _ref: DatasetReference, select: string, position: DatasetPosition, limit: number) =>
    new Promise<DatasetRead>((resolve, reject) => { calls.push({ handle, generation, select, position, limit, resolve, reject }); }));
  return { engine: { readDataset } as unknown as Engine, calls, readDataset };
}
const flush = async () => { await act(async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); }); };
const textOf = (node: ReactTestInstance): string => node.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
const text = (tree: ReactTestRenderer) => textOf(tree.root);
const button = (tree: ReactTestRenderer, label: string) => tree.root.findAll(node => node.type === "button" && textOf(node) === label)[0]!;

function mount(value: StoredValue, engine: Engine, generation = "g1", handle = "stored-1", mode: "preview" | "window" = "window") {
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<ValueBlock engine={engine} value={value} stored={{ handle, generation }} cacheKey={`${generation}:${handle}`} mode={mode} inCell={false} />); });
  return tree;
}

it("pages one snapshot by cursor and exact ordinals beyond the safe integer range", async () => {
  const { engine, calls } = fakeEngine();
  const tree = mount(rootValue, engine);
  expect(calls).toHaveLength(1);
  expect(calls[0]).toMatchObject({ handle: "stored-1", generation: "g1", select: "", position: { from: "0" }, limit: 100 });
  calls[0]!.resolve(page(reference(), "0", 100)); await flush();
  expect(text(tree)).toContain("records 0–99 of 9 007 199 254 740 993");
  expect(text(tree)).toContain("9 007 199 254 740 893 after");
  expect(text(tree)).toContain("synthetic 42");
  expect(text(tree)).toContain("420–429");

  act(() => button(tree, "next ›").props.onClick());
  expect(calls[1]!.position).toEqual({ cursor: "next100" });

  // Go to the last record: the ordinal stays a string and Previous is computed exactly.
  const input = tree.root.findByProps({ "aria-label": "First ordinal to read" });
  act(() => input.props.onChange({ target: { value: "9007199254740992" } }));
  act(() => tree.root.findByType("form").props.onSubmit({ preventDefault() {} }));
  expect(calls[2]!.position).toEqual({ from: "9007199254740992" });
  calls[1]!.resolve(page(reference(), "100", 100)); // superseded: must not be drawn
  calls[2]!.resolve(page(reference(), "9007199254740992", 100)); await flush();
  expect(text(tree)).toContain("records 9 007 199 254 740 992–9 007 199 254 740 992");
  expect(text(tree)).not.toContain("synthetic 150");
  // The end of the committed snapshot of an open dataset is not the end of its producer.
  expect(text(tree)).toContain("end of committed snapshot · dataset still open");
  act(() => button(tree, "‹ previous").props.onClick());
  expect(calls[3]!.position).toEqual({ from: "9007199254740892" });

  act(() => input.props.onChange({ target: { value: BEYOND_SAFE } }));
  act(() => tree.root.findByType("form").props.onSubmit({ preventDefault() {} }));
  expect(calls).toHaveLength(4);
  expect(text(tree)).toContain("enter an ordinal from 0 to 9 007 199 254 740 992");
  act(() => tree.unmount());
});

it("reads a nested Dataset by its own pointer only after it is opened by hand", async () => {
  const { engine, calls } = fakeEngine();
  const holder: TypeShape = { kind: "record", name: "Capture", fields: [{ name: "runs", type: { kind: "list", element: { kind: "record", name: "Run", fields: [{ name: "events", type: DATASET }] } } }, { name: "label", type: TEXT }] };
  const value: StoredValue = { type: holder, provenance: {}, data: { runs: [{ events: descriptor(reference("d1", "2")) }, { events: descriptor(reference("d2", "5")) }], label: "synthetic" } };
  expect(datasetSelect(holder, "/runs/1/events")).toBe("/runs/1/events");
  const tree = mount(value, engine);
  expect(calls).toHaveLength(0);
  act(() => tree.root.findByProps({ "aria-label": "Open /runs" }).props.onClick());
  act(() => tree.root.findByProps({ "aria-label": "Open /runs/1" }).props.onClick());
  expect(calls).toHaveLength(0);
  act(() => tree.root.findByProps({ "aria-label": "Read records at /runs/1/events" }).props.onClick());
  expect(calls).toHaveLength(1);
  expect(calls[0]!.select).toBe("/runs/1/events");
  calls[0]!.resolve(page(reference("d2", "5"), "0", 100)); await flush();
  expect(text(tree)).toContain("records 0–4 of 5");
  // Closing the records drops the page; reopening reads again instead of reusing old rows.
  act(() => tree.root.findByProps({ "aria-label": "Close records at /runs/1/events" }).props.onClick());
  expect(text(tree)).not.toContain("synthetic 4");
  act(() => tree.root.findByProps({ "aria-label": "Read records at /runs/1/events" }).props.onClick());
  expect(calls).toHaveLength(2);
  act(() => tree.unmount());
});

it("does not read a Dataset behind an Option and says why", () => {
  const { engine, calls } = fakeEngine();
  const holder: TypeShape = { kind: "record", name: "Holder", fields: [{ name: "maybe", type: { kind: "option", element: DATASET } }] };
  expect(datasetSelect(holder, "/maybe")).toBeUndefined();
  expect(datasetSelect({ kind: "list", element: DATASET }, "/01")).toBeUndefined();
  const tree = mount({ type: { kind: "option", element: DATASET }, provenance: {}, data: { kind: "some", value: descriptor() } }, engine);
  expect(calls).toHaveLength(0);
  expect(text(tree)).toContain("records behind an optional value are not read here");
  act(() => tree.unmount());
});

it("keeps a labelled previous page while readers are busy, and clears everything on withdrawal", async () => {
  const { engine, calls } = fakeEngine();
  const tree = mount(rootValue, engine, "g-busy", "stored-busy");
  calls[0]!.resolve(page(reference(), "0", 100)); await flush();
  act(() => button(tree, "next ›").props.onClick());
  calls[1]!.reject(new DatasetReadError("busy", 503, "DATASET_READ_BUSY", "synthetic busy", true)); await flush();
  expect(text(tree)).toContain("readers busy · previous page shown");
  expect(text(tree)).toContain("synthetic 3");
  act(() => button(tree, "read again").props.onClick());
  calls[2]!.reject(new DatasetReadError("withdrawn", 410, "DATASET_WITHDRAWN", "Access withdrawn", false)); await flush();
  const shown = text(tree);
  expect(shown).toContain("Result · Access withdrawn");
  for (const gone of ["synthetic 3", "records", "9 007 199", "generation", "sha256", "SyntheticRow"]) expect(shown).not.toContain(gone);
  act(() => tree.unmount());
});

it("treats refused access like withdrawal and a missing result as distinct from corruption", async () => {
  const { engine, calls } = fakeEngine();
  const refused = mount(rootValue, engine, "g-refused", "stored-refused");
  calls[0]!.reject(new DatasetReadError("withdrawn", 403, "DATASET_ACCESS_REFUSED", "Access withdrawn", false)); await flush();
  expect(text(refused)).toContain("Access withdrawn");
  expect(text(refused)).not.toContain("records");
  act(() => refused.unmount());
  const missing = mount(rootValue, engine, "g-missing", "stored-missing");
  calls[1]!.reject(new DatasetReadError("missing", 404, "DATASET_MISSING", "synthetic missing", false)); await flush();
  expect(text(missing)).toContain("stored result unavailable");
  act(() => missing.unmount());
  const broken = mount(rootValue, engine, "g-broken", "stored-broken");
  calls[2]!.reject(new DatasetReadError("failed", 500, "DATASET_READ_FAILED", "synthetic failure", false)); await flush();
  expect(text(broken)).toContain("DATASET_READ_FAILED · synthetic failure");
  act(() => broken.unmount());
});

it("drops a late reply from a previous session and reads again for the new one", async () => {
  const { engine, calls } = fakeEngine();
  const tree = mount(rootValue, engine, "g-old", "stored-late");
  act(() => tree.update(<ValueBlock engine={engine} value={rootValue} stored={{ handle: "stored-late", generation: "g-new" }} cacheKey="g-new:stored-late" mode="window" inCell={false} />));
  expect(calls.map(call => call.generation)).toEqual(["g-old", "g-new"]);
  calls[0]!.resolve(page(reference(), "0", 100)); await flush();
  expect(text(tree)).not.toContain("synthetic 0");
  calls[1]!.resolve(page(reference(), "0", 100)); await flush();
  expect(text(tree)).toContain("synthetic 0");
  act(() => tree.unmount());
});

it("never reads a collapsed block or a value that is not a stored result", () => {
  const { engine, calls } = fakeEngine();
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<ValueBlock engine={engine} value={rootValue} stored={{ handle: "h", generation: "g" }} cacheKey="g:h" mode="preview" collapsed />); });
  act(() => tree.update(<ValueBlock engine={engine} value={rootValue} cacheKey="sample" mode="window" inCell={false} />));
  expect(calls).toHaveLength(0);
  expect(text(tree)).toContain("records unavailable · they are read from a stored result");
  act(() => tree.unmount());
});

it("lists a List of Datasets by summary and reads one only by its own index pointer when opened", async () => {
  const { engine, calls } = fakeEngine();
  const value: StoredValue = { type: { kind: "list", element: DATASET }, provenance: {}, data: [descriptor(reference("e1", "2")), descriptor(reference("e2", "3"))] };
  const tree = mount(value, engine);
  expect(calls).toHaveLength(0);
  expect(text(tree)).toContain("Dataset<SyntheticRow> · 2 records");
  expect(text(tree)).toContain("Dataset<SyntheticRow> · 3 records");
  expect(text(tree)).not.toContain("synthetic 0");
  // The second element's own disclosure; the list is never turned into rows of both Datasets.
  const closed = () => tree.root.findAll(node => node.type === "button" && node.props["aria-expanded"] === false && String(node.props.className).includes("value-toggle"));
  expect(closed()).toHaveLength(2);
  act(() => closed()[1]!.props.onClick());
  expect(calls).toHaveLength(1);
  expect(calls[0]).toMatchObject({ select: "/1", position: { from: "0" } });
  calls[0]!.resolve(page(reference("e2", "3"), "0", 100)); await flush();
  expect(text(tree)).toContain("records 0–2 of 3");
  // The first Dataset stays closed and unread: its records are not mixed into the second's.
  expect(closed()).toHaveLength(1);
  expect(calls).toHaveLength(1);
  act(() => tree.unmount());
});

it("never reads a Dataset inside a page row through the enclosing result's handle", async () => {
  const { engine, calls } = fakeEngine();
  const inner: TypeShape = { kind: "record", name: "Holder", fields: [{ name: "nested", type: DATASET }] };
  const outer: TypeShape = { kind: "dataset", element: inner };
  const tree = mount({ type: outer, provenance: {}, data: descriptor() }, engine, "g-row", "stored-row");
  calls[0]!.resolve({ ...page(reference(), "0", 1), page: { ...page(reference(), "0", 1).page!, rows: [{ ordinal: "0", sourceStart: "0", sourceEnd: "9",
    value: { type: inner, provenance: {}, data: { nested: descriptor(reference("f1", "4")) } } }] } }); await flush();
  // Focus the record: it opens whole beneath the grid, as data that is not a stored result.
  act(() => tree.root.findAll(node => node.props.role === "row" && node.props.tabIndex === 0)[0]!.props.onClick());
  const read = tree.root.findAll(node => node.type === "button" && node.props["aria-label"] === "Read records at /nested");
  expect(read).toHaveLength(1);
  act(() => read[0]!.props.onClick());
  expect(calls).toHaveLength(1);
  expect(text(tree)).toContain("records unavailable · they are read from a stored result");
  act(() => tree.unmount());
});
