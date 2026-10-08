import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "../../engine";
import { DatasetReadError, type DatasetPosition, type DatasetRead, type DatasetRecording } from "../../dataset-read";
import { decodeDatasetReference, type DatasetReference } from "../../presentation/dataset";
import type { StoredValue, TypeShape } from "../../protocol";
import { ComposeContext, type Composer } from "../dataset-management";
import { ValueBlock } from "./ValueBlock";
import { FOLLOW_INTERVAL_MS } from "./DatasetBrowser";

/*
 * Synthetic snapshots only: invented store, dataset, manifest and run identities, digests and byte
 * counts that name no real store, recording or analysis. Each generation gets its own digest so the
 * prepared command can be checked for exactly the shown one.
 */

vi.mock("../Cell", async (original) => ({ ...(await original<typeof import("../Cell")>()), useBlockReport: () => () => undefined }));
beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); });

const CAPTURE = "capture shown prefix…";
const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const ROW: TypeShape = { kind: "record", name: "SyntheticEvent", fields: [{ name: "label", type: TEXT }] };
const DATASET: TypeShape = { kind: "dataset", element: ROW };
const digest = (generation: string) => `sha256:${generation.padStart(2, "0").repeat(32)}`;
const at = (generation: string, records: string): DatasetReference => decodeDatasetReference({
  store: "00000000-0000-4000-8000-0000000000a1", dataset: "00000000-0000-4000-8000-0000000000b2",
  generation, manifest: `00000000-0000-4000-8000-0000000000${generation.padStart(2, "0")}`, manifestDigest: digest(generation), manifestBytes: "512",
  schemaDigest: `sha256:${"fedcba9876543210".repeat(4)}`, records, authorizationGeneration: "1",
})!;
const ORIGINAL = at("3", "20");
const value: StoredValue = { type: DATASET, provenance: {}, data: { kind: "dataset", reference: { ...ORIGINAL } } };
const command = (generation: string, name = "shownPrefix") =>
  `:dataset snapshot $events basis:"${digest("3")}" generation:"${generation}" digest:"${digest(generation)}" > ${name}`;

const recording = (ref: DatasetReference, termination: DatasetRecording["termination"]): DatasetRecording => ({
  run: "00000000-0000-4000-8000-0000000000e1", epoch: ref.dataset, first: "1", acceptedThrough: ref.records,
  committedThrough: ref.records, pending: "0", rejected: "0", termination,
});
function head(ref: DatasetReference, lifecycle: DatasetRead["lifecycle"] = "open", termination: DatasetRecording["termination"] = null): DatasetRead {
  return { reference: ref, lifecycle, protected: false, persistence: "Durable", segmentBytes: "8192", recording: recording(ref, termination) };
}
function page(ref: DatasetReference, first: string, limit: number, lifecycle: DatasetRead["lifecycle"] = "open"): DatasetRead {
  const start = BigInt(first), total = BigInt(ref.records), end = total < start + BigInt(limit) ? total : start + BigInt(limit);
  const rows = [];
  for (let n = start; n < end; n++) rows.push({ ordinal: n.toString(), sourceStart: n.toString(), sourceEnd: n.toString(), value: { type: ROW, provenance: {}, data: { label: `event ${n}` } } });
  const exhausted = end === total;
  return { reference: ref, lifecycle, protected: false, persistence: "Durable", segmentBytes: "8192",
    page: { first, next: end.toString(), rows, extentExhausted: exhausted, limitedBy: null, cursor: exhausted ? null : `c${end}` } };
}

type Pending<T> = { readonly args: T; resolve: (read: DatasetRead) => void; reject: (error: unknown) => void; readonly signal: AbortSignal };
function fakeEngine() {
  const frozen: Pending<{ select: string; position: DatasetPosition; limit: number }>[] = [];
  const heads: Pending<{ shown: DatasetReference }>[] = [];
  const extents: Pending<{ extent: DatasetReference; position: DatasetPosition; limit: number }>[] = [];
  const promise = <T,>(list: Pending<T>[], args: T, signal: AbortSignal) => new Promise<DatasetRead>((resolve, reject) => list.push({ args, resolve, reject, signal }));
  const engine = {
    readDataset: vi.fn((_h: string, _g: string, _r: DatasetReference, select: string, position: DatasetPosition, limit: number, signal: AbortSignal) => promise(frozen, { select, position, limit }, signal)),
    readDatasetHead: vi.fn((_h: string, _g: string, _o: DatasetReference, shown: DatasetReference, _s: string, signal: AbortSignal) => promise(heads, { shown }, signal)),
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
/** The note beside the capture action: the line that follows it in the same slot. */
const note = (tree: ReactTestRenderer) => textOf(button(tree, CAPTURE)!.parent!);
const composer = (taken: readonly string[] = []) => { const compose = vi.fn(); return { compose, value: { compose, taken: new Set(taken) } satisfies Composer }; };

type Mode = "preview" | "expanded" | "window";
interface Options { readonly mode?: Mode; readonly name?: string | null; readonly composer?: Composer | null; readonly collapsed?: boolean; readonly value?: StoredValue }
let mounts = 0;
function mount(engine: Engine, options: Options = {}) {
  const handle = `stored-capture-${++mounts}`;
  const inner = <ValueBlock engine={engine} value={options.value ?? value} stored={{ handle, generation: "g1" }} cacheKey={`g1:${handle}`} mode={options.mode ?? "window"} inCell={false}
    {...(options.name === null ? {} : { name: options.name ?? "events" })} {...(options.collapsed ? { collapsed: true } : {})} />;
  let tree!: ReactTestRenderer;
  act(() => { tree = create(options.composer === null ? inner : <ComposeContext.Provider value={options.composer ?? composer().value}>{inner}</ComposeContext.Provider>); });
  return tree;
}
/** Mounted, Reading the saved snapshot's first page, which ends as `lifecycle` says. */
async function reading(options: Options = {}, lifecycle: DatasetRead["lifecycle"] = "open") {
  const fake = fakeEngine();
  const tree = mount(fake.engine, options);
  fake.frozen[0]!.resolve(page(ORIGINAL, "0", fake.frozen[0]!.args.limit, lifecycle)); await flush();
  return { ...fake, tree };
}
/** Following, with the head grown to `grown` and its tail page shown. */
async function following(options: Options = {}, grown = at("4", "130"), lifecycle: DatasetRead["lifecycle"] = "open", termination: DatasetRecording["termination"] = null) {
  const fake = await reading(options);
  press(fake.tree, "follow");
  fake.heads[0]!.resolve(head(grown, lifecycle, termination)); await flush();
  const tail = fake.extents[0]!;
  tail.resolve(page(grown, (tail.args.position as { readonly from: string }).from, tail.args.limit, lifecycle)); await flush();
  return { ...fake, grown };
}

describe("capturing the shown prefix", () => {
  it("prepares the exact basis and shown generation under a fresh name, stops following and submits nothing", async () => {
    const { compose, value: prompt } = composer(["shownPrefix"]);
    const fake = await following({ composer: prompt });
    expect(text(fake.tree)).toContain("following · newest committed records");
    await tick(FOLLOW_INTERVAL_MS);
    const poll = fake.heads[1]!;
    const reads = { heads: fake.heads.length, extents: fake.extents.length, frozen: fake.frozen.length };
    press(fake.tree, CAPTURE);
    expect(compose.mock.calls).toEqual([[command("4", "shownPrefix2")]]);
    // Follow stopped with the shown generation; the poll in flight is abandoned and nothing more is read.
    expect(poll.signal.aborted).toBe(true);
    expect(button(fake.tree, "follow")).toBeDefined();
    poll.resolve(head(at("5", "400"))); await flush();
    await tick(10 * FOLLOW_INTERVAL_MS);
    expect({ heads: fake.heads.length, extents: fake.extents.length, frozen: fake.frozen.length }).toEqual(reads);
    const shown = text(fake.tree);
    expect(shown).toContain("showing newer committed records: generation 4, 130 records");
    expect(shown).not.toContain("generation 5");
    act(() => fake.tree.unmount());
  });

  it("names the last page actually drawn, never a newer head whose page is still in flight", async () => {
    const { compose, value: prompt } = composer();
    const fake = await following({ composer: prompt });
    await tick(FOLLOW_INTERVAL_MS);
    fake.heads[1]!.resolve(head(at("5", "200"))); await flush();
    const late = fake.extents[1]!;
    // The newer head is known but not drawn: the action still names generation 4.
    expect(note(fake.tree)).toContain("records 0–129 of generation 4");
    press(fake.tree, CAPTURE);
    expect(late.signal.aborted).toBe(true);
    late.resolve(page(at("5", "200"), "100", 100)); await flush();
    expect(compose.mock.calls).toEqual([[command("4")]]);
    expect(text(fake.tree)).not.toContain("event 199");
    act(() => fake.tree.unmount());
  });

  it("says the range, generation and segment data, and that Keep is separate, without a cost or a kept claim", async () => {
    const fake = await following();
    const said = note(fake.tree);
    expect(said).toBe(`${CAPTURE}records 0–129 of generation 4 · segment data 8 192 bytes · captures it as a new result; Keep is a separate action · nothing runs until you submit it`);
    // The descriptor's own encoding size is not the dataset's data.
    expect(said).not.toContain("512");
    expect(said).not.toMatch(/not kept|\bkept\b|cost|shared|exclusive|total/i);
    // The saved snapshot's Keep/Pin line is unchanged and still about generation 3.
    expect(text(fake.tree)).toContain("Keep or Pin retains exactly records 0–19 of generation 3; records committed later are not included");
    act(() => fake.tree.unmount());
  });

  it("leaves the retention preview on the saved snapshot while a newer prefix is shown: that prefix is captured first", async () => {
    const { compose, value: prompt } = composer();
    const fake = await following({ composer: prompt });
    const reads = { heads: fake.heads.length, extents: fake.extents.length, frozen: fake.frozen.length };
    press(fake.tree, "preview retention…");
    expect(compose.mock.calls).toEqual([[`:dataset retention $events basis:"${digest("3")}" > retention`]]);
    expect(compose.mock.calls[0]![0]).not.toContain(digest("4"));
    expect({ heads: fake.heads.length, extents: fake.extents.length, frozen: fake.frozen.length }).toEqual(reads);
    // The capture of the shown prefix is still its own action.
    press(fake.tree, CAPTURE);
    expect(compose.mock.calls[1]).toEqual([command("4")]);
    act(() => fake.tree.unmount());
  });

  it("offers a same-count terminal generation: its sealing and coverage are worth capturing", async () => {
    const { compose, value: prompt } = composer();
    const fake = await following({ composer: prompt }, at("4", "20"), "sealed", "natural");
    expect(text(fake.tree)).toContain("reading · final page · sealed · the source ended");
    expect(note(fake.tree)).toContain("records 0–19 of generation 4");
    press(fake.tree, CAPTURE);
    expect(compose.mock.calls).toEqual([[command("4")]]);
    act(() => fake.tree.unmount());
  });
});

describe("when no capture is offered", () => {
  it("offers none for the saved snapshot itself, whose Keep/Pin line already applies", async () => {
    const { compose, value: prompt } = composer();
    const fake = await reading({ composer: prompt });
    expect(button(fake.tree, CAPTURE)).toBeUndefined();
    expect(text(fake.tree)).toContain("Keep or Pin retains exactly records 0–19 of generation 3");
    expect(compose).not.toHaveBeenCalled();
    act(() => fake.tree.unmount());
  });

  it.each([
    ["no session prompt", { composer: null }, "management commands are prepared in the session"],
    ["an unnamed result", { name: null }, "management commands refer to results by name; this result has none"],
  ] as const)("offers none with %s and keeps the existing explanation", async (_name, options, explanation) => {
    const fake = await following(options);
    expect(text(fake.tree)).toContain("showing newer committed records: generation 4");
    expect(button(fake.tree, CAPTURE)).toBeUndefined();
    expect(text(fake.tree)).toContain(explanation);
    act(() => fake.tree.unmount());
  });

  it("offers none when every candidate result name is taken", async () => {
    const taken = ["shownPrefix", ...Array.from({ length: 998 }, (_, at) => `shownPrefix${at + 2}`)];
    const { compose, value: prompt } = composer(taken);
    const fake = await following({ composer: prompt });
    expect(button(fake.tree, CAPTURE)).toBeUndefined();
    expect(compose).not.toHaveBeenCalled();
    act(() => fake.tree.unmount());
  });

  it("offers none for a Dataset whose path a command cannot spell", async () => {
    const fake = fakeEngine();
    const list: StoredValue = { type: { kind: "list", element: DATASET }, provenance: {}, data: [{ kind: "dataset", reference: { ...ORIGINAL } }] };
    const tree = mount(fake.engine, { value: list });
    act(() => tree.root.findAll(node => node.type === "button" && node.props["aria-expanded"] === false && String(node.props.className).includes("value-toggle"))[0]!.props.onClick());
    expect(fake.frozen[0]!.args.select).toBe("/0");
    fake.frozen[0]!.resolve(page(ORIGINAL, "0", 100)); await flush();
    press(tree, "follow");
    fake.heads[0]!.resolve(head(at("4", "130"))); await flush();
    fake.extents[0]!.resolve(page(at("4", "130"), "30", 100)); await flush();
    expect(text(tree)).toContain("showing newer committed records: generation 4");
    expect(button(tree, CAPTURE)).toBeUndefined();
    expect(text(tree)).toContain("management commands cannot name this Dataset's path");
    act(() => tree.unmount());
  });

  it("neither reads nor offers a capture while collapsed", async () => {
    const fake = fakeEngine();
    const tree = mount(fake.engine, { mode: "preview", collapsed: true });
    await tick(5 * FOLLOW_INTERVAL_MS);
    expect(fake.frozen).toHaveLength(0);
    expect(fake.heads).toHaveLength(0);
    expect(button(tree, CAPTURE)).toBeUndefined();
    act(() => tree.unmount());
  });
});

describe("pending reads and failures", () => {
  it("keeps the action steady while a page of the same extent is read, and after it lands", async () => {
    const fake = await following();
    press(fake.tree, "‹ previous");
    expect(text(fake.tree)).toContain("reading · previous page shown");
    expect(note(fake.tree)).toContain("records 0–129 of generation 4");
    fake.extents[1]!.resolve(page(fake.grown, "0", 100)); await flush();
    expect(note(fake.tree)).toContain("records 0–129 of generation 4");
    // A tier change rereads the same extent: still no gap.
    act(() => fake.tree.update(<ComposeContext.Provider value={composer().value}>
      <ValueBlock engine={fake.engine} value={value} stored={{ handle: `stored-capture-${mounts}`, generation: "g1" }} cacheKey={`g1:stored-capture-${mounts}`} mode="expanded" inCell={false} name="events" />
    </ComposeContext.Provider>));
    expect(fake.extents.at(-1)!.args).toMatchObject({ extent: fake.grown, limit: 50 });
    expect(button(fake.tree, CAPTURE)).toBeDefined();
    act(() => fake.tree.unmount());
  });

  it.each([
    ["a busy reader", new DatasetReadError("busy", 503, "DATASET_READ_BUSY", "synthetic busy", true), "read again"],
    ["a failed read", new DatasetReadError("failed", 500, "DATASET_READ_FAILED", "synthetic failure", false), "read again"],
  ])("shows only the recovery action after %s of the head", async (_name, error, recovery) => {
    const fake = await following();
    await tick(FOLLOW_INTERVAL_MS);
    fake.heads[1]!.reject(error); await flush();
    expect(button(fake.tree, recovery)).toBeDefined();
    expect(button(fake.tree, CAPTURE)).toBeUndefined();
    act(() => fake.tree.unmount());
  });

  it("shows only the smaller-page action after a page limit", async () => {
    const fake = await following();
    press(fake.tree, "‹ previous");
    fake.extents[1]!.reject(new DatasetReadError("limit", 413, "DATASET_READ_LIMIT", "synthetic limit", false)); await flush();
    expect(button(fake.tree, "read 50 rows per page")).toBeDefined();
    expect(button(fake.tree, CAPTURE)).toBeUndefined();
    act(() => fake.tree.unmount());
  });

  it("offers nothing once the stored result is missing", async () => {
    const fake = await following();
    press(fake.tree, "‹ previous");
    fake.extents[1]!.reject(new DatasetReadError("missing", 404, "DATASET_MISSING", "synthetic missing", false)); await flush();
    expect(text(fake.tree)).toContain("stored result unavailable");
    expect(button(fake.tree, CAPTURE)).toBeUndefined();
    act(() => fake.tree.unmount());
  });

  it("still names the retained page after a continuity stop, leaving the checks to the engine", async () => {
    const { compose, value: prompt } = composer();
    const fake = await following({ composer: prompt });
    await tick(FOLLOW_INTERVAL_MS);
    fake.heads[1]!.reject(new DatasetReadError("continuity", 409, "DATASET_CONTINUITY_CHANGED", "synthetic continuity", false)); await flush();
    expect(text(fake.tree)).toContain("previous committed page retained");
    expect(button(fake.tree, "read again")).toBeUndefined();
    press(fake.tree, CAPTURE);
    expect(compose.mock.calls).toEqual([[command("4")]]);
    act(() => fake.tree.unmount());
  });

  it("drops the action and every shown identity on withdrawal, and never touches the prompt", async () => {
    const { compose, value: prompt } = composer();
    const fake = await following({ composer: prompt });
    press(fake.tree, CAPTURE);
    press(fake.tree, "‹ previous");
    fake.extents[1]!.reject(new DatasetReadError("withdrawn", 410, "DATASET_WITHDRAWN", "Access withdrawn", false)); await flush();
    const shown = text(fake.tree);
    expect(shown).toContain("Access withdrawn");
    expect(button(fake.tree, CAPTURE)).toBeUndefined();
    for (const gone of ["generation 4", "130 records", "segment data", "sha256", "event 129"]) expect(shown).not.toContain(gone);
    // The prepared command stays the person's text; withdrawal composes nothing over it.
    expect(compose).toHaveBeenCalledTimes(1);
    act(() => fake.tree.unmount());
  });
});

describe("the action line", () => {
  const slots = (tree: ReactTestRenderer) => tree.root.findAll(node => node.type === "div" && node.props.className === "dataset-recovery");
  const actions = (slot: ReactTestInstance) => slot.findAll(node => node.type === "button");

  it("is the same single slot whether empty, offering read again or offering a capture", async () => {
    const empty = await reading();
    expect(slots(empty.tree)).toHaveLength(1);
    expect(actions(slots(empty.tree)[0]!)).toHaveLength(0);
    act(() => empty.tree.unmount());
    const capture = await following();
    expect(actions(slots(capture.tree)[0]!).map(textOf)).toEqual([CAPTURE]);
    await tick(FOLLOW_INTERVAL_MS);
    capture.heads[1]!.reject(new DatasetReadError("busy", 503, "DATASET_READ_BUSY", "synthetic busy", true)); await flush();
    expect(slots(capture.tree)).toHaveLength(1);
    expect(actions(slots(capture.tree)[0]!).map(textOf)).toEqual(["read again"]);
    act(() => capture.tree.unmount());
  });

  // A stylesheet contract only: the test renderer lays nothing out, so this proves no pixel geometry.
  it("reserves one cell action's height in the stylesheet, empty or not", () => {
    const strip = (css: string) => css.replace(/\/\*[\s\S]*?\*\//g, "");
    const rule = (css: string, selector: string) => strip(css).split("}")
      .filter(block => block.split("{").at(-2)?.split(",").map(s => s.trim()).includes(`.wes-terminal ${selector}`)).join(" ");
    const dataset = readFileSync(new URL("./dataset.css", import.meta.url), "utf8");
    expect(rule(dataset, ".dataset-browser")).toContain("--dataset-action:calc(var(--dataset-line) + 2px)");
    const slot = rule(dataset, ".dataset-recovery");
    expect(slot).toMatch(/(^|[\s;{])height:var\(--dataset-action\)/);
    expect(slot).not.toContain("min-height");
    expect(slot).toContain("white-space:nowrap");
    expect(slot).not.toMatch(/flex-wrap:\s*wrap/);
    const action = rule(dataset, ".dataset-recovery > .cell-action");
    for (const declaration of ["box-sizing:border-box", "height:var(--dataset-action)", "line-height:var(--dataset-line)"]) expect(action).toContain(declaration);
    // The 2px is the cell action's own block padding with no border; it is the rule cell.css applies last.
    const cell = rule(readFileSync(new URL("../cell.css", import.meta.url), "utf8"), ".cell-action");
    expect(cell).toContain("border:0");
    expect(cell).toContain("padding:1px 4px");
    const surface = strip(readFileSync(new URL("../surface.css", import.meta.url), "utf8"));
    expect(surface.indexOf(`"./roles.css"`)).toBeGreaterThanOrEqual(0);
    expect(surface.indexOf(`"./roles.css"`)).toBeLessThan(surface.indexOf(`"./cell.css"`));
  });
});

describe("every tier", () => {
  it.each([["preview"], ["expanded"], ["window"]] as const)("offers the same capture in %s", async (mode) => {
    const { compose, value: prompt } = composer();
    const fake = await following({ mode, composer: prompt });
    press(fake.tree, CAPTURE);
    expect(compose.mock.calls).toEqual([[command("4")]]);
    act(() => fake.tree.unmount());
  });
});

describe("lifecycle of the read snapshot", () => {
  const warned = (tree: ReactTestRenderer) => tree.root.findAll(node => node.type === "span" && node.props.className === "mono-warn").map(textOf).join("|");

  it("labels an earlier prefix with later generations as committed history, not as an interrupted writer", async () => {
    const fake = await reading({}, "prefix");
    const shown = text(fake.tree);
    expect(shown).toContain("generation 3 · prefix · later generations committed");
    expect(shown).toContain("end of committed snapshot · later generations committed");
    expect(shown).not.toContain("interrupted");
    expect(warned(fake.tree)).not.toContain("prefix");
    act(() => fake.tree.unmount());
  });

  /*
   * A head read inspects the head, then reads that manifest in a second request. An append can commit
   * in between, so the reply truthfully names an older open generation, read as `prefix`.
   */
  it("keeps following past a head that was already a prefix when it was read, and ends on a terminal head", async () => {
    const fake = await following();
    const poll = async (request: number, read: DatasetRead) => {
      await tick(FOLLOW_INTERVAL_MS);
      fake.heads[request]!.resolve(read); await flush();
      const tail = fake.extents.at(-1)!;
      expect(tail.args.extent).toEqual(read.reference);
      tail.resolve(page(read.reference, (tail.args.position as { readonly from: string }).from, tail.args.limit, read.lifecycle)); await flush();
    };
    await poll(1, head(at("5", "160"), "prefix", null));
    expect(button(fake.tree, "reading")).toBeDefined();
    expect(text(fake.tree)).toContain("following · newest committed records");
    expect(text(fake.tree)).toContain("generation 5 · prefix · later generations committed");
    expect(text(fake.tree)).not.toContain("final page");
    // The later extension that was committed meanwhile is followed into.
    await poll(2, head(at("6", "190"), "open", null));
    expect(button(fake.tree, "reading")).toBeDefined();
    expect(text(fake.tree)).toContain("showing newer committed records: generation 6, 190 records");
    await poll(3, head(at("7", "190"), "sealed", "natural"));
    expect(button(fake.tree, "follow")).toBeDefined();
    expect(text(fake.tree)).toContain("reading · final page · sealed · the source ended");
    const polls = fake.heads.length;
    await tick(5 * FOLLOW_INTERVAL_MS);
    expect(fake.heads).toHaveLength(polls);
    act(() => fake.tree.unmount());
  });

  it("still warns about an interrupted latest head and says an open one is still open", async () => {
    const interrupted = await reading({}, "interrupted");
    expect(text(interrupted.tree)).toContain("end of committed snapshot · interrupted");
    expect(warned(interrupted.tree)).toContain(" · interrupted");
    act(() => interrupted.tree.unmount());
    const open = await reading({}, "open");
    expect(text(open.tree)).toContain("end of committed snapshot · dataset still open");
    expect(text(open.tree)).not.toContain("prefix");
    act(() => open.tree.unmount());
  });
});
