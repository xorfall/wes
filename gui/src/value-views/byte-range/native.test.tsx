import { act, create, type ReactTestRenderer, type ReactTestRendererJSON } from "react-test-renderer";
import { afterEach, expect, it, vi } from "vitest";
import { DecodedBytes, prepareSync, type Prepared } from "../../presentation/prepare";
import { present } from "../../presentation/present";
import { Registry } from "../../presentation/registry";
import type { Context } from "../../presentation/types";
import type { StoredValue, TypeShape } from "../../protocol";
import { byteRangeViewModule, canonicalBase64, decodeSpan, MAX_RANGE_BYTES } from "./module";

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });
const context: Context = { mode: "window", columns: 100, lines: 4000, density: "normal", locale: "en-GB", timeZone: "UTC" };
const p = (name: string): TypeShape => ({ kind: "primitive", name });
const optionalText: TypeShape = { kind: "option", element: p("TEXT") };
const rangeType = (name = "ByteRange"): TypeShape => ({ kind: "record", name, fields: Object.entries({
  source: p("TEXT"), run: optionalText, revision: p("TEXT"), handle: optionalText, digest: optionalText, selection: { kind: "list", element: p("TEXT") } as TypeShape,
  from: p("TEXT"), next: p("TEXT"), extent: p("TEXT"), text: p("BOOL"), extentExhausted: p("BOOL"), data: p("BYTES"),
}).map(([n, type]) => ({ name: n, type })) });
const encode = (bytes: Uint8Array) => { let binary = ""; for (const byte of bytes) binary += String.fromCharCode(byte); return btoa(binary); };
const range = (bytes: Uint8Array, from = "0", extent?: string, data: Record<string, unknown> = {}, name?: string): StoredValue => {
  const next = (BigInt(from) + BigInt(bytes.length)).toString(), end = extent ?? next;
  return { type: rangeType(name), data: { source: "evidence", run: { kind: "some", value: "run-7" }, revision: "3", handle: { kind: "none" }, digest: { kind: "none" },
    selection: ["raw"], from, next, extent: end, text: false, extentExhausted: next === end, data: encode(bytes), ...data } } as StoredValue;
};
const sample = Uint8Array.from([0x48, 0x69, 0x00, 0xff, 0x41]);
const counting = (length: number) => Uint8Array.from({ length }, (_, at) => at % 256);

const shown = (prepared: Prepared, overrides: Partial<Context> = {}) => {
  const node = present({ prepared, facts: { state: "ready" }, context: { ...context, ...overrides }, registry: Registry.core() }).root;
  if (node.kind !== "view" || node.view !== "byte-range") throw new Error("Expected byte range presentation");
  return node;
};
const draw = (node: ReturnType<typeof shown>) => { const Component = byteRangeViewModule.Component; return <Component model={node.model} children={node.children} renderChild={() => null} />; };
const text = (tree: ReactTestRenderer) => {
  const out: string[] = [];
  const visit = (node: ReactTestRendererJSON | string | null | (ReactTestRendererJSON | string)[]) => {
    if (node === null) return; if (typeof node === "string") { out.push(node); return; }
    if (Array.isArray(node)) { node.forEach(visit); return; } node.children?.forEach(visit);
  };
  visit(tree.toJSON()); return out.join("\n");
};
const cells = (tree: ReactTestRenderer) => tree.root.findAllByProps({ role: "gridcell" }).filter(it => it.type === "button");
const ascii = (tree: ReactTestRenderer) => tree.root.findByProps({ role: "grid" }).findAll(it => it.type === "span" && it.props.className === "byte-range-ascii")
  .map(row => row.children.map(cell => typeof cell === "string" ? cell : cell.children.join("")).join("")).join("");
const button = (tree: ReactTestRenderer, label: string) => {
  const found = tree.root.findAllByType("button").find(it => it.children.join("") === label);
  if (!found) throw new Error(`Missing button ${label}`);
  return found;
};

it("matches the native byte-range structure and refuses forged or inconsistent ranges", () => {
  const { matches } = byteRangeViewModule, valid = range(sample);
  expect(matches(valid.type, valid.data)).toBe(true);
  expect(matches(rangeType("Renamed"), valid.data)).toBe(true);
  expect(matches({ kind: "record", name: "ByteRange", fields: [{ name: "data", type: p("BYTES") }] }, valid.data)).toBe(false);
  const forged: Record<string, unknown>[] = [
    { next: "4" }, { extent: "4" }, { extentExhausted: false }, { from: "01" }, { from: "18446744073709551616" },
    { data: "not base64!" }, { data: 17 }, { run: "run-7" }, { digest: { kind: "some", value: 3 } }, { selection: "raw" },
    { text: "false" }, { source: null },
  ];
  for (const change of forged) expect(matches(valid.type, (range(sample, "0", undefined, change)).data)).toBe(false);
  const oversized = range(new Uint8Array(MAX_RANGE_BYTES + 3));
  expect(matches(oversized.type, oversized.data)).toBe(false);
  const fallback = present({ prepared: prepareSync(range(sample, "0", undefined, { next: "9" })), facts: { state: "ready" }, context, registry: Registry.core() }).root;
  expect(fallback.kind === "view" && fallback.view === "byte-range").toBe(false);
});

it("accepts only canonical Base64, padding bits included, without decoding to decide", () => {
  const atob = vi.spyOn(globalThis, "atob");
  const of = (stored: string, size: number) => range(new Uint8Array(size), "0", undefined, { data: stored });
  const accepted = (value: StoredValue) => byteRangeViewModule.matches(value.type, value.data);
  for (const [stored, size] of [["", 0], ["Zg==", 1], ["Zm8=", 2], ["Zm9v", 3], ["AA==", 1], ["//8=", 2], ["Zm9vYg==", 4]] as const) {
    expect(canonicalBase64(stored)).toBe(true); expect(accepted(of(stored, size))).toBe(true);
  }
  for (const [stored, size] of [["Zh==", 1], ["Zm9=", 2], ["Zg=", 1], ["Zg", 1], ["Z===", 1], ["Zg==Zg==", 2], ["Zm 9", 2], ["Zm-_", 3], ["=Zm9", 2], ["Zm9v\n", 3], ["//9=", 2], ["AB==", 1]] as const) {
    expect(canonicalBase64(stored)).toBe(false); expect(accepted(of(stored, size))).toBe(false);
  }
  expect(atob).not.toHaveBeenCalled();
});

it("states the end of the source as a fact rather than suggesting a command", async () => {
  const node = shown(prepareSync(range(sample, "0", "5")));
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  expect(text(tree)).toContain("This read only · reaches the source end · read another range with :read bytes");
  expect(text(tree)).not.toMatch(/from:|limit:/);
  act(() => tree.unmount());
});

it("prepares without decoding and keeps a one-line preview free of payload decoding", async () => {
  const atob = vi.spyOn(globalThis, "atob");
  const prepared = prepareSync(range(counting(4096), "0", "1048576"));
  const data = (prepared.data as { data: DecodedBytes }).data;
  expect(data).toBeInstanceOf(DecodedBytes);
  expect(data.size).toBe(4096);
  expect(data.text).toBeUndefined();
  expect(atob).not.toHaveBeenCalled();
  const one = shown(prepared, { mode: "preview", lines: 1 });
  expect(one.ownLines).toBeLessThanOrEqual(1);
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(one)); });
  expect(text(tree)).toContain("Bytes 0–4095");
  expect(text(tree)).toContain("4,096 B of 1,048,576 B · Bytes field");
  expect(atob).not.toHaveBeenCalled();
  expect(cells(tree)).toHaveLength(0);
  act(() => tree.unmount());

  const two = shown(prepared, { mode: "preview", lines: 2 });
  expect(two.ownLines).toBeLessThanOrEqual(2);
  await act(async () => { tree = create(draw(two)); });
  expect(text(tree)).toContain("00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f …");
  expect(atob.mock.calls.every(([chunk]) => String(chunk).length <= 24)).toBe(true);
  act(() => tree.unmount());
});

it("draws actual bytes as hex and printable ASCII, without treating a Text source as UTF-8", async () => {
  const node = shown(prepareSync(range(sample, "0", "5", { text: true })));
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  expect(cells(tree).map(it => it.children.join(""))).toEqual(["48", "69", "00", "ff", "41"]);
  expect(ascii(tree)).toBe("Hi··A");
  expect(text(tree)).toContain("5 B of 5 B · Text field · reaches end");
  expect(text(tree)).toContain("Select a byte to see its original offset");
  act(() => button(tree, "ff").props.onClick());
  expect(text(tree)).toContain("Offset 3 · 0xff · 255 · not printable ASCII");
  act(() => button(tree, "41").props.onClick());
  expect(text(tree)).toContain(`Offset 4 · 0x41 · 65 · ASCII "A"`);
  expect(decodeSpan(encode(sample), 1, 4)).toEqual(Uint8Array.from([0x69, 0x00, 0xff]));
  act(() => tree.unmount());
});

it("keeps original u64 coordinates exact and explains how to read beyond the supplied range", async () => {
  const from = "18446744073709551000";
  const node = shown(prepareSync(range(counting(16), from, "18446744073709551615")));
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  const all = text(tree);
  expect(all).toContain("Bytes 18446744073709551000–18446744073709551015");
  expect(all).toContain("This read only · next offset 18446744073709551016 · read another range with :read bytes");
  // No executable snippet: the view knows no source variable and must not invent one or a limit.
  expect(all).not.toMatch(/from:|limit:|\$\w/);
  expect(all).toContain("evidence · run run-7 · revision 3");
  act(() => cells(tree)[5]!.props.onClick());
  expect(text(tree)).toContain("Offset 18446744073709551005 · 0x05 · 5");
  expect(tree.root.findAllByType("button").map(it => it.children.join(""))).not.toContain("Read more");
  act(() => tree.unmount());
});

it("draws and decodes no more bytes than its line budget, paging only within supplied bytes", async () => {
  const atob = vi.spyOn(globalThis, "atob");
  const node = shown(prepareSync(range(counting(4096), "0", "1048576")), { lines: 12 });
  expect(node.ownLines).toBe(12);
  const rows = (node.model as { rows: number }).rows;
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  expect(cells(tree).length).toBe(rows * 16);
  expect(atob.mock.calls.every(([chunk]) => String(chunk).length <= Math.ceil(rows * 16 / 3) * 4)).toBe(true);
  expect(text(tree)).toContain(`Rows 0–${rows * 16 - 1} of this read`);
  expect(button(tree, "Earlier bytes").props.disabled).toBe(true);
  act(() => button(tree, "Later bytes").props.onClick());
  expect(text(tree)).toContain(`Rows ${rows * 16}–${rows * 32 - 1} of this read`);
  expect(cells(tree)[0]!.children.join("")).toBe((rows * 16 % 256).toString(16).padStart(2, "0"));
  act(() => tree.unmount());

  const empty = shown(prepareSync(range(new Uint8Array(0), "1048576", "1048576")), { lines: 6 });
  expect(empty.ownLines).toBeLessThanOrEqual(6);
  await act(async () => { tree = create(draw(empty)); });
  expect(text(tree)).toContain("No bytes at offset 1048576");
  expect(tree.root.findAllByProps({ role: "grid" })).toHaveLength(0);
  expect(tree.root.findAllByProps({ role: "status" })).toHaveLength(0);
  act(() => tree.unmount());
});

it("moves the selection by keyboard inside the supplied range without leaking keys", async () => {
  const node = shown(prepareSync(range(counting(40))));
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  const keys = (key: string) => { const event = { key, preventDefault: vi.fn(), stopPropagation: vi.fn() }; act(() => tree.root.findByProps({ role: "grid" }).props.onKeyDown(event)); return event; };
  expect(keys("r").preventDefault).not.toHaveBeenCalled();
  expect(keys("ArrowRight").stopPropagation).toHaveBeenCalled();
  expect(text(tree)).toContain("Offset 1 · 0x01");
  keys("ArrowDown");
  expect(text(tree)).toContain("Offset 17 · 0x11");
  keys("End");
  expect(text(tree)).toContain("Offset 39 · 0x27");
  keys("ArrowRight");
  expect(text(tree)).toContain("Offset 39 · 0x27");
  act(() => tree.unmount());
});

it("resets the selected byte when the read is refreshed or withdrawn, not when the pane resizes", async () => {
  const prepared = prepareSync(range(sample));
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(shown(prepared))); });
  act(() => button(tree, "ff").props.onClick());
  await act(async () => tree.update(draw(shown(prepared, { columns: 60 }))));
  expect(text(tree)).toContain("Offset 3 · 0xff");
  await act(async () => tree.update(draw(shown(prepareSync(range(sample))))));
  expect(text(tree)).not.toContain("Offset 3 · 0xff");
  expect(text(tree)).toContain("Select a byte to see its original offset");
  act(() => tree.unmount());
});

it("has no iframe, links, copy, export or network side effects", async () => {
  const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
  const url = vi.spyOn(URL, "createObjectURL");
  const node = shown(prepareSync(range(counting(300), "0", "1000")), { lines: 14 });
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  act(() => cells(tree)[2]!.props.onClick());
  act(() => button(tree, "Later bytes").props.onClick());
  expect(tree.root.findAllByType("iframe")).toHaveLength(0);
  expect(tree.root.findAllByType("a")).toHaveLength(0);
  expect(tree.root.findAllByType("button").map(it => it.children.join("")).every(label => /^[0-9a-f]{2}$/.test(label) || /bytes$/.test(label))).toBe(true);
  expect(text(tree)).not.toMatch(/Copy|Download|Export/);
  expect(fetch).not.toHaveBeenCalled();
  expect(url).not.toHaveBeenCalled();
  act(() => tree.unmount());
});
