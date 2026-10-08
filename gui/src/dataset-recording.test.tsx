import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { DatasetReadError, decodeDatasetRead, type DatasetRead } from "./dataset-read";
import type { Engine } from "./engine";
import { parseExactJson } from "./exact-json";
import { decodeDatasetReference, type DatasetReference } from "./presentation/dataset";
import type { StoredValue, TypeShape } from "./protocol";
import { recordingLines } from "./surface/render/DatasetBrowser";
import { ValueBlock } from "./surface/render/ValueBlock";
import { DatasetBridge, frameBindings } from "./value-views/view-datasets";
import type { ViewDefinition } from "./value-views/definition";
import type { ViewFrame } from "./value-views/instances";
import { lineText } from "./surface/MonoLine";

/*
 * Independently authored synthetic recordings: invented run and dataset identities, sequence
 * numbers and counts. They describe no real recording, source or store.
 */
vi.mock("./surface/Cell", async (original) => ({ ...(await original<typeof import("./surface/Cell")>()), useBlockReport: () => () => undefined }));
afterEach(() => { vi.restoreAllMocks(); });

const DATASET = "00000000-0000-4000-8000-0000000000d4";
const RUN = "00000000-0000-4000-8000-0000000000e5";
const reference = (records: string, generation = "2"): DatasetReference => decodeDatasetReference({
  store: "00000000-0000-4000-8000-0000000000a1", dataset: DATASET, generation, manifest: "00000000-0000-4000-8000-0000000000c3",
  manifestDigest: `sha256:${"3".repeat(64)}`, manifestBytes: "640", schemaDigest: `sha256:${"4".repeat(64)}`, records, authorizationGeneration: "1",
})!;
/** Sequences 11..40 committed: 30 records. */
const coverage = (over: Record<string, unknown> = {}) => ({
  run: RUN, epoch: DATASET, first: "11", acceptedThrough: "40", committedThrough: "40", pending: "0", rejected: "0", termination: "natural", ...over,
});
const reply = (records: string, recording: unknown, lifecycle = "sealed") => ({ reference: { ...reference(records) }, stream: "outputs", lifecycle, protected: true, persistence: "Durable", segmentBytes: "4096", recording });

describe("recording coverage decoding", () => {
  it.each([
    ["a natural end, sealed", "30", coverage(), "sealed"],
    ["a requested stop, sealed", "30", coverage({ termination: "manual" }), "sealed"],
    ["accepted beyond committed with a known pending count", "30", coverage({ acceptedThrough: "45", pending: "5", termination: null }), "open"],
    ["an unknown pending count", "30", coverage({ acceptedThrough: "45", pending: null, termination: "unconfirmed" }), "incomplete"],
    ["an empty generation", "0", coverage({ committedThrough: "10", acceptedThrough: "12", pending: "2", termination: null }), "interrupted"],
    ["exact counts beyond a JavaScript number", "9007199254740993", coverage({ first: "1", committedThrough: "9007199254740993", acceptedThrough: "18446744073709551615", pending: null, rejected: "9007199254740995", termination: "overloaded" }), "incomplete"],
  ])("accepts %s", (_name, records, raw, lifecycle) => {
    const read = decodeDatasetRead(reply(records, raw, lifecycle), reference(records));
    expect(read?.recording).toEqual(raw);
  });

  it("keeps an ordinary analysis read without any recording", () => {
    const plain = { reference: { ...reference("30") }, stream: "outputs", lifecycle: "sealed", protected: false, persistence: "Durable", segmentBytes: "64" };
    expect(decodeDatasetRead(plain, reference("30"))).not.toHaveProperty("recording");
  });

  it.each([
    ["another dataset's epoch", coverage({ epoch: "00000000-0000-4000-8000-0000000000ff" })],
    ["an uppercase run identity", coverage({ run: RUN.toUpperCase() })],
    ["a committed interval larger than the snapshot", coverage({ committedThrough: "41", acceptedThrough: "41" })],
    ["a committed interval smaller than the snapshot", coverage({ committedThrough: "39" })],
    ["committed beyond accepted", coverage({ acceptedThrough: "39" })],
    ["a pending count that disagrees", coverage({ acceptedThrough: "45", pending: "4", termination: null })],
    ["a first sequence of zero", coverage({ first: "0", committedThrough: "29", acceptedThrough: "29" })],
    ["a leading zero", coverage({ rejected: "01" })],
    ["an exponent", coverage({ acceptedThrough: "4e1" })],
    ["a count past u64", coverage({ rejected: "18446744073709551616" })],
    ["an unknown termination", coverage({ termination: "paused" })],
    ["a surplus field", { ...coverage(), live: true }],
    ["a missing field", (({ pending: _pending, ...rest }) => rest)(coverage())],
  ])("refuses %s", (_name, raw) => {
    expect(decodeDatasetRead(reply("30", raw), reference("30"))).toBeUndefined();
  });

  it("refuses a sealed generation without a natural or requested end", () => {
    expect(decodeDatasetRead(reply("30", coverage({ termination: null })), reference("30"))).toBeUndefined();
    expect(decodeDatasetRead(reply("30", coverage({ termination: "cancelled" })), reference("30"))).toBeUndefined();
  });

  it("refuses an open or prefix generation that records an end, as no stored open manifest does", () => {
    for (const lifecycle of ["open", "prefix"]) {
      expect(decodeDatasetRead(reply("30", coverage({ termination: null }), lifecycle), reference("30"))?.lifecycle).toBe(lifecycle);
      for (const end of ["natural", "manual", "write_failed"]) {
        expect(decodeDatasetRead(reply("30", coverage({ termination: end }), lifecycle), reference("30"))).toBeUndefined();
      }
    }
    // An interrupted read comes from either a lost latest open manifest or a stored interrupted one.
    expect(decodeDatasetRead(reply("30", coverage({ termination: null }), "interrupted"), reference("30"))?.lifecycle).toBe("interrupted");
    expect(decodeDatasetRead(reply("30", coverage({ termination: "write_failed" }), "interrupted"), reference("30"))?.lifecycle).toBe("interrupted");
  });

  it("refuses numbers sent as JSON numbers, even exact ones", () => {
    const text = JSON.stringify(reply("30", coverage())).replace('"acceptedThrough":"40"', '"acceptedThrough":40');
    expect(decodeDatasetRead(parseExactJson(text), reference("30"))).toBeUndefined();
  });
});

describe("recording coverage in the browser", () => {
  const text = (segments: ReturnType<typeof recordingLines>) => segments.map(lineText);

  it("says the committed interval, accepted, pending, rejected and the end in the generation's own terms", () => {
    expect(text(recordingLines(coverage({ acceptedThrough: "45", pending: "5", rejected: "2", termination: null }) as never))).toEqual([
      "recording · sequences 11–40 committed in this generation",
      "accepted through 45 · 5 accepted but not committed · 2 rejected · no end recorded in this generation",
    ]);
    expect(text(recordingLines(coverage({ acceptedThrough: "45", pending: null, termination: "write_failed" }) as never))[1])
      .toBe("accepted through 45 · accepted but uncommitted: unknown · 0 rejected · ended: a write failed");
    expect(text(recordingLines(coverage({ committedThrough: "10", acceptedThrough: "10", pending: "0", termination: "manual" }) as never))[0])
      .toBe("recording · no sequences committed in this generation · from sequence 11");
    for (const line of text(recordingLines(coverage() as never))) expect(line).not.toMatch(/saved|recording now|active/i);
  });

  const DATASET_TYPE: TypeShape = { kind: "dataset", element: { kind: "primitive", name: "TEXT" } };
  const value: StoredValue = { type: DATASET_TYPE, provenance: {}, data: { kind: "dataset", reference: { ...reference("30") } } };
  const textOf = (item: ReactTestInstance): string => item.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
  const flush = async () => { await act(async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); }); };
  function mount() {
    const pending: { resolve: (read: DatasetRead) => void; reject: (error: unknown) => void }[] = [];
    const engine = { readDataset: vi.fn(() => new Promise<DatasetRead>((resolve, reject) => pending.push({ resolve, reject }))) } as unknown as Engine;
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ValueBlock engine={engine} value={value} stored={{ handle: `h-${Math.random()}`, generation: "g-rec" }} cacheKey={`rec-${Math.random()}`} mode="window" inCell={false} />); });
    return { tree, pending };
  }
  const page = (recording: unknown): DatasetRead => decodeDatasetRead({ ...reply("30", recording, "open"),
    page: { first: "0", next: "1", extentExhausted: false, limitedBy: null, cursor: "bmV4dA", rows: [{ ordinal: "0", sourceStart: "11", sourceEnd: "11", value: { type: { kind: "primitive", name: "TEXT" }, provenance: {}, data: "synthetic event" } }] } },
  reference("30"), 100)!;

  it("draws coverage beneath the rows and drops it with everything else on withdrawal", async () => {
    const { tree, pending } = mount();
    pending[0]!.resolve(page(coverage({ acceptedThrough: "42", pending: null, termination: null })));
    await flush();
    expect(textOf(tree.root)).toContain("recording · sequences 11–40 committed in this generation");
    expect(textOf(tree.root)).toContain("accepted but uncommitted: unknown");
    // Depth-first document order: the coverage comes after the record grid, never above the controls.
    const order = tree.root.findAll(item => typeof item.type === "string" && (item.props.role === "grid" || item.props.role === "toolbar" || item.props["aria-label"] === "Recording coverage of this generation"))
      .map(item => item.props.role ?? "coverage");
    expect(order).toEqual(["toolbar", "grid", "coverage"]);
    act(() => tree.root.findAll(item => item.type === "button" && textOf(item) === "next ›")[0]!.props.onClick());
    pending[1]!.reject(new DatasetReadError("withdrawn", 410, "DATASET_WITHDRAWN", "Access withdrawn", false));
    await flush();
    const shown = textOf(tree.root);
    expect(shown).toContain("Access withdrawn");
    for (const gone of ["recording", "accepted", "sequences", "generation", "40"]) expect(shown).not.toContain(gone);
    act(() => tree.unmount());
  });
});

describe("recording coverage for Views", () => {
  it("projects the coverage read-only without the run or epoch identities", async () => {
    const definition = { input: "Input", contracts: {
      Input: { kind: "record", fields: { events: { type: "Events", optional: false } }, constraints: {} },
      Events: { kind: "dataset", element: "Text", constraints: {} },
      Text: { kind: "scalar", primitive: "Text", constraints: { min: null, max: null, minLength: null, maxLength: null, minItems: null, maxItems: null, patterns: [], enum: [] } },
    } } as unknown as ViewDefinition;
    let resolve!: (read: DatasetRead) => void;
    const engine = { readViewDataset: vi.fn(() => new Promise<DatasetRead>(done => { resolve = done; })) } as unknown as Engine;
    const frame: ViewFrame = { root: "root", instances: [{ id: "root", instance: "i", revision: "1", inputRevision: "1", linkedInputs: [], input: { type: { kind: "unknown" }, provenance: {}, data: {} } } as never] };
    const replies: Record<string, unknown>[] = [];
    const bridge = new DatasetBridge(text => replies.push(JSON.parse(text)), 1024 * 1024);
    bridge.handle({ kind: "dataset-read", request: 1, operation: "inspect", select: "/events" }, definition,
      { events: { kind: "dataset", reference: reference("30") } }, { kind: "frame", binding: frameBindings(frame, "i", engine, "g")("view/root")! });
    resolve(decodeDatasetRead(reply("30", coverage({ acceptedThrough: "41", pending: "1", termination: null }), "open"), reference("30"))!);
    for (let i = 0; i < 6; i++) await Promise.resolve();
    expect(replies[0]).toMatchObject({ ok: true, result: { records: "30", recording: { first: "11", committedThrough: "40", acceptedThrough: "41", pending: "1", rejected: "0", termination: null } } });
    expect(JSON.stringify(replies[0])).not.toContain(RUN);
    expect(Object.keys((replies[0]!.result as { recording: object }).recording).sort()).toEqual(["acceptedThrough", "committedThrough", "first", "pending", "rejected", "termination"]);
  });

  it("passes an earlier prefix's lifecycle through unchanged", async () => {
    const definition = { input: "Input", contracts: {
      Input: { kind: "record", fields: { events: { type: "Events", optional: false } }, constraints: {} },
      Events: { kind: "dataset", element: "Text", constraints: {} },
      Text: { kind: "scalar", primitive: "Text", constraints: { min: null, max: null, minLength: null, maxLength: null, minItems: null, maxItems: null, patterns: [], enum: [] } },
    } } as unknown as ViewDefinition;
    let resolve!: (read: DatasetRead) => void;
    const engine = { readViewDataset: vi.fn(() => new Promise<DatasetRead>(done => { resolve = done; })) } as unknown as Engine;
    const frame: ViewFrame = { root: "root", instances: [{ id: "root", instance: "i", revision: "1", inputRevision: "1", linkedInputs: [], input: { type: { kind: "unknown" }, provenance: {}, data: {} } } as never] };
    const replies: Record<string, unknown>[] = [];
    const bridge = new DatasetBridge(text => replies.push(JSON.parse(text)), 1024 * 1024);
    bridge.handle({ kind: "dataset-read", request: 1, operation: "inspect", select: "/events" }, definition,
      { events: { kind: "dataset", reference: reference("30") } }, { kind: "frame", binding: frameBindings(frame, "i", engine, "g")("view/root")! });
    resolve(decodeDatasetRead(reply("30", coverage({ termination: null }), "prefix"), reference("30"))!);
    for (let i = 0; i < 6; i++) await Promise.resolve();
    expect(replies[0]).toMatchObject({ ok: true, result: { records: "30", lifecycle: "prefix", recording: { termination: null } } });
  });
});
