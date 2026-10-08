import { afterEach, describe, expect, it, vi } from "vitest";
import { Engine } from "../engine";
import { DatasetReadError, type DatasetRead } from "../dataset-read";
import { decodeDatasetReference, type DatasetReference } from "../presentation/dataset";
import type { TypeShape } from "../protocol";
import { decodeContract, type ViewDefinition, type ContractSchema } from "./definition";
import { DatasetBridge, datasetTarget, frameBindings, viewDatasetRequest, type DatasetRoute, type ViewDatasetBinding } from "./view-datasets";
import type { ViewFrame } from "./instances";

/* Synthetic View definition, frame and snapshot: invented identities, revisions and rows. */

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

const none = { min: null, max: null, minLength: null, maxLength: null, minItems: null, maxItems: null, patterns: [], enum: [] };
const scalar = (primitive: string): ContractSchema => ({ kind: "scalar", primitive, constraints: none });
const definition = {
  name: "Events", id: "synthetic.events", digest: "sha256:synthetic", summary: "", input: "Input", inputModes: [], inputReferences: ["current"], inputDelivery: ["finite"],
  outputs: { picked: { type: "Events", mode: "state", shared: false } }, outputScope: "local", interaction: null, execution: "none", slots: {},
  eventWindow: { items: 0, bytes: 0, read: "", retention: "", overflow: "" },
  contracts: {
    Input: { kind: "record", fields: { events: { type: "Events", optional: false }, maybe: { type: "MaybeEvents", optional: true }, linked: { type: "Events", optional: true }, title: { type: "Text", optional: false } }, constraints: none },
    Events: { kind: "dataset", element: "Row", constraints: none },
    MaybeEvents: { kind: "option", element: "Events", constraints: none },
    Row: { kind: "record", fields: { label: { type: "Text", optional: false }, count: { type: "Int", optional: false } }, constraints: none },
    Text: scalar("Text"), Int: scalar("Int"),
  },
} as unknown as ViewDefinition;

const reference = (records = "9007199254740993"): DatasetReference => decodeDatasetReference({
  store: "00000000-0000-4000-8000-0000000000a1", dataset: "00000000-0000-4000-8000-0000000000b2", generation: "3",
  manifest: "00000000-0000-4000-8000-0000000000c3", manifestDigest: `sha256:${"0123456789abcdef".repeat(4)}`, manifestBytes: "512",
  schemaDigest: `sha256:${"fedcba9876543210".repeat(4)}`, records, authorizationGeneration: "1",
})!;
const TEXT: TypeShape = { kind: "primitive", name: "TEXT" }, INT: TypeShape = { kind: "primitive", name: "INT" };
const ROW: TypeShape = { kind: "record", name: "Row", fields: [{ name: "label", type: TEXT }, { name: "count", type: INT }] };
const INPUT: TypeShape = { kind: "record", name: "Input", fields: [{ name: "events", type: { kind: "dataset", element: ROW } }, { name: "title", type: TEXT }] };
const wire = (ref = reference()) => ({ events: { kind: "dataset", reference: { ...ref } }, title: "synthetic" });
const input = () => decodeContract(definition, "Input", wire(), true, INPUT);

describe("Dataset input contract", () => {
  it("accepts exactly a snapshot descriptor whose captured element matches the declared rows", () => {
    expect(input()).toMatchObject({ events: { kind: "dataset", reference: { records: "9007199254740993" } } });
    expect(Object.isFrozen((input() as { events: object }).events)).toBe(true);
  });
  it.each([
    ["a numeric count", { ...wire(), events: { kind: "dataset", reference: { ...reference(), records: 12 } } }],
    ["a surplus descriptor key", { ...wire(), events: { kind: "dataset", reference: reference(), url: "/elsewhere" } }],
    ["a bad digest", { ...wire(), events: { kind: "dataset", reference: { ...reference(), schemaDigest: "md5:00" } } }],
    ["a list in place of a Dataset", { ...wire(), events: [] }],
  ])("refuses %s", (_name, raw) => {
    expect(() => decodeContract(definition, "Input", raw, true, INPUT)).toThrow();
  });
  it("refuses a captured element that is not the declared row contract", () => {
    const other: TypeShape = { kind: "record", name: "Input", fields: [{ name: "events", type: { kind: "dataset", element: TEXT } }, { name: "title", type: TEXT }] };
    expect(() => decodeContract(definition, "Input", wire(), true, other)).toThrow();
  });
  it("never lets a View's own outputs or state hold a Dataset", () => {
    expect(() => decodeContract(definition, "Events", { kind: "dataset", reference: reference() })).toThrow();
  });
});

describe("request shape and target", () => {
  it("accepts only a pointer, a canonical position and a bounded limit", () => {
    expect(viewDatasetRequest({ kind: "dataset-read", request: 1, operation: "page", select: "/events", position: { from: "9007199254740993" }, limit: 20 }))
      .toEqual({ request: 1, operation: "page", select: "/events", position: { from: "9007199254740993" }, limit: 20 });
    for (const bad of [
      { kind: "dataset-read", request: 1, operation: "page", select: "https://elsewhere/datasets" },
      { kind: "dataset-read", request: 1, operation: "page", select: "/events", position: { from: "01" } },
      { kind: "dataset-read", request: 1, operation: "page", select: "/events", limit: 101 },
      { kind: "dataset-read", request: 1, operation: "page", select: "/events", handle: "h" },
      { kind: "dataset-read", request: 1, operation: "inspect", select: "/events", limit: 1 },
      { kind: "dataset-read", request: 1, operation: "delete", select: "/events" },
    ]) expect(viewDatasetRequest(bad)).toBeUndefined();
  });
  it("reaches only declared Dataset fields of the drawn input, never through an Option or a linked field", () => {
    expect(datasetTarget(definition, input(), "/events")).toMatchObject({ element: "Row", reference: { generation: "3" } });
    expect(datasetTarget(definition, input(), "/title")).toBeUndefined();
    expect(datasetTarget(definition, { ...(input() as object), maybe: { kind: "dataset", reference: reference() } }, "/maybe")).toBeUndefined();
    expect(datasetTarget(definition, input(), "/events", ["events"])).toBeUndefined();
  });
});

describe("Engine.readViewDataset", () => {
  it("binds the exact frame, member and revisions and reads only by relative pointer", async () => {
    const reply: DatasetRead = { reference: reference("2"), lifecycle: "sealed", protected: false, persistence: "Durable", segmentBytes: "64",
      page: { first: "0", next: "1", extentExhausted: false, limitedBy: null, cursor: "Y3Vyc29y",
        rows: [{ ordinal: "0", sourceStart: "0", sourceEnd: "9", value: { type: ROW, provenance: {}, data: { label: "first", count: 1 } } }] } };
    const fetch = vi.fn(async () => new Response(JSON.stringify(reply), { status: 200 }));
    vi.stubGlobal("fetch", fetch);
    const engine = Object.assign(new Engine("synthetic"), { generation: "g-view" }) as Engine;
    const binding: ViewDatasetBinding = { engine, generation: "g-view", root: "root node", rootInstance: "root-instance", member: "member", revision: "7", inputRevision: "9007199254740993", linkedInputs: [] };
    const read = await engine.readViewDataset(binding, reference("2"), "/events", { from: "0" }, 1, new AbortController().signal);
    expect(read.page?.rows[0]?.ordinal).toBe("0");
    const [url, init] = fetch.mock.calls[0]! as unknown as [string, { headers: Record<string, string>; method?: string }];
    expect(url).toBe("/view-datasets/root%20node/root-instance/member?select=%2Fevents&from=0&limit=1");
    expect(init.headers).toMatchObject({ "X-Wes-Session": "g-view", "X-Wes-View-Revision": "7", "X-Wes-Input-Revision": "9007199254740993" });
    expect(init.method).toBeUndefined();
  });

  it("refuses a reply larger than 1 MiB without decoding it", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response("x".repeat(1024 * 1024 + 1), { status: 200 })));
    const engine = Object.assign(new Engine(), { generation: "g" }) as Engine;
    const binding: ViewDatasetBinding = { engine, generation: "g", root: "r", rootInstance: "i", member: "m", revision: "1", inputRevision: "1", linkedInputs: [] };
    await expect(engine.readViewDataset(binding, reference(), "/events", { from: "0" }, 1, new AbortController().signal)).rejects.toMatchObject({ kind: "limit" });
  });
});

describe("DatasetBridge", () => {
  type Pending = { resolve: (read: DatasetRead) => void; reject: (error: unknown) => void; args: unknown[] };
  function harness(maxBytes = 1024 * 1024) {
    const pending: Pending[] = [];
    const readViewDataset = vi.fn((...args: unknown[]) => new Promise<DatasetRead>((resolve, reject) => pending.push({ resolve, reject, args })));
    const submit = vi.fn();
    const engine = { readViewDataset, readDataset: vi.fn(), submit } as unknown as Engine;
    const frame: ViewFrame = { root: "root", instances: [
      { id: "root", instance: "root-instance", revision: "4", inputRevision: "5", input: { type: INPUT, provenance: {}, data: wire() }, linkedInputs: [] } as never,
    ] };
    const binding = frameBindings(frame, "root-instance", engine, "g-b")("view/root")!;
    const replies: Record<string, unknown>[] = [];
    const bridge = new DatasetBridge(text => replies.push(JSON.parse(text)), maxBytes);
    const route: DatasetRoute = { kind: "frame", binding };
    const ask = (request: number, extra: Record<string, unknown> = {}) => bridge.handle({ kind: "dataset-read", request, operation: "page", select: "/events", ...extra }, definition, input(), route);
    return { bridge, ask, pending, replies, readViewDataset, submit, binding };
  }
  const page = (rows: unknown[]): DatasetRead => ({ reference: reference(), lifecycle: "open", protected: false, persistence: "Durable", segmentBytes: "64",
    page: { first: "0", next: String(rows.length), extentExhausted: false, limitedBy: null, cursor: "bmV4dA", rows: rows as never } });
  const flush = async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); };

  it("binds reads to the drawn member and decodes rows against the declared contract", async () => {
    const { ask, pending, replies, binding, submit } = harness();
    expect(binding).toMatchObject({ root: "root", rootInstance: "root-instance", member: "root", revision: "4", inputRevision: "5", generation: "g-b" });
    ask(1, { position: { from: "0" }, limit: 2 });
    expect(pending[0]!.args.slice(1, 5)).toEqual([reference(), "/events", { from: "0" }, 2]);
    pending[0]!.resolve(page([{ ordinal: "0", sourceStart: "0", sourceEnd: "4", value: { type: ROW, provenance: {}, data: { label: "first", count: 1 } } }]));
    await flush();
    expect(replies).toEqual([{ kind: "dataset-reply", request: 1, ok: true, result: expect.objectContaining({ generation: "3", records: "9007199254740993", lifecycle: "open",
      cursor: "bmV4dA", rows: [{ ordinal: "0", sourceStart: "0", sourceEnd: "4", value: { label: "first", count: 1 } }] }) }]);
    expect(submit).not.toHaveBeenCalled();
  });

  it("refuses rows that do not satisfy the declared element contract", async () => {
    const { ask, pending, replies } = harness();
    ask(1);
    pending[0]!.resolve(page([{ ordinal: "0", sourceStart: "0", sourceEnd: "4", value: { type: ROW, provenance: {}, data: { label: 7, count: 1 } } }]));
    await flush();
    expect(replies).toEqual([{ kind: "dataset-reply", request: 1, ok: false, error: "invalid" }]);
  });

  it("keeps at most two reads in flight and refuses unsupported sources explicitly", () => {
    const { bridge, ask, replies, readViewDataset } = harness();
    ask(1); ask(2); ask(3);
    expect(readViewDataset).toHaveBeenCalledTimes(2);
    expect(replies).toEqual([{ kind: "dataset-reply", request: 3, ok: false, error: "busy" }]);
    bridge.handle({ kind: "dataset-read", request: 4, operation: "page", select: "/events" }, definition, input(), { kind: "none" });
    expect(replies.at(-1)).toEqual({ kind: "dataset-reply", request: 4, ok: false, error: "unavailable" });
  });

  it("settles reads in flight as changed on a new input and never delivers their late replies", async () => {
    const { bridge, ask, pending, replies } = harness();
    ask(1);
    bridge.reset();
    expect(replies).toEqual([{ kind: "dataset-reply", request: 1, ok: false, error: "changed" }]);
    expect((pending[0]!.args[5] as AbortSignal).aborted).toBe(true);
    pending[0]!.resolve(page([]));
    await flush();
    expect(replies).toHaveLength(1);
  });

  it("reports a host continuity refusal as a plain failed read, never as a workspace change, and never asks for a head or extent", async () => {
    const { ask, pending, replies, readViewDataset } = harness();
    ask(1, { position: { from: "0" }, limit: 2 });
    pending[0]!.reject(new DatasetReadError("continuity", 409, "DATASET_CONTINUITY_CHANGED", "private detail", false));
    await flush();
    expect(replies).toEqual([{ kind: "dataset-reply", request: 1, ok: false, error: "failed" }]);
    expect(JSON.stringify(replies)).not.toContain("private detail");
    // The View route takes the frozen input only: its call carries no head or extent target.
    expect(readViewDataset.mock.calls[0]).toHaveLength(6);
  });

  it("maps withdrawal and session changes to coarse codes and drops replies after close", async () => {
    const { bridge, ask, pending, replies } = harness();
    ask(1); ask(2);
    pending[0]!.reject(new DatasetReadError("withdrawn", 410, "DATASET_WITHDRAWN", "Access withdrawn", false));
    pending[1]!.reject(new DatasetReadError("session", 409, "DATASET_SESSION_CHANGED", "private detail", false));
    await flush();
    expect(replies).toEqual([{ kind: "dataset-reply", request: 1, ok: false, error: "withdrawn" }, { kind: "dataset-reply", request: 2, ok: false, error: "changed" }]);
    ask(3);
    bridge.close();
    pending[2]!.resolve(page([]));
    await flush();
    expect(replies).toHaveLength(2);
  });

  it("replaces a reply over the frame's input budget with a limit refusal", async () => {
    const { ask, pending, replies } = harness(200);
    ask(1);
    pending[0]!.resolve(page([{ ordinal: "0", sourceStart: "0", sourceEnd: "4", value: { type: ROW, provenance: {}, data: { label: "x".repeat(400), count: 1 } } }]));
    await flush();
    expect(replies).toEqual([{ kind: "dataset-reply", request: 1, ok: false, error: "limit" }]);
  });
});
