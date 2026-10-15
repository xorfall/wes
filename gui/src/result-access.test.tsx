vi.mock("./workspace-events", () => ({ WorkspaceEvents: function(path: string) { return new EventSource(path); } }));
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { Engine } from "./engine";
import type { Event, StoredValue } from "./protocol";
import { decodeEvent } from "./protocol-decode";
import { ResultReadError, ResultReader, ResultWithdrawnError } from "./result-reader";
import { LiveViewReader } from "./live-view-reader";
import { ViewFrameReader, type FrameSample } from "./value-views/instances";
import { apply, emptyWorkspace, type Workspace, type WorkspaceNode } from "./workspace";
import { ResultObservations } from "./surface/results";
import { Cell, type CellBlock, type CellProps } from "./surface/Cell";
import { ResultType } from "./surface/ResultType";
import { datasetWithdrawals, WITHDRAWN_TITLE } from "./surface/render/dataset-source";
import { OpenScreen } from "./surface/screens/Open";
import { peekOf, PeekScreen } from "./surface/screens/Peek";
import { memberProblem } from "./dashboard/sources";
import { decodeDatasetReference } from "./presentation/dataset";

/* Synthetic workspace events and values only: invented node names, handles and payloads. */

class Events {
  static current: Events;
  onmessage?: (message: { data: string }) => void;
  onerror?: () => void;
  constructor() { Events.current = this; }
  close() {}
  say(event: unknown) { this.onmessage?.({ data: JSON.stringify(event) }); }
}
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

const value: StoredValue = { type: { kind: "primitive", name: "TEXT" }, provenance: {}, data: "synthetic secret row" };
const ready = (node: string, handle: string) => ({ event: "ready", node, handle, type: "Text", bytes: 20, provenance: {}, cautions: [], kept: false });
const access = (node: string) => ({ event: "result-access", node, readable: false });
const restored = (node: string, handle: string | null) => ({ event: "result-access", node, readable: true, handle, epoch: "e".repeat(64) });
const stale = (node: string) => ({ event: "node", node, state: "stale", staleReason: { code: "result_withdrawn", message: "Access to the result was withdrawn. Cached data and value-derived metadata were cleared; no work was replayed." } });
const created = (node: string) => ({ event: "created", node, name: "rows", command: ":synthetic rows", dependsOn: [], dependencyLifetime: "continuous", interactive: false });
const textOf = (item: ReactTestInstance): string => item.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };

/** An engine listening to a scripted event source; value reads go through a controllable fetch. */
function connected(generation = "g1") {
  vi.stubGlobal("EventSource", Events);
  vi.stubGlobal("fetch", vi.fn(async () => { throw new Error("no view packages in this test"); }));
  const replies: ((body: unknown) => void)[] = [];
  const reads = vi.fn((_url: string) => new Promise(resolve => replies.push(body => resolve({ ok: true, status: 200, text: async () => JSON.stringify(body) }))));
  vi.stubGlobal("window", { fetch: reads });
  const engine = new Engine();
  const heard: { event: Event; withdrawn: boolean }[] = [];
  engine.listen(event => heard.push({ event, withdrawn: datasetWithdrawals.has({ handle: "h-old", generation }) }), () => {});
  Events.current.say({ event: "session", generation });
  return { engine, reads, replies, heard };
}

describe("result-access decoding", () => {
  it("accepts exactly a withdrawal for a named node", () => {
    expect(decodeEvent(access("n1"))).toEqual(access("n1"));
  });
  it("accepts a bounded host proof naming only the current capture", () => {
    expect(decodeEvent(restored("n1", "current"))).toEqual(restored("n1", "current"));
    expect(decodeEvent(restored("n1", null))).toEqual(restored("n1", null));
    for (const raw of [{ ...restored("n1", "current"), epoch: "old" }, { ...restored("n1", "current"), handle: [] }, { ...restored("n1", "current"), extra: true }]) expect(() => decodeEvent(raw)).toThrow();
  });
  it.each([
    ["a grant", { event: "result-access", node: "n1", readable: true }],
    ["a missing readable", { event: "result-access", node: "n1" }],
    ["an empty node", { event: "result-access", node: "", readable: false }],
    ["a numeric node", { event: "result-access", node: 1, readable: false }],
    ["an extra key", { event: "result-access", node: "n1", readable: false, dataset: "00000000-0000-4000-8000-0000000000b2" }],
    ["a text readable", { event: "result-access", node: "n1", readable: "false" }],
  ])("refuses %s", (_name, raw) => {
    expect(() => decodeEvent(raw)).toThrow();
  });
});

describe("Engine handling before distribution", () => {
  it("does not publish a value if authority changes while its renderer catalogue loads", async () => {
    const { engine, replies } = connected("g-renderer");
    let completeCatalogue!: (reply: unknown) => void;
    const catalogue = vi.fn(() => new Promise(resolve => { completeCatalogue = resolve; }));
    vi.stubGlobal("fetch", catalogue);
    Events.current.say(ready("n1", "current"));
    const reading = engine.fetch("current");
    await flush();
    replies[0]!({ ...value, data: { view: "synthetic.pending-renderer" } });
    await flush();
    expect(catalogue).toHaveBeenCalledTimes(1);
    Events.current.say(restored("n1", "current"));
    completeCatalogue({ ok: true, text: async () => "[]" });
    await expect(reading).rejects.toBeInstanceOf(ResultWithdrawnError);
  });
  it("rejects an old Dataset body after a coalesced fresh proof and allows a fresh read", async () => {
    const { engine } = connected("g-dataset");
    const reference = decodeDatasetReference({
      store: "00000000-0000-4000-8000-0000000000a1", dataset: "00000000-0000-4000-8000-0000000000b2",
      generation: "1", manifest: "00000000-0000-4000-8000-0000000000c3", manifestDigest: `sha256:${"a".repeat(64)}`,
      manifestBytes: "512", schemaDigest: `sha256:${"b".repeat(64)}`, records: "0", authorizationGeneration: "1",
    })!;
    const body = { reference, stream: "outputs", lifecycle: "sealed", protected: false, persistence: "Durable", segmentBytes: "0" };
    let completeOldBody!: (text: string) => void;
    const transport = vi.fn().mockResolvedValueOnce({ ok: true, status: 200, text: () => new Promise(resolve => { completeOldBody = resolve; }) })
      .mockResolvedValue({ ok: true, status: 200, text: async () => JSON.stringify(body) });
    vi.stubGlobal("fetch", transport);
    Events.current.say(ready("n1", "current"));
    const reading = engine.readDataset("current", "g-dataset", reference, "", undefined, 20, new AbortController().signal);
    await flush();
    Events.current.say(restored("n1", "current"));
    completeOldBody(JSON.stringify(body));
    await expect(reading).rejects.toMatchObject({ name: "DatasetReadError", kind: "authority", code: "DATASET_READ_AUTHORITY_CHANGED", retryable: false });
    await expect(engine.readDataset("current", "g-dataset", reference, "", undefined, 20, new AbortController().signal))
      .resolves.toMatchObject({ reference, lifecycle: "sealed" });
    expect(transport).toHaveBeenCalledTimes(2);
  });
  it("a coalesced new proof closes older reads even when no false notice was delivered", async () => {
    const { engine, reads, replies } = connected("g-coalesced");
    Events.current.say(ready("n1", "current"));
    const oldRead = engine.fetch("current");
    await flush();
    Events.current.say(restored("n1", "current"));
    const freshRead = engine.fetch("current");
    await flush();
    expect(reads).toHaveBeenCalledTimes(2);
    replies[0]!(value);
    await expect(oldRead).rejects.toBeInstanceOf(ResultWithdrawnError);
    replies[1]!(value);
    await expect(freshRead).resolves.toMatchObject({ data: "synthetic secret row" });
  });
  it("fresh proof opens only its exact capture while every old request stays closed", async () => {
    const { engine, reads, replies } = connected("g-proof");
    Events.current.say(ready("n1", "older-scope"));
    Events.current.say(ready("n1", "current-scope"));
    const oldRead = engine.fetch("current-scope");
    await flush();
    Events.current.say(access("n1"));
    Events.current.say(restored("n1", "current-scope"));
    expect(datasetWithdrawals.has({ handle: "current-scope", generation: "g-proof" })).toBe(false);
    expect(datasetWithdrawals.has({ handle: "older-scope", generation: "g-proof" })).toBe(true);
    const freshRead = engine.fetch("current-scope");
    await flush();
    expect(reads).toHaveBeenCalledTimes(2);
    replies[0]!(value);
    await expect(oldRead).rejects.toBeInstanceOf(ResultWithdrawnError);
    replies[1]!(value);
    await expect(freshRead).resolves.toMatchObject({ data: "synthetic secret row" });
    await expect(engine.fetch("older-scope")).rejects.toBeInstanceOf(ResultWithdrawnError);
  });
  it("drops a read in flight when the notice arrives, and never reads that handle again", async () => {
    const { engine, reads, replies, heard } = connected("g-pending");
    Events.current.say(ready("n1", "h-old"));
    const reading = engine.fetch("h-old");
    await flush();
    expect(reads).toHaveBeenCalledTimes(1);
    Events.current.say(access("n1"));
    // The server's reply was already on its way; it lands nowhere.
    replies[0]!(value);
    await expect(reading).rejects.toBeInstanceOf(ResultWithdrawnError);
    await expect(engine.fetch("h-old")).rejects.toBeInstanceOf(ResultWithdrawnError);
    expect(reads).toHaveBeenCalledTimes(1);
    expect(datasetWithdrawals.has({ handle: "h-old", generation: "g-pending" })).toBe(true);
    expect(heard.map(it => it.event.event)).toContain("result-access");
  });

  it("clears caches before any consumer hears the notice", () => {
    const { heard } = connected("g1");
    Events.current.say(ready("n1", "h-old"));
    Events.current.say(access("n1"));
    expect(heard.find(it => it.event.event === "result-access")?.withdrawn).toBe(true);
  });

  it("leaves a new explicitly produced handle readable and keeps the old one withdrawn", async () => {
    const { engine, reads, replies } = connected("g-new");
    Events.current.say(ready("n1", "h-before"));
    Events.current.say(access("n1"));
    Events.current.say(ready("n1", "h-after"));
    const reading = engine.fetch("h-after");
    await flush();
    replies[0]!(value);
    await expect(reading).resolves.toMatchObject({ data: "synthetic secret row" });
    expect(reads.mock.calls[0]![0]).toBe("/values/h-after");
    expect(datasetWithdrawals.has({ handle: "h-after", generation: "g-new" })).toBe(false);
    await expect(engine.fetch("h-before")).rejects.toBeInstanceOf(ResultWithdrawnError);
  });

  it("keeps a withdrawn handle withdrawn across a reconnect into a new session", async () => {
    const { engine, reads } = connected("g-a");
    Events.current.say(ready("n1", "h-gone"));
    Events.current.say(access("n1"));
    Events.current.say({ event: "session", generation: "g-b" });
    // The full projection replays the notice before the stale state; it carries no handle.
    Events.current.say(created("n1"));
    Events.current.say(access("n1"));
    Events.current.say(stale("n1"));
    expect(datasetWithdrawals.has({ handle: "h-gone", generation: "g-b" })).toBe(true);
    await expect(engine.fetch("h-gone")).rejects.toBeInstanceOf(ResultWithdrawnError);
    expect(reads).not.toHaveBeenCalled();
  });
});

describe("Workspace projection of the notice", () => {
  const run = (events: readonly unknown[]): Workspace => events.reduce<Workspace>((workspace, event) => apply(workspace, decodeEvent(event)), emptyWorkspace);
  it("drops the handle and value-derived facts and keeps the command", () => {
    const node = run([created("n1"), ready("n1", "h-old"), access("n1"), stale("n1")]).nodes[0]!;
    expect(node).toMatchObject({ id: "n1", command: ":synthetic rows", state: "stale", accessWithdrawn: true, kept: false });
    expect(node.handle).toBeUndefined();
    expect(node.type).toBeUndefined();
    expect(node.bytes).toBeUndefined();
  });
  it("replays on a fresh projection without inventing a handle", () => {
    const node = run([{ event: "session", generation: "g" }, created("n1"), access("n1"), stale("n1")]).nodes[0]!;
    expect(node.accessWithdrawn).toBe(true);
    expect(node.handle).toBeUndefined();
  });
  it("ends with a new run's own result, which brings only its new handle", () => {
    const withdrawn = run([created("n1"), ready("n1", "h-old"), access("n1"), stale("n1")]);
    const rerun = apply(apply(withdrawn, decodeEvent({ event: "node", node: "n1", state: "running" })), decodeEvent(ready("n1", "h-new")));
    expect(rerun.nodes[0]).toMatchObject({ handle: "h-new", state: "ready" });
    expect(rerun.nodes[0]!.accessWithdrawn).toBeUndefined();
  });
  it("does not let the last observation stand in for a withdrawn value", () => {
    const observations = new ResultObservations();
    const before = run([created("n1"), ready("n1", "h-old")]).nodes;
    expect(observations.select(before, new Map([["h-old", value]]), new Map()).get("n1")?.value).toBe(value);
    const after = run([created("n1"), ready("n1", "h-old"), access("n1"), stale("n1")]).nodes;
    expect(observations.select(after, new Map(), new Map()).get("n1")).toBeUndefined();
  });
});

describe("Display caches", () => {
  it("ignores an old abort after revalidation and resumes fresh display reads only", async () => {
    vi.useFakeTimers();
    let rejectOld!: (error: Error) => void;
    const fetch = vi.fn().mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectOld = reject; }))
      .mockResolvedValue({ value, metadata: { revision: "2", epochs: [], sources: [] } });
    const reader = new LiveViewReader(fetch);
    const seen: unknown[] = [];
    const stop = reader.watch("n1", "g", sample => seen.push(sample));
    await vi.advanceTimersByTimeAsync(150);
    reader.withdraw("n1");
    reader.restore("n1");
    rejectOld(new Error("old aborted read"));
    await flush();
    expect(seen.at(-1)).toEqual({ revision: 0 });
    await vi.advanceTimersByTimeAsync(150);
    expect(seen.at(-1)).toMatchObject({ value, metadata: { revision: "2" } });
    expect(reader.cellSnapshot("n1", "g")).toBeUndefined();
    stop();
  });
  it("drops held live snapshots and tells watchers at once, without a later budget complaint", async () => {
    vi.useFakeTimers();
    let abort!: () => void;
    const reader = new LiveViewReader((_node, _generation, signal) => new Promise((_resolve, reject) => { abort = () => reject(new Error("aborted")); signal.addEventListener("abort", abort); }));
    reader.holdCell("n1", "g", { sample: { revision: 1, value }, at: "09:14" });
    const seen: unknown[] = [];
    reader.watch("n1", "g", sample => seen.push(sample));
    await vi.advanceTimersByTimeAsync(150);
    reader.withdraw("n1");
    await flush();
    expect(reader.cellSnapshot("n1", "g")).toBeUndefined();
    expect(seen.at(-1)).toMatchObject({ withdrawn: true, problemCode: "withdrawn" });
    expect(JSON.stringify(seen.at(-1))).not.toContain("synthetic secret row");
  });

  it("purges held view frames and their revision tags so no withdrawn input stays drawn", async () => {
    vi.useFakeTimers();
    const read = vi.fn(async (_node: string, _generation: string, _signal: AbortSignal, etag?: string) => ({ frame: { root: "v", instances: [], seen: etag ?? "none" } as never, etag: "tag-1" }));
    const frames = new ViewFrameReader(read);
    const seen: FrameSample[] = [];
    frames.watch("v", "g", sample => seen.push(sample));
    await vi.advanceTimersByTimeAsync(150);
    expect(seen.at(-1)?.frame).toBeDefined();
    frames.purge();
    expect(seen.at(-1)).toEqual({});
    await vi.advanceTimersByTimeAsync(150);
    // Read afresh, without the old revision tag.
    expect(read.mock.calls.at(-1)?.[3]).toBeUndefined();
  });
});

describe("Consumers", () => {
  const node: WorkspaceNode = { id: "n1", name: "rows", command: ":synthetic rows", dependsOn: [], state: "stale", provenance: {}, cautions: [], kept: false, run: "r1", accessWithdrawn: true };

  it("keeps a collapsed cell header in place with a type-free withdrawal header", () => {
    const block: CellBlock = { key: "n1", identity: { id: "n1", label: "$rows", glyph: "ready" }, open: true, hasValue: true, type: { kind: "primitive", name: "TEXT" },
      typeLabel: "Text", header: [{ text: "3 items", role: "mono-dim" }], accessWithdrawn: true, content: <span>notice</span> };
    const props: CellProps = { theme: "keys", state: "default", label: "c", rows: [{ segments: [{ text: ":synthetic rows" }], nodes: [block.identity!] }], verdict: [], time: "09:14", blocks: [block], view: "collapsed" };
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Cell {...props} />); });
    expect(tree.root.findByType(ResultType).props).toMatchObject({ shape: undefined, meta: undefined, label: "Result" });
    expect(textOf(tree.root)).not.toContain("3 items");
    expect(textOf(tree.root)).toContain("Access withdrawn");
    expect(textOf(tree.root)).toContain(":synthetic rows");
    expect(tree.root.findAll(item => item.type === "header" && item.props.className === "result-header")).toHaveLength(1);
    act(() => tree.unmount());
  });

  it("says so in peeks and /open, in this or a separate window, and keeps the command", async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<PeekScreen top={[]} subject={[{ text: "$rows", role: "mono-ref" }]} what="value" {...peekOf(node, undefined)} />); });
    expect(textOf(tree.root)).toContain(WITHDRAWN_TITLE);
    await act(async () => { tree.root.findByProps({ "aria-label": "Copy to the clipboard" }).props.onClick(); });
    expect(writeText).toHaveBeenCalledWith(expect.stringContaining(WITHDRAWN_TITLE));
    act(() => tree.update(<PeekScreen top={[]} subject={[]} what="source" {...peekOf(node, undefined)} />));
    expect(tree.root.findByProps({ "aria-label": "Command source" }).props.value).toContain(":synthetic rows");
    act(() => tree.update(<OpenScreen top={[]} subject={[{ text: "$rows", role: "mono-ref" }, { text: "Text" }]} tab="result" viewing={{ node }} />));
    expect(textOf(tree.root)).toContain(WITHDRAWN_TITLE);
    expect(textOf(tree.root)).not.toContain("Text");
    act(() => tree.unmount());
  });

  it("explains a withdrawn dashboard member without reading it", () => {
    expect(memberProblem({ node: "n1", generation: "g" } as never, "g", node)).toContain("Access withdrawn");
  });
});

describe("Server refusal of a withdrawn handle", () => {
  type Reply = { ok: boolean; status: number; text: () => Promise<string> };
  const refusal = (status: number, code: string, handle: string, retryable = false): Reply => ({ ok: false, status,
    text: async () => JSON.stringify({ error: { code, message: "Result access was withdrawn; no command was rerun.", retryable, retryAfterMs: null, context: { operation: "read-value", handle } } }) });
  const body = (data: unknown): Reply => ({ ok: true, status: 200, text: async () => JSON.stringify(data) });
  /** A reader whose replies are released by hand, recording every refusal it reports. */
  function reader(generation = "g-r") {
    const replies = new Map<string, (reply: Reply) => void>();
    const fetch = vi.fn((url: string) => new Promise<Reply>(resolve => replies.set(decodeURIComponent(url.slice("/values/".length)), resolve)));
    vi.stubGlobal("window", { fetch });
    const refused: [string, string | undefined][] = [];
    const results = new ResultReader(() => generation, () => 15000, () => undefined, (handle, at) => refused.push([handle, at]));
    return { results, fetch, replies, refused };
  }

  it("withdraws on structured 403 without any notice: no retry, shared gate set, nothing drawn", async () => {
    const { results, fetch, replies, refused } = reader("g-403");
    const reading = results.read("h-403");
    await flush();
    replies.get("h-403")!(refusal(403, "VALUE_ACCESS_WITHDRAWN", "h-403"));
    await expect(reading).rejects.toBeInstanceOf(ResultWithdrawnError);
    expect(refused).toEqual([["h-403", "g-403"]]);
    await expect(results.read("h-403")).rejects.toBeInstanceOf(ResultWithdrawnError);
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("feeds the Engine's refusal into the shared stored gate that clears headers and peeks", async () => {
    const { engine, reads } = connected("g-engine");
    reads.mockImplementationOnce(async () => refusal(403, "VALUE_ACCESS_WITHDRAWN", "h-engine") as never);
    await expect(engine.fetch("h-engine")).rejects.toBeInstanceOf(ResultWithdrawnError);
    expect(datasetWithdrawals.has({ handle: "h-engine", generation: "g-engine" })).toBe(true);
    // The cell header for that stored identity now says only the generic notice.
    const block: CellBlock = { key: "n1", identity: { id: "n1", label: "$rows", glyph: "ready" }, open: true, hasValue: true, type: value.type,
      typeLabel: "Text", header: [{ text: "3 items", role: "mono-dim" }], stored: { handle: "h-engine", generation: "g-engine" }, content: <span>body</span> };
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Cell theme="keys" state="default" label="c" rows={[]} verdict={[]} time="09:14" blocks={[block]} />); });
    expect(tree.root.findByType(ResultType).props).toMatchObject({ shape: undefined, label: "Result" });
    expect(textOf(tree.root)).not.toContain("3 items");
    act(() => tree.unmount());
  });

  it("treats private 410 as withdrawal, but keeps 404, a different handle, a retryable code and 500 apart", async () => {
    const { results, replies, refused } = reader("g-cls");
    const cases: [string, Reply, boolean][] = [
      ["h-410", refusal(410, "VALUE_PRIVATE_UNAVAILABLE", "h-410"), true],
      ["h-404", refusal(404, "VALUE_MISSING", "h-404"), false],
      ["h-other", refusal(403, "VALUE_ACCESS_WITHDRAWN", "h-elsewhere"), false],
      ["h-flag", refusal(403, "VALUE_ACCESS_WITHDRAWN", "h-flag", true), false],
      ["h-500", { ok: false, status: 500, text: async () => "upstream failure" }, false],
      ["h-500s", refusal(500, "VALUE_READ_FAILED", "h-500s"), false],
      ["h-capacity", refusal(413, "VALUE_CAPACITY", "h-capacity"), false],
      ["h-conflict", refusal(409, "VALUE_REVISION_CONFLICT", "h-conflict"), false],
      ["h-closed", refusal(503, "VALUE_OWNER_UNAVAILABLE", "h-closed"), false],
      ["h-corrupt", refusal(500, "VALUE_CORRUPT", "h-corrupt"), false],
      ["h-uncertain", refusal(409, "VALUE_STORAGE_UNCONFIRMED", "h-uncertain"), false],
      ["h-session", refusal(409, "VALUE_SESSION_CHANGED", "h-session"), false],
      ["h-authority", refusal(403, "VALUE_ACCESS_REFUSED", "h-authority"), false],
    ];
    for (const [handle, reply, withdrawn] of cases) {
      const reading = results.read(handle);
      await flush();
      replies.get(handle)!(reply);
      const error = await reading.catch(caught => caught);
      expect(error instanceof ResultWithdrawnError, handle).toBe(withdrawn);
      expect(error instanceof ResultReadError, handle).toBe(!withdrawn);
      expect(results.isWithdrawn(handle), handle).toBe(withdrawn);
    }
    expect(refused.map(([handle]) => handle)).toEqual(["h-410"]);
  });

  it("keeps a withdrawn pending read refused after its tombstone is evicted by 4096 later withdrawals", async () => {
    const { results, fetch, replies } = reader("g-fence");
    const reading = results.read("h-oldest");
    await flush();
    results.withdraw("h-oldest");
    for (let at = 0; at < 4100; at++) results.withdraw(`h-filler-${at}`);
    // The bounded tombstone no longer remembers the handle ...
    expect(results.isWithdrawn("h-oldest")).toBe(false);
    // ... but this request's own fence still refuses its late successful body.
    replies.get("h-oldest")!(body(value));
    await expect(reading).rejects.toBeInstanceOf(ResultWithdrawnError);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(results.withdrawnHandles()).toHaveLength(4096);
  });

  it("does not let a withdrawn request's coalesced promise serve a later read", async () => {
    const { results, fetch, replies } = reader("g-coalesce");
    const first = results.read("h-shared");
    await flush();
    results.withdraw("h-shared");
    for (let at = 0; at < 4100; at++) results.withdraw(`h-evict-${at}`);
    // After eviction a new explicit read is a new request, with its own fence.
    const second = results.read("h-shared");
    expect(second).not.toBe(first);
    await flush();
    replies.get("h-shared")!(body({ ...value, data: "synthetic fresh row" }));
    await expect(second).resolves.toMatchObject({ data: "synthetic fresh row" });
    expect(fetch).toHaveBeenCalledTimes(2);
  });
});
