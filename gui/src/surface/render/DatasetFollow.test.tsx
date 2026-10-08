import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "../../engine";
import {
  DatasetReadError, datasetQuery, decodeDatasetHead, extendsReference,
  type DatasetPosition, type DatasetRead, type DatasetRecording,
} from "../../dataset-read";
import { decodeDatasetReference, type DatasetReference } from "../../presentation/dataset";
import type { StoredValue, TypeShape } from "../../protocol";
import { ValueBlock } from "./ValueBlock";
import { FOLLOW_INTERVAL_MS } from "./DatasetBrowser";

/*
 * Synthetic snapshots only: invented store, dataset, manifest and run identities and digests that
 * name no real store, recording or analysis.
 */

vi.mock("../Cell", async (original) => ({ ...(await original<typeof import("../Cell")>()), useBlockReport: () => () => undefined }));
beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); });

const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const ROW: TypeShape = { kind: "record", name: "SyntheticEvent", fields: [{ name: "label", type: TEXT }] };
const DATASET: TypeShape = { kind: "dataset", element: ROW };
const at = (generation: string, records: string, manifest = `00000000-0000-4000-8000-0000000000${generation.padStart(2, "0")}`, over: Partial<Record<string, string>> = {}): DatasetReference => decodeDatasetReference({
  store: "00000000-0000-4000-8000-0000000000a1", dataset: "00000000-0000-4000-8000-0000000000b2",
  generation, manifest, manifestDigest: `sha256:${"0123456789abcdef".repeat(4)}`, manifestBytes: "512",
  schemaDigest: `sha256:${"fedcba9876543210".repeat(4)}`, records, authorizationGeneration: "1", ...over,
})!;
const ORIGINAL = at("3", "20");
const value: StoredValue = { type: DATASET, provenance: {}, data: { kind: "dataset", reference: { ...ORIGINAL } } };

const recording = (ref: DatasetReference, termination: DatasetRecording["termination"] = null): DatasetRecording => ({
  run: "00000000-0000-4000-8000-0000000000e1", epoch: ref.dataset, first: "1", acceptedThrough: ref.records,
  committedThrough: ref.records, pending: "0", rejected: "0", termination,
});
function head(ref: DatasetReference, lifecycle: DatasetRead["lifecycle"] = "open", termination: DatasetRecording["termination"] = null): DatasetRead {
  return { reference: ref, stream: "outputs", lifecycle, protected: true, persistence: "Durable", segmentBytes: "4096", recording: recording(ref, termination) };
}
function page(ref: DatasetReference, first: string, limit: number, lifecycle: DatasetRead["lifecycle"] = "open"): DatasetRead {
  const start = BigInt(first), total = BigInt(ref.records), end = total < start + BigInt(limit) ? total : start + BigInt(limit);
  const rows = [];
  for (let n = start; n < end; n++) rows.push({ ordinal: n.toString(), sourceStart: n.toString(), sourceEnd: n.toString(), value: { type: ROW, provenance: {}, data: { label: `event ${n}` } } });
  const exhausted = end === total;
  return { reference: ref, stream: "outputs", lifecycle, protected: true, persistence: "Durable", segmentBytes: "4096",
    page: { first, next: end.toString(), rows, extentExhausted: exhausted, limitedBy: null, cursor: exhausted ? null : `c${end}` } };
}

type Pending<T> = { readonly args: T; resolve: (read: DatasetRead) => void; reject: (error: unknown) => void; readonly signal: AbortSignal };
function fakeEngine() {
  const frozen: Pending<{ position: DatasetPosition; limit: number }>[] = [];
  const heads: Pending<{ original: DatasetReference; shown: DatasetReference }>[] = [];
  const extents: Pending<{ extent: DatasetReference; position: DatasetPosition; limit: number }>[] = [];
  const promise = <T,>(list: Pending<T>[], args: T, signal: AbortSignal) => new Promise<DatasetRead>((resolve, reject) => list.push({ args, resolve, reject, signal }));
  const engine = {
    readDataset: vi.fn((_h: string, _g: string, _r: DatasetReference, _s: string, position: DatasetPosition, limit: number, signal: AbortSignal) => promise(frozen, { position, limit }, signal)),
    readDatasetHead: vi.fn((_h: string, _g: string, original: DatasetReference, shown: DatasetReference, _s: string, signal: AbortSignal) => promise(heads, { original, shown }, signal)),
    readDatasetExtent: vi.fn((_h: string, _g: string, _o: DatasetReference, extent: DatasetReference, _s: string, position: DatasetPosition, limit: number, signal: AbortSignal) =>
      promise(extents, { extent, position, limit }, signal)),
  } as unknown as Engine;
  return { engine, frozen, heads, extents };
}
const flush = async () => { await act(async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); }); };
const tick = async (ms: number) => { await act(async () => { vi.advanceTimersByTime(ms); }); await flush(); };
const textOf = (node: ReactTestInstance): string => node.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
const text = (tree: ReactTestRenderer) => textOf(tree.root);
const button = (tree: ReactTestRenderer, label: string) => tree.root.findAll(node => node.type === "button" && textOf(node) === label)[0];
const press = (tree: ReactTestRenderer, label: string) => act(() => button(tree, label)!.props.onClick());
const rows = (tree: ReactTestRenderer) => tree.root.findByProps({ role: "grid" });

/** Each mount reads its own stored result, so one test's withdrawal never gates another's. */
let mounts = 0;
/** The row scroller as the browser sees it; the last mounted one, for scroll-position assertions. */
let scroller = { scrollTop: 0, scrollHeight: 1000 };
const block = (engine: Engine, handle: string, mode: "preview" | "expanded" | "window", collapsed: boolean) =>
  <ValueBlock engine={engine} value={value} stored={{ handle, generation: "g1" }} cacheKey={`g1:${handle}`} mode={mode} inCell={false} {...(collapsed ? { collapsed: true } : {})} />;
let lastHandle = "";
function mount(engine: Engine, mode: "preview" | "expanded" | "window" = "window", collapsed = false) {
  const handle = lastHandle = `stored-follow-${++mounts}`;
  scroller = { scrollTop: 0, scrollHeight: 1000 };
  let tree!: ReactTestRenderer;
  act(() => { tree = create(block(engine, handle, mode, collapsed), {
    createNodeMock: element => element.props.role === "grid" ? scroller : null,
  }); });
  return tree;
}
/** Mounted, Reading the saved snapshot's first page. */
async function reading(mode: "preview" | "expanded" | "window" = "window") {
  const fake = fakeEngine();
  const tree = mount(fake.engine, mode);
  fake.frozen[0]!.resolve(page(ORIGINAL, "0", fake.frozen[0]!.args.limit)); await flush();
  return { ...fake, tree };
}
/** Following, with the head grown to `grown` and its tail page shown. */
async function following(grown = at("4", "130")) {
  const fake = await reading();
  press(fake.tree, "follow");
  fake.heads[0]!.resolve(head(grown)); await flush();
  const tail = fake.extents[0]!;
  tail.resolve(page(grown, (tail.args.position as { readonly from: string }).from, tail.args.limit)); await flush();
  return { ...fake, grown };
}

describe("starting", () => {
  it("reads only the saved snapshot on mount, and never a head without an explicit follow", async () => {
    const { engine, frozen, heads, extents, tree } = await reading();
    expect(frozen).toHaveLength(1);
    expect(frozen[0]!.args.position).toEqual({ from: "0" });
    await tick(10 * FOLLOW_INTERVAL_MS);
    expect(heads).toHaveLength(0);
    expect(extents).toHaveLength(0);
    expect(text(tree)).toContain("reading · the result's saved snapshot");
    expect(button(tree, "follow")).toBeDefined();
    expect((engine.readDatasetHead as ReturnType<typeof vi.fn>)).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("does no head read for a collapsed block, and a remount is Reading again", async () => {
    const fake = fakeEngine();
    const collapsed = mount(fake.engine, "preview", true);
    await tick(5 * FOLLOW_INTERVAL_MS);
    expect(fake.frozen).toHaveLength(0);
    expect(fake.heads).toHaveLength(0);
    act(() => collapsed.unmount());
    // A reopened or restored block starts from its saved snapshot, Reading.
    const reopened = mount(fake.engine);
    expect(fake.frozen).toHaveLength(1);
    expect(fake.heads).toHaveLength(0);
    expect(button(reopened, "follow")).toBeDefined();
    act(() => reopened.unmount());
  });

  it("follows explicitly: one head inspection, then the tail page of that head, labelled apart from the saved input", async () => {
    const { heads, extents, tree, grown } = await following();
    expect(heads[0]!.args).toEqual({ original: ORIGINAL, shown: ORIGINAL });
    expect(extents[0]!.args).toMatchObject({ extent: grown, position: { from: "30" }, limit: 100 });
    const shown = text(tree);
    expect(shown).toContain("following · newest committed records");
    expect(shown).toContain("showing newer committed records: generation 4, 130 records");
    expect(shown).toContain("saved input is generation 3, 20 records");
    // Keep and Pin still describe the saved input only; nothing offers to pin the newer head.
    expect(shown).toContain("Keep or Pin retains exactly records 0–19 of generation 3");
    expect(shown).not.toMatch(/pin .*head|pin newest/i);
    expect(shown).toContain("event 129");
    act(() => tree.unmount());
  });
});

describe("while following", () => {
  it("polls at most once a second and never overlaps a head and a page read", async () => {
    const fake = await reading();
    press(fake.tree, "follow");
    // The first head is outstanding: no second request of any kind, however long it takes.
    await tick(5 * FOLLOW_INTERVAL_MS);
    expect(fake.heads).toHaveLength(1);
    const grown = at("4", "130");
    fake.heads[0]!.resolve(head(grown)); await flush();
    // The tail page is outstanding: still no head.
    await tick(5 * FOLLOW_INTERVAL_MS);
    expect(fake.heads).toHaveLength(1);
    expect(fake.extents).toHaveLength(1);
    fake.extents[0]!.resolve(page(grown, "30", 100)); await flush();
    await tick(FOLLOW_INTERVAL_MS - 1);
    expect(fake.heads).toHaveLength(1);
    await tick(1);
    expect(fake.heads).toHaveLength(2);
    expect(fake.heads[1]!.args.shown).toEqual(grown);
    // An unchanged head reads no page.
    fake.heads[1]!.resolve(head(grown)); await flush();
    expect(fake.extents).toHaveLength(1);
    expect(fake.frozen).toHaveLength(1);
    act(() => fake.tree.unmount());
  });

  it.each([
    ["reading", (tree: ReactTestRenderer) => press(tree, "reading")],
    ["a wheel scroll", (tree: ReactTestRenderer) => act(() => rows(tree).props.onWheel())],
    ["a scroll key", (tree: ReactTestRenderer) => act(() => rows(tree).props.onKeyDown({ key: "PageUp" }))],
    ["previous page", (tree: ReactTestRenderer) => press(tree, "‹ previous")],
    ["opening a row", (tree: ReactTestRenderer) => act(() => tree.root.findAll(node => node.props.role === "row" && node.props.tabIndex === 0)[0]!.props.onClick())],
    ["editing the ordinal", (tree: ReactTestRenderer) => act(() => tree.root.findByProps({ "aria-label": "First ordinal to read" }).props.onChange({ target: { value: "4" } }))],
  ])("returns to Reading on %s and keeps the shown head and rows while newer records arrive", async (_name, gesture) => {
    const fake = await following();
    await tick(FOLLOW_INTERVAL_MS);
    expect(fake.heads).toHaveLength(2);
    const pending = fake.heads[1]!;
    gesture(fake.tree);
    expect(pending.signal.aborted).toBe(true);
    // A late head after the gesture is not drawn and starts nothing.
    pending.resolve(head(at("5", "400"))); await flush();
    await tick(10 * FOLLOW_INTERVAL_MS);
    expect(fake.heads).toHaveLength(2);
    expect(fake.extents.length).toBeLessThanOrEqual(2);
    const shown = text(fake.tree);
    expect(shown).toContain("generation 4, 130 records");
    expect(shown).not.toContain("generation 5");
    expect(shown).not.toContain("event 399");
    expect(button(fake.tree, "follow")).toBeDefined();
    act(() => fake.tree.unmount());
  });

  it("follows again to the tail of the newest head after Reading", async () => {
    const fake = await following();
    press(fake.tree, "reading");
    press(fake.tree, "follow");
    expect(fake.heads.at(-1)!.args.shown).toEqual(fake.grown);
    const newer = at("6", "260");
    fake.heads.at(-1)!.resolve(head(newer)); await flush();
    expect(fake.extents.at(-1)!.args).toMatchObject({ extent: newer, position: { from: "160" } });
    act(() => fake.tree.unmount());
  });
});

describe("how following ends", () => {
  it("shows the final page of an ended head and returns to Reading without reconnecting", async () => {
    const fake = await following();
    await tick(FOLLOW_INTERVAL_MS);
    const final = at("5", "140");
    fake.heads[1]!.resolve(head(final, "sealed", "natural")); await flush();
    fake.extents[1]!.resolve(page(final, "40", 100, "sealed")); await flush();
    const shown = text(fake.tree);
    expect(shown).toContain("reading · final page · sealed · the source ended");
    expect(shown).toContain("event 139");
    await tick(10 * FOLLOW_INTERVAL_MS);
    expect(fake.heads).toHaveLength(2);
    expect(button(fake.tree, "follow")).toBeDefined();
    act(() => fake.tree.unmount());
  });

  it("stops on a refused head, such as a resumed analysis attempt, and keeps the labelled page", async () => {
    const fake = await following();
    await tick(FOLLOW_INTERVAL_MS);
    fake.heads[1]!.reject(new DatasetReadError("failed", 500, "DATASET_READ_FAILED", "synthetic attempt changed", false)); await flush();
    const shown = text(fake.tree);
    expect(shown).toContain("following stopped · synthetic attempt changed");
    expect(shown).toContain("previous page shown");
    expect(shown).toContain("event 129");
    await tick(10 * FOLLOW_INTERVAL_MS);
    expect(fake.heads).toHaveLength(2);
    act(() => fake.tree.unmount());
  });

  it("stops on a continuity refusal with a truthful label, keeps only the shown page and opens nothing else", async () => {
    const fake = await following();
    await tick(FOLLOW_INTERVAL_MS);
    fake.heads[1]!.reject(new DatasetReadError("continuity", 409, "DATASET_CONTINUITY_CHANGED",
      "The selected dataset no longer has the same committed identity or analysis attempt. Keep reading the saved snapshot or open a new result.", false)); await flush();
    const shown = text(fake.tree);
    expect(shown).toContain("following stopped · this dataset no longer has the same committed identity or analysis attempt");
    expect(shown).toContain("open a result explicitly to read another snapshot · previous committed page retained");
    // No promise that any snapshot will stay readable.
    expect(shown).not.toMatch(/stays readable|will remain|still readable/i);
    expect(shown).toContain("event 129");
    // No read-again, no switch to another attempt, no further reads of any kind.
    expect(button(fake.tree, "read again")).toBeUndefined();
    expect(shown).not.toMatch(/new attempt|switch to|resume|start recording/i);
    await tick(10 * FOLLOW_INTERVAL_MS);
    expect(fake.heads).toHaveLength(2);
    expect(fake.extents).toHaveLength(1);
    act(() => fake.tree.unmount());
  });

  it("keeps following through a busy reader, with the previous page labelled", async () => {
    const fake = await following();
    await tick(FOLLOW_INTERVAL_MS);
    fake.heads[1]!.reject(new DatasetReadError("busy", 503, "DATASET_READ_BUSY", "synthetic busy", true)); await flush();
    expect(text(fake.tree)).toContain("readers busy · previous page shown");
    expect(text(fake.tree)).toContain("event 129");
    await tick(FOLLOW_INTERVAL_MS);
    expect(fake.heads).toHaveLength(3);
    act(() => fake.tree.unmount());
  });

  it("clears everything on withdrawal and reads nothing more", async () => {
    const fake = await following();
    await tick(FOLLOW_INTERVAL_MS);
    fake.heads[1]!.reject(new DatasetReadError("withdrawn", 410, "DATASET_WITHDRAWN", "Access withdrawn", false)); await flush();
    const shown = text(fake.tree);
    expect(shown).toContain("Access withdrawn");
    for (const gone of ["event 129", "generation 4", "130 records", "following"]) expect(shown).not.toContain(gone);
    await tick(10 * FOLLOW_INTERVAL_MS);
    expect(fake.heads).toHaveLength(2);
    act(() => fake.tree.unmount());
  });

  it("aborts a cycle in flight on unmount and lands nothing afterwards", async () => {
    const fake = await reading();
    press(fake.tree, "follow");
    act(() => fake.tree.unmount());
    expect(fake.heads[0]!.signal.aborted).toBe(true);
    fake.heads[0]!.resolve(head(at("4", "130"))); await flush();
    await tick(5 * FOLLOW_INTERVAL_MS);
    expect(fake.extents).toHaveLength(0);
    expect(fake.heads).toHaveLength(1);
  });
});

describe("the reading position", () => {
  it("keeps the scroller where the reader left it when a late head reply arrives after pausing", async () => {
    const fake = await following();
    // Following kept the newest row in view.
    expect(scroller.scrollTop).toBe(scroller.scrollHeight);
    await tick(FOLLOW_INTERVAL_MS);
    act(() => rows(fake.tree).props.onWheel());
    scroller.scrollTop = 420; // the reader's own scroll lands
    fake.heads[1]!.resolve(head(at("5", "400"))); await flush();
    await tick(5 * FOLLOW_INTERVAL_MS);
    expect(scroller.scrollTop).toBe(420);
    expect(text(fake.tree)).toContain("event 129");
    expect(text(fake.tree)).not.toContain("event 399");
    expect(fake.extents).toHaveLength(1);
    act(() => fake.tree.unmount());
  });

  it("keeps the scroller and rows when a late tail page arrives after pausing", async () => {
    const fake = await following();
    await tick(FOLLOW_INTERVAL_MS);
    const newer = at("5", "200");
    fake.heads[1]!.resolve(head(newer)); await flush();
    const late = fake.extents[1]!;
    act(() => rows(fake.tree).props.onKeyDown({ key: "ArrowUp" }));
    expect(late.signal.aborted).toBe(true);
    scroller.scrollTop = 300;
    late.resolve(page(newer, "100", 100)); await flush();
    expect(scroller.scrollTop).toBe(300);
    expect(text(fake.tree)).toContain("generation 4, 130 records");
    expect(text(fake.tree)).not.toContain("event 199");
    act(() => fake.tree.unmount());
  });

  it("rereads after a tier change with the old rows labelled as retained, and never resumes following", async () => {
    const fake = await following();
    press(fake.tree, "reading");
    const before = fake.extents.length;
    act(() => fake.tree.update(block(fake.engine, lastHandle, "expanded", false)));
    // One intentional reread of the same shown head at the new page size; nothing else.
    expect(fake.extents).toHaveLength(before + 1);
    expect(fake.extents.at(-1)!.args).toMatchObject({ extent: fake.grown, limit: 50 });
    expect(text(fake.tree)).toContain("reading · previous page shown");
    expect(text(fake.tree)).toContain("event 129");
    await tick(5 * FOLLOW_INTERVAL_MS);
    expect(fake.heads).toHaveLength(1);
    expect(button(fake.tree, "follow")).toBeDefined();
    act(() => fake.tree.unmount());
  });
});

describe("every tier", () => {
  it.each([["preview"], ["expanded"], ["window"]] as const)("offers the same Reading/Follow reader in %s", async (mode) => {
    const fake = await reading(mode);
    expect(button(fake.tree, "follow")).toBeDefined();
    press(fake.tree, "follow");
    expect(fake.heads).toHaveLength(1);
    act(() => fake.tree.unmount());
  });
});

describe("head and extent reads", () => {
  it("asks for a head only as an inspection, and pages an extent with its exact descriptor", () => {
    expect(datasetQuery("/events", undefined, undefined, { kind: "head" })).toBe("select=%2Fevents&inspect=true&head=true");
    expect(() => datasetQuery("", { from: "0" }, 10, { kind: "head" })).toThrow();
    expect(() => datasetQuery("", undefined, undefined, { kind: "extent", reference: ORIGINAL })).toThrow();
    const query = new URLSearchParams(datasetQuery("", { cursor: "c10" }, 10, { kind: "extent", reference: ORIGINAL }));
    expect(JSON.parse(query.get("extent")!)).toEqual({ ...ORIGINAL });
    expect(query.get("cursor")).toBe("c10");
    expect(query.has("head")).toBe(false);
  });

  it("accepts a head only as a committed extension of both the saved snapshot and the shown head", () => {
    const raw = (ref: DatasetReference) => ({ reference: { ...ref }, stream: "outputs", lifecycle: "open", protected: true, persistence: "Durable", segmentBytes: "4096" });
    const shown = at("4", "130");
    expect(decodeDatasetHead(raw(at("5", "140")), ORIGINAL, shown)?.reference).toEqual(at("5", "140"));
    expect(decodeDatasetHead(raw(shown), ORIGINAL, shown)).toBeDefined();
    for (const refused of [
      at("3", "20"), // behind the shown head
      at("5", "120"), // fewer records than shown
      at("4", "130", "00000000-0000-4000-8000-0000000000ff"), // another manifest at the shown generation
      at("5", "140", undefined, { dataset: "00000000-0000-4000-8000-0000000000b9" }),
      at("5", "140", undefined, { schemaDigest: `sha256:${"1".repeat(64)}` }),
      at("5", "140", undefined, { authorizationGeneration: "2" }),
    ]) expect(decodeDatasetHead(raw(refused), ORIGINAL, shown)).toBeUndefined();
    expect(decodeDatasetHead({ ...raw(at("5", "140")), page: {} }, ORIGINAL, shown)).toBeUndefined();
    expect(extendsReference(ORIGINAL, ORIGINAL)).toBe(true);
  });
});
