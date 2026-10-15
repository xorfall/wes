import { act, create, type ReactTestRenderer, type ReactTestRendererJSON } from "react-test-renderer";
import { afterEach, expect, it, vi } from "vitest";
import { ExactNumber } from "../../exact-json";
import { prepareSync, type Prepared } from "../../presentation/prepare";
import { present } from "../../presentation/present";
import { Registry } from "../../presentation/registry";
import type { Context } from "../../presentation/types";
import type { StoredValue, TypeShape } from "../../protocol";
import { captureIndexViewModule, MAX_INDEX_ROWS } from "./module";

afterEach(() => { vi.restoreAllMocks(); });
const context: Context = { mode: "window", columns: 120, lines: 4000, density: "normal", locale: "en-GB", timeZone: "UTC" };
const p = (name: string): TypeShape => ({ kind: "primitive", name });
const rec = (name: string, fields: Record<string, TypeShape>): TypeShape => ({ kind: "record", name, fields: Object.entries(fields).map(([n, type]) => ({ name: n, type })) });
const iface = rec("CaptureInterface", { id: p("INT"), section: p("INT"), linkType: p("INT"), snapLength: p("INT"), timestampScale: p("TEXT"), timestampOffset: p("TEXT") });
const packet = rec("CapturePacket", { number: p("INT"), interface: p("INT"), recordOffset: p("TEXT"), dataOffset: p("TEXT"), capturedLength: p("INT"), originalLength: p("INT"), timestampTicks: { kind: "option", element: p("TEXT") } });
const fields = { format: p("TEXT"), rawBytes: p("TEXT"), sha256: p("TEXT"), packetCount: p("TEXT"), indexComplete: p("BOOL"), skippedBlocks: p("INT"),
  interfaces: { kind: "list", element: iface } as TypeShape, packets: { kind: "list", element: packet } as TypeShape };
const indexType = (name = "CaptureIndex") => rec(name, fields);
const sha = `sha256:${"ab".repeat(32)}`;
const rows = () => [
  { number: 1, interface: 0, recordOffset: "24", dataOffset: "40", capturedLength: 60, originalLength: 60, timestampTicks: { kind: "some", value: "1700000000000001" } },
  { number: 2, interface: 0, recordOffset: "100", dataOffset: "116", capturedLength: 4096, originalLength: 9000, timestampTicks: { kind: "none" } },
  { number: 3, interface: 1, recordOffset: "4212", dataOffset: "4228", capturedLength: 32, originalLength: 32, timestampTicks: { kind: "some", value: "5" } },
];
const capture = (data: Record<string, unknown> = {}, name?: string): StoredValue => ({ type: indexType(name), data: {
  format: "pcapng", rawBytes: "1048576", sha256: sha, packetCount: "70000", indexComplete: false, skippedBlocks: 2,
  interfaces: [{ id: 0, section: 0, linkType: 1, snapLength: 65535, timestampScale: "10^6", timestampOffset: "0" },
    { id: 1, section: 1, linkType: 101, snapLength: 262144, timestampScale: "2^30", timestampOffset: "-3600" }],
  packets: rows(), ...data } } as StoredValue);

const shown = (prepared: Prepared, overrides: Partial<Context> = {}) => {
  const node = present({ prepared, facts: { state: "ready" }, context: { ...context, ...overrides }, registry: Registry.core() }).root;
  if (node.kind !== "view" || node.view !== "capture-index") throw new Error("Expected capture index presentation");
  return node;
};
const draw = (node: ReturnType<typeof shown>) => { const Component = captureIndexViewModule.Component; return <Component model={node.model} children={node.children} renderChild={() => null} />; };
const text = (tree: ReactTestRenderer) => {
  const out: string[] = [];
  const visit = (node: ReactTestRendererJSON | string | null | (ReactTestRendererJSON | string)[]) => {
    if (node === null) return; if (typeof node === "string") { out.push(node); return; }
    if (Array.isArray(node)) { node.forEach(visit); return; } node.children?.forEach(visit);
  };
  visit(tree.toJSON()); return out.join("\n");
};
const press = (tree: ReactTestRenderer, label: string) => {
  const button = tree.root.findAllByType("button").find(it => it.children.join("") === label);
  if (!button) throw new Error(`Missing button ${label}`);
  act(() => button.props.onClick());
};
const filter = (tree: ReactTestRenderer, query: string) => act(() => tree.root.findByProps({ "aria-label": "Filter indexed packet rows" }).props.onChange({ target: { value: query } }));

it("matches index structure, not a record name, and refuses forged or inconsistent facts", () => {
  const { matches } = captureIndexViewModule;
  expect(matches(capture().type, capture().data)).toBe(true);
  expect(matches(indexType("AnyName"), capture().data)).toBe(true);
  expect(matches(rec("CaptureIndex", { format: p("TEXT") }), capture().data)).toBe(false);
  const forged: Record<string, unknown>[] = [
    { format: "pcapx" }, { sha256: "md5:00" }, { packetCount: "0070000" }, { rawBytes: "18446744073709551616" }, { indexComplete: "yes" },
    { packetCount: "2" },
    { packets: [{ ...rows()[0], dataOffset: "1048570" }] },
    { packets: [{ ...rows()[0], timestampTicks: "5" }] },
    { packets: [rows()[1], rows()[0]] },
    { packets: [{ ...rows()[0], number: 0 }] },
    { packets: [{ ...rows()[0], recordOffset: "41" }] },
    { packets: Array.from({ length: MAX_INDEX_ROWS + 1 }, (_, at) => ({ ...rows()[0], number: at + 1 })) },
    { interfaces: [{ id: 0, section: 0, linkType: 1, snapLength: 65535, timestampScale: "ms", timestampOffset: "0" }] },
  ];
  for (const change of forged) expect(matches(capture(change).type, capture(change).data)).toBe(false);
  expect(matches(capture().type, "not a record")).toBe(false);
  const fallback = present({ prepared: prepareSync(capture({ format: "pcapx" })), facts: { state: "ready" }, context, registry: Registry.core() }).root;
  expect(fallback.kind === "view" && fallback.view === "capture-index").toBe(false);
});

const withInterface = (change: Record<string, unknown>) => capture({ interfaces: [{ id: 0, section: 0, linkType: 1, snapLength: 65535, timestampScale: "10^6", timestampOffset: "0", ...change },
  { id: 1, section: 1, linkType: 101, snapLength: 262144, timestampScale: "2^30", timestampOffset: "-3600" }] });
const accepted = (value: StoredValue) => captureIndexViewModule.matches(value.type, value.data);

it("accepts every declared resolution exponent from 0 to 127 and signed i64 offsets, exactly as declared", async () => {
  for (const scale of ["10^0", "10^3", "10^6", "10^9", "10^99", "10^127", "2^0", "2^10", "2^127"]) expect(accepted(withInterface({ timestampScale: scale }))).toBe(true);
  for (const scale of ["10^128", "2^128", "2^999", "10^06", "10^-1", "10^", "2^1.5", "1e6", "10^6 ", "16^2", "10**6"]) expect(accepted(withInterface({ timestampScale: scale }))).toBe(false);
  for (const offset of ["-9223372036854775808", "9223372036854775807", "-1", "0"]) expect(accepted(withInterface({ timestampOffset: offset }))).toBe(true);
  for (const offset of ["9223372036854775808", "-9223372036854775809", "-0", "+5", "01", "1.5", ""]) expect(accepted(withInterface({ timestampOffset: offset }))).toBe(false);

  const node = shown(prepareSync(withInterface({ timestampScale: "10^127", timestampOffset: "-9223372036854775808" })));
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  press(tree, "1");
  expect(text(tree)).toContain("Timestamp ticks 1700000000000001 · scale 10^127 · offset -9223372036854775808 s");
  press(tree, "2");
  expect(text(tree)).toContain("Timestamp unknown · not recorded in this index · interface scale 10^127 · offset -9223372036854775808 s");
  act(() => tree.unmount());
});

it("refuses contradictory counts, unknown or duplicate interfaces, gaps and impossible lengths", () => {
  const described = (count: number) => Array.from({ length: count }, (_, id) => ({ id, section: 0, linkType: 1, snapLength: 65535, timestampScale: "10^6", timestampOffset: "0" }));
  expect(accepted(capture({ interfaces: described(256) }))).toBe(true);
  expect(accepted(capture({ interfaces: described(257) }))).toBe(false);
  expect(accepted(capture({ packetCount: "3", indexComplete: true }))).toBe(true);
  expect(accepted(capture({ packetCount: "70000", indexComplete: true }))).toBe(false);
  expect(accepted(capture({ packetCount: "3", indexComplete: false }))).toBe(false);
  expect(accepted(capture({ packetCount: "2", indexComplete: false }))).toBe(false);
  expect(accepted(capture({ packetCount: "0", indexComplete: true, packets: [] }))).toBe(true);
  expect(accepted(capture({ interfaces: [...described(2), described(1)[0]] }))).toBe(false);
  expect(accepted(capture({ interfaces: described(1) }))).toBe(false);
  expect(accepted(capture({ packets: [rows()[0], { ...rows()[1], interface: 7 }] }))).toBe(false);
  expect(accepted(capture({ packets: [rows()[0], rows()[2]] }))).toBe(false);
  expect(accepted(capture({ packets: [rows()[1], rows()[2]].map((row, at) => ({ ...row, number: at + 2 })) }))).toBe(false);
  expect(accepted(capture({ packets: [{ ...rows()[0], capturedLength: 61, originalLength: 60 }] }))).toBe(false);
  expect(accepted(capture({ packets: [{ ...rows()[0], capturedLength: 59, originalLength: 60 }] }))).toBe(true);
});

it("keeps the preview inside its line budget with format, count, size, digest and index status", async () => {
  const prepared = prepareSync(capture());
  for (const lines of [1, 2, 3]) {
    const node = shown(prepared, { mode: "preview", lines });
    expect(node.ownLines).toBeLessThanOrEqual(lines);
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(draw(node)); });
    const shownText = text(tree);
    expect(shownText).toContain("pcapng capture index");
    expect(shownText).toContain("index incomplete");
    expect(shownText.includes("70,000 packets · 1,048,576 B · sha256:abababababab…")).toBe(lines >= 2);
    expect(shownText.includes("Index incomplete · counts cover only what was indexed · 2 blocks skipped")).toBe(lines >= 3);
    expect(tree.root.findAllByType("table")).toHaveLength(0);
    expect(tree.root.findAllByType("input")).toHaveLength(0);
    act(() => tree.unmount());
  }
});

it("states the bounded index and its limits, and spends no unbudgeted detail rows", async () => {
  const node = shown(prepareSync(capture()));
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  const all = text(tree);
  expect(all).toContain("Rows for packets 1–3 only · 3 of 70,000 · search covers these rows");
  expect(all).toContain(`File ${sha}`);
  expect(all).toContain("says nothing about drops or traffic coverage");
  expect(all).not.toMatch(/TCP|UDP|IPv4|Ethernet|\d+\.\d+\.\d+\.\d+/);
  act(() => tree.unmount());

  // Six fact lines, filter, column header, status and three rows: no room for the six-line detail.
  const tight = shown(prepareSync(capture()), { lines: 12 });
  expect(tight.ownLines).toBe(12);
  await act(async () => { tree = create(draw(tight)); });
  expect(tree.root.findAllByProps({ "aria-label": "Selected packet" })).toHaveLength(0);
  expect(tree.root.findAllByType("tr")).toHaveLength(1 + 3);
  act(() => tree.unmount());

  const crowded = shown(prepareSync(capture()), { lines: 9 });
  expect(crowded.ownLines).toBeLessThanOrEqual(9);
  await act(async () => { tree = create(draw(crowded)); });
  expect(tree.root.findAllByType("table")).toHaveLength(0);
  expect(tree.root.findAllByType("input")).toHaveLength(0);
  act(() => tree.unmount());
});

it("shows a selected packet's exact coordinates and an unknown timestamp as unknown", async () => {
  const node = shown(prepareSync(capture()));
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  expect(text(tree)).toContain("Select a packet row to see its index facts.");
  press(tree, "2");
  const all = text(tree);
  expect(all).toContain("Packet 2 · interface 0");
  expect(all).toContain("Record offset 100 · data offset 116");
  expect(all).toContain("Captured data at original file bytes 116 to 4211");
  expect(all).toContain("Captured 4,096 B · original 9,000 B · fewer bytes stored than the original length");
  expect(all).toContain("Timestamp unknown · not recorded in this index");
  expect(all).toContain("Interface 0 · section 0 · link type 1 · snap length 65,535");
  press(tree, "3");
  expect(text(tree)).toContain("Timestamp ticks 5 · scale 2^30 · offset -3600 s");
  act(() => tree.unmount());
});

it("keeps large u64 file coordinates exact", async () => {
  const big = capture({ rawBytes: "18446744073709551615", packetCount: "1", indexComplete: true, skippedBlocks: 0,
    packets: [{ number: 1, interface: 0, recordOffset: "18446744073709550984", dataOffset: "18446744073709551000", capturedLength: 16, originalLength: new ExactNumber("16"), timestampTicks: { kind: "none" } }] });
  expect(captureIndexViewModule.matches(big.type, big.data)).toBe(true);
  const node = shown(prepareSync(big));
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  expect(text(tree)).toContain("18446744073709551000");
  expect(text(tree)).toContain("All 1 indexed packet rows are here");
  expect(text(tree)).toContain("index complete");
  press(tree, "1");
  expect(text(tree)).toContain("Captured data at original file bytes 18446744073709551000 to 18446744073709551015");
  expect(text(tree)).toContain("Captured 16 B · original 16 B");
  act(() => tree.unmount());
});

it("filters only the supplied rows and says how many matched", async () => {
  const node = shown(prepareSync(capture()));
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  filter(tree, "4228");
  expect(tree.root.findAllByType("tr")).toHaveLength(2);
  expect(text(tree)).toContain("1 of 3 indexed rows match · showing 1–1");
  filter(tree, "unknown");
  expect(tree.root.findAllByType("button").map(it => it.children.join(""))).toEqual(["2"]);
  filter(tree, "no-such-row");
  expect(text(tree)).toContain("No indexed rows match · 3 rows searched");
  act(() => tree.unmount());
});

it("resets selection and filter when the value is refreshed or withdrawn, not when the pane resizes", async () => {
  const prepared = prepareSync(capture());
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(shown(prepared))); });
  filter(tree, "if0"); press(tree, "2");
  await act(async () => tree.update(draw(shown(prepared, { columns: 60 }))));
  expect(text(tree)).toContain("Packet 2 · interface 0");
  expect(tree.root.findByProps({ "aria-label": "Filter indexed packet rows" }).props.value).toBe("if0");
  await act(async () => tree.update(draw(shown(prepareSync(capture({ packetCount: "3", indexComplete: true, skippedBlocks: 0 }))))));
  expect(text(tree)).not.toContain("Packet 2 · interface 0");
  expect(text(tree)).toContain("Select a packet row to see its index facts.");
  expect(tree.root.findByProps({ "aria-label": "Filter indexed packet rows" }).props.value).toBe("");
  act(() => tree.unmount());
});

it("has no iframe, links, export actions or network side effects", async () => {
  const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
  const url = vi.spyOn(URL, "createObjectURL");
  const node = shown(prepareSync(capture()));
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(node)); });
  press(tree, "1"); filter(tree, "40");
  expect(tree.root.findAllByType("iframe")).toHaveLength(0);
  expect(tree.root.findAllByType("a")).toHaveLength(0);
  expect(tree.root.findAllByType("button").map(it => it.children.join("")).every(label => /^\d+$/.test(label) || /rows$/.test(label))).toBe(true);
  expect(text(tree)).not.toMatch(/Copy|Download|Export|Open in/);
  expect(fetch).not.toHaveBeenCalled();
  expect(url).not.toHaveBeenCalled();
  act(() => tree.unmount());
  vi.unstubAllGlobals();
});
