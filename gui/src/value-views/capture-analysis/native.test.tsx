import { act, create, type ReactTestRenderer, type ReactTestRendererJSON } from "react-test-renderer";
import { afterEach, expect, it, vi } from "vitest";
import { ExactNumber } from "../../exact-json";
import { prepareSync, type Prepared } from "../../presentation/prepare";
import { present } from "../../presentation/present";
import { Registry } from "../../presentation/registry";
import type { Context } from "../../presentation/types";
import type { StoredValue, TypeShape } from "../../protocol";
import { captureIndexViewModule } from "../capture-index/module";
import { captureAnalysisViewModule, MAX_FIELDS, MAX_QUERY } from "./module";

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });
const context: Context = { mode: "window", columns: 200, lines: 4000, density: "normal", locale: "en-GB", timeZone: "UTC" };
const p = (name: string): TypeShape => ({ kind: "primitive", name });
const opt = (element: TypeShape): TypeShape => ({ kind: "option", element });
const list = (element: TypeShape): TypeShape => ({ kind: "list", element });
const rec = (name: string, fields: Record<string, TypeShape>): TypeShape => ({ kind: "record", name, fields: Object.entries(fields).map(([n, type]) => ({ name: n, type })) });
const indexType = rec("CaptureIndex", { format: p("TEXT"), rawBytes: p("TEXT"), sha256: p("TEXT"), packetCount: p("TEXT"), indexComplete: p("BOOL"), skippedBlocks: p("INT"),
  interfaces: list(rec("CaptureInterface", { id: p("INT"), section: p("INT"), linkType: p("INT"), snapLength: p("INT"), timestampScale: p("TEXT"), timestampOffset: p("TEXT") })),
  packets: list(rec("CapturePacket", { number: p("INT"), interface: p("INT"), recordOffset: p("TEXT"), dataOffset: p("TEXT"), capturedLength: p("INT"), originalLength: p("INT"), timestampTicks: opt(p("TEXT")) })) });
const fieldType = rec("DecodedField", { layer: p("TEXT"), key: p("TEXT"), label: p("TEXT"), value: p("TEXT"), offset: p("TEXT"), length: p("INT") });
const packetType = rec("DecodedPacket", { number: p("INT"), interface: p("INT"), dataOffset: p("TEXT"), capturedLength: p("INT"), originalLength: p("INT"), protocol: p("TEXT"),
  source: opt(p("TEXT")), destination: opt(p("TEXT")), sourcePort: opt(p("INT")), destinationPort: opt(p("INT")), status: p("TEXT"), inspectedBytes: p("INT"),
  fields: list(fieldType), notes: list(p("TEXT")) });
const analysisFields = {
  decoder: rec("DecoderIdentity", { name: p("TEXT"), version: p("TEXT"), runtime: p("TEXT"), codeSha256: p("TEXT"), containerSha256: p("TEXT"), schemaSha256: p("TEXT") }),
  options: rec("AnalysisOptions", { packetLimit: p("INT"), headerBytes: p("INT") }),
  index: indexType, packets: list(packetType), sampleComplete: p("BOOL"), limitations: list(p("TEXT")),
};
const analysisType = (name = "CaptureAnalysis") => rec(name, analysisFields);

const sha = (digit: string) => `sha256:${digit.repeat(64)}`;
const none = { kind: "none" };
const some = (value: unknown) => ({ kind: "some", value });
const f = (layer: string, key: string, label: string, value: string, offset: string, length: number) => ({ layer, key, label, value, offset, length });
const indexRows = () => [
  { number: 1, interface: 0, recordOffset: "24", dataOffset: "40", capturedLength: 60, originalLength: 60, timestampTicks: none },
  { number: 2, interface: 0, recordOffset: "100", dataOffset: "116", capturedLength: 4096, originalLength: 9000, timestampTicks: none },
  { number: 3, interface: 1, recordOffset: "4212", dataOffset: "4228", capturedLength: 32, originalLength: 32, timestampTicks: none },
  { number: 4, interface: 0, recordOffset: "4260", dataOffset: "4276", capturedLength: 10, originalLength: 10, timestampTicks: none },
];
const indexData = (change: Record<string, unknown> = {}) => ({ format: "pcapng", rawBytes: "1048576", sha256: sha("a"), packetCount: "70000", indexComplete: false, skippedBlocks: 0,
  interfaces: [{ id: 0, section: 0, linkType: 1, snapLength: 65535, timestampScale: "10^6", timestampOffset: "0" },
    { id: 1, section: 1, linkType: 101, snapLength: 262144, timestampScale: "10^9", timestampOffset: "0" }],
  packets: indexRows(), ...change });
const tcpFields = () => [
  f("Ethernet", "destination", "Destination MAC", "02:00:00:00:00:01", "40", 6), f("Ethernet", "source", "Source MAC", "02:00:00:00:00:02", "46", 6),
  f("Ethernet", "type", "EtherType", "0x0800", "52", 2), f("IPv4", "version", "Version", "4", "54", 1),
  f("IPv4", "source", "Source address", "192.0.2.1", "66", 4), f("IPv4", "destination", "Destination address", "198.51.100.7", "70", 4),
  f("TCP", "sourcePort", "Source port", "443", "74", 2), f("TCP", "destinationPort", "Destination port", "51000", "76", 2),
  f("TCP", "flags", "Flags", "0x12 · SYN,ACK", "87", 1),
];
const packets = () => [
  { number: 1, interface: 0, dataOffset: "40", capturedLength: 60, originalLength: 60, protocol: "TCP", source: some("192.0.2.1"), destination: some("198.51.100.7"),
    sourcePort: some(443), destinationPort: some(51000), status: "decoded", inspectedBytes: 60, fields: tcpFields(), notes: [] },
  { number: 2, interface: 0, dataOffset: "116", capturedLength: 4096, originalLength: 9000, protocol: "UDP", source: some("192.0.2.9"), destination: some("203.0.113.4"),
    sourcePort: some(53), destinationPort: some(5353), status: "truncated", inspectedBytes: 128,
    fields: [f("Ethernet", "type", "EtherType", "0x0800", "128", 2), f("IPv4", "source", "Source address", "192.0.2.9", "142", 4),
      f("UDP", "sourcePort", "Source port", "53", "150", 2), f("UDP", "destinationPort", "Destination port", "5353", "152", 2)],
    notes: ["Captured packet is shorter than its recorded original length."] },
  { number: 3, interface: 1, dataOffset: "4228", capturedLength: 32, originalLength: 32, protocol: "unknown", source: none, destination: none,
    sourcePort: none, destinationPort: none, status: "unsupported", inspectedBytes: 32, fields: [], notes: ["Unsupported link type 101."] },
];
const fourth = { number: 4, interface: 0, dataOffset: "4276", capturedLength: 10, originalLength: 10, protocol: "Ethernet", source: none, destination: none,
  sourcePort: none, destinationPort: none, status: "truncated", inspectedBytes: 10, fields: [], notes: ["Ethernet header is incomplete in captured bytes."] };
const limitations = ["Fixed Ethernet/IPv4/TCP/UDP headers only.", "Filters cover this sample only."];
const analysis = (change: Record<string, unknown> = {}, name?: string): StoredValue => ({ type: analysisType(name), data: {
  decoder: { name: "wes-packet-headers", version: "1", runtime: "Python 3.12.1", codeSha256: sha("b"), containerSha256: sha("c"), schemaSha256: sha("d") },
  options: { packetLimit: 3, headerBytes: 128 }, index: indexData(), packets: packets(), sampleComplete: false, limitations, ...change } } as StoredValue);
const withPacket = (at: number, change: Record<string, unknown>) => analysis({ packets: packets().map((row, i) => i === at ? { ...row, ...change } : row) });
const accepted = (value: StoredValue) => captureAnalysisViewModule.matches(value.type, value.data);

const shown = (prepared: Prepared, overrides: Partial<Context> = {}) => {
  const node = present({ prepared, facts: { state: "ready" }, context: { ...context, ...overrides }, registry: Registry.core() }).root;
  if (node.kind !== "view" || node.view !== "capture-analysis") throw new Error("Expected capture analysis presentation");
  return node;
};
const draw = (node: ReturnType<typeof shown>) => { const Component = captureAnalysisViewModule.Component; return <Component model={node.model} children={node.children} renderChild={() => null} />; };
const text = (tree: ReactTestRenderer) => {
  const out: string[] = [];
  const visit = (node: ReactTestRendererJSON | string | null | (ReactTestRendererJSON | string)[]) => {
    if (node === null) return; if (typeof node === "string") { out.push(node); return; }
    if (Array.isArray(node)) { node.forEach(visit); return; } node.children?.forEach(visit);
  };
  visit(tree.toJSON()); return out.join("\n");
};
const labels = (tree: ReactTestRenderer) => tree.root.findAllByType("button").map(it => it.children.join(""));
const press = (tree: ReactTestRenderer, label: string) => {
  const button = tree.root.findAllByType("button").find(it => it.children.join("") === label);
  if (!button) throw new Error(`Missing button ${label}`);
  act(() => button.props.onClick());
};
const filter = (tree: ReactTestRenderer, query: string) => act(() => tree.root.findByProps({ "aria-label": "Filter sampled packet rows" }).props.onChange({ target: { value: query } }));
const render = async (node: ReturnType<typeof shown>) => { let tree!: ReactTestRenderer; await act(async () => { tree = create(draw(node)); }); return tree; };
const sampleRows = (tree: ReactTestRenderer) => tree.root.findByProps({ "aria-label": "Sampled packets" }).findAllByType("tr").length - 1;

it("matches analysis structure and validated facts, never a name, and leaves the index view index-only", () => {
  const value = analysis();
  expect(accepted(value)).toBe(true);
  expect(accepted(analysis({}, "AnyName"))).toBe(true);
  const partial = Object.fromEntries(Object.entries(analysisFields).filter(([name]) => name !== "packets"));
  expect(captureAnalysisViewModule.matches(rec("CaptureAnalysis", partial), value.data)).toBe(false);
  expect(captureAnalysisViewModule.matches(rec("CaptureAnalysis", { ...analysisFields, index: rec("CaptureIndex", { format: p("TEXT") }) }), value.data)).toBe(false);
  expect(captureAnalysisViewModule.matches(value.type, "not a record")).toBe(false);
  expect(captureIndexViewModule.matches(value.type, value.data)).toBe(false);
  expect(captureIndexViewModule.matches(indexType, (value.data as Record<string, unknown>).index)).toBe(true);
  expect(captureAnalysisViewModule.matches(indexType, indexData())).toBe(false);
  expect(shown(prepareSync(value)).view).toBe("capture-analysis");
  const fallback = present({ prepared: prepareSync(analysis({ sampleComplete: true })), facts: { state: "ready" }, context, registry: Registry.core() }).root;
  expect(fallback.kind === "view" && fallback.view === "capture-analysis").toBe(false);
});

it("refuses malformed identity, options, option wire forms, statuses and text", () => {
  const decoder = (change: Record<string, unknown>) => analysis({ decoder: { name: "wes-packet-headers", version: "1", runtime: "Python 3.12.1", codeSha256: sha("b"), containerSha256: sha("c"), schemaSha256: sha("d"), ...change } });
  for (const change of [{ codeSha256: "sha256:XYZ" }, { schemaSha256: sha("B") }, { name: "" }, { runtime: "Python\n3" }]) expect(accepted(decoder(change))).toBe(false);
  for (const options of [{ packetLimit: 0, headerBytes: 128 }, { packetLimit: 513, headerBytes: 128 }, { packetLimit: 3, headerBytes: 63 }, { packetLimit: 3, headerBytes: 65537 },
    { packetLimit: 2.5, headerBytes: 128 }, { packetLimit: "3", headerBytes: 128 }]) expect(accepted(analysis({ options }))).toBe(false);
  expect(accepted(analysis({ options: { packetLimit: new ExactNumber("3"), headerBytes: new ExactNumber("128") } }))).toBe(true);
  for (const change of [{ source: null }, { source: "192.0.2.1" }, { source: { kind: "some" } }, { sourcePort: some(65536) }, { sourcePort: some(-1) },
    { destinationPort: some("443") }, { destinationPort: null }, { status: "ok" }, { protocol: "TCP\n" }, { protocol: "" }, { notes: ["\u202eevil"] }])
    expect(accepted(withPacket(0, change))).toBe(false);
  expect(accepted(withPacket(0, { sourcePort: some(new ExactNumber("443")) }))).toBe(true);
  expect(accepted(withPacket(0, { fields: [f("IPv4", "source", "Source address", "\u202e1.2.0.291", "66", 4)] }))).toBe(false);
  expect(accepted(analysis({ index: indexData({ sha256: "md5:00" }) }))).toBe(false);
  expect(accepted(analysis({ limitations: "none" }))).toBe(false);
});

it("refuses contradictory sampling and rows that disagree with the index", () => {
  expect(accepted(analysis({ sampleComplete: true }))).toBe(false);
  expect(accepted(analysis({ options: { packetLimit: 2, headerBytes: 128 } }))).toBe(false);
  expect(accepted(analysis({ options: { packetLimit: 4, headerBytes: 128 } }))).toBe(false);
  expect(accepted(analysis({ options: { packetLimit: 4, headerBytes: 128 }, packets: [...packets(), fourth] }))).toBe(true);
  const complete = indexData({ packetCount: "3", indexComplete: true, packets: indexRows().slice(0, 3) });
  expect(accepted(analysis({ index: complete, options: { packetLimit: 512, headerBytes: 128 }, sampleComplete: true }))).toBe(true);
  expect(accepted(analysis({ index: complete, options: { packetLimit: 512, headerBytes: 128 }, sampleComplete: false }))).toBe(false);
  // Sample rows beyond the indexed rows cannot be cross-checked and are refused.
  expect(accepted(analysis({ index: indexData({ packets: indexRows().slice(0, 2) }) }))).toBe(false);
  expect(accepted(analysis({ packets: [packets()[1], packets()[0], packets()[2]] }))).toBe(false);
  expect(accepted(analysis({ packets: [packets()[0], { ...packets()[2], number: 2 }, { ...packets()[2] }] }))).toBe(false);
  for (const change of [{ interface: 1 }, { dataOffset: "41" }, { capturedLength: 61 }, { originalLength: 61 }, { number: 0 }])
    expect(accepted(withPacket(0, change))).toBe(false);
  // A short capture is never "decoded"; inspected bytes stay within the capture and the header budget.
  expect(accepted(withPacket(1, { status: "decoded" }))).toBe(false);
  expect(accepted(withPacket(0, { inspectedBytes: 61 }))).toBe(false);
  expect(accepted(withPacket(1, { inspectedBytes: 129 }))).toBe(false);
  expect(accepted(withPacket(1, { inspectedBytes: 64, fields: [] }))).toBe(true);
});

it("refuses oversized lists and field ranges outside the inspected original interval", () => {
  const spread = (count: number) => Array.from({ length: count }, (_, at) => f("Probe", `k${at}`, `Field ${at}`, at === count - 1 ? "needle-last" : String(at), String(40 + (at % 60)), 1));
  expect(accepted(withPacket(0, { fields: spread(MAX_FIELDS) }))).toBe(true);
  expect(accepted(withPacket(0, { fields: spread(MAX_FIELDS + 1) }))).toBe(false);
  expect(accepted(withPacket(0, { notes: Array.from({ length: 9 }, (_, at) => `note ${at}`) }))).toBe(false);
  expect(accepted(analysis({ limitations: Array.from({ length: 17 }, (_, at) => `limit ${at}`) }))).toBe(false);
  for (const field of [f("IPv4", "x", "X", "1", "39", 2), f("TCP", "x", "X", "1", "99", 2), f("TCP", "x", "X", "1", "50", 0),
    f("TCP", "x", "X", "1", "18446744073709551616", 1), f("TCP", "x", "X", "1", "050", 1), f("TCP", "x", "X", "v".repeat(257), "50", 1)])
    expect(accepted(withPacket(0, { fields: [field] }))).toBe(false);
  expect(accepted(withPacket(0, { fields: [f("TCP", "x", "X", "1", "99", 1)] }))).toBe(true);
  // Nothing inspected means no field can lie inside the inspected interval.
  expect(accepted(withPacket(2, { inspectedBytes: 0, fields: [f("X", "x", "X", "1", "4228", 1)] }))).toBe(false);
});

const big = (offset = "18446744073709551062") => analysis({
  options: { packetLimit: 1, headerBytes: 64 }, sampleComplete: true,
  index: indexData({ rawBytes: "18446744073709551615", packetCount: "1", indexComplete: true,
    packets: [{ number: 1, interface: 0, recordOffset: "18446744073709550984", dataOffset: "18446744073709551000", capturedLength: 64, originalLength: new ExactNumber("64"), timestampTicks: none }] }),
  packets: [{ number: new ExactNumber("1"), interface: 0, dataOffset: "18446744073709551000", capturedLength: new ExactNumber("64"), originalLength: 64, protocol: "Ethernet",
    source: none, destination: none, sourcePort: none, destinationPort: none, status: "unsupported", inspectedBytes: 64,
    fields: [f("Ethernet", "type", "Probe", "0x88b5", offset, 2)], notes: ["EtherType is not supported by this IPv4 decoder."] }],
});

it("keeps u64 coordinates exact at the boundary and refuses one byte past the inspected end", async () => {
  expect(accepted(big())).toBe(true);
  expect(accepted(big("18446744073709551063"))).toBe(false);
  expect(accepted(big("18446744073709550999"))).toBe(false);
  const tree = await render(shown(prepareSync(big())));
  expect(text(tree)).toContain("every container packet sampled");
  press(tree, "1");
  expect(text(tree)).toContain("original bytes 18446744073709551000–18446744073709551063 · captured 64 of 64 B · inspected 64 B at 18446744073709551000–18446744073709551063");
  press(tree, "Probe");
  expect(text(tree)).toContain("Ethernet Probe = 0x88b5 · original capture bytes 18446744073709551062–18446744073709551063 (2 bytes, packet offset 62) · coordinates only, bytes not read by this view");
  act(() => tree.unmount());
});

it("keeps preview inside its line budget with no table, filter or panel", async () => {
  const prepared = prepareSync(analysis());
  for (const lines of [1, 2, 3]) {
    const node = shown(prepared, { mode: "preview", lines });
    expect(node.ownLines).toBeLessThanOrEqual(lines);
    const tree = await render(node), all = text(tree);
    expect(all).toContain("Packet header analysis");
    expect(all).toContain("partial sample");
    expect(all.includes("Sample packets 1–3 of 70,000 · limit 3 · header prefix ≤ 128 B per packet")).toBe(lines >= 2);
    expect(all.includes("1 decoded · 1 unsupported · 1 truncated · TCP 1 · UDP 1 · unknown 1")).toBe(lines >= 3);
    expect(tree.root.findAllByType("table")).toHaveLength(0);
    expect(tree.root.findAllByType("input")).toHaveLength(0);
    expect(tree.root.findAllByProps({ role: "tablist" })).toHaveLength(0);
    act(() => tree.unmount());
  }
});

it("spends only budgeted window lines for facts, table and detail panel", async () => {
  const prepared = prepareSync(analysis());
  for (let lines = 1; lines <= 40; lines++) expect(shown(prepared, { lines }).ownLines).toBeLessThanOrEqual(lines);
  const crowded = shown(prepared, { lines: 8 });
  expect(crowded.ownLines).toBe(5);
  let tree = await render(crowded);
  expect(tree.root.findAllByType("table")).toHaveLength(0);
  expect(tree.root.findAllByType("input")).toHaveLength(0);
  act(() => tree.unmount());
  // Five facts, filter, column header, status and three rows: no room for the tab row and panel.
  const tight = shown(prepared, { lines: 12 });
  expect(tight.ownLines).toBe(11);
  tree = await render(tight);
  expect(sampleRows(tree)).toBe(3);
  expect(tree.root.findAllByProps({ role: "tablist" })).toHaveLength(0);
  act(() => tree.unmount());
  const fitted = shown(prepared, { lines: 20 });
  expect(fitted.ownLines).toBe(20);
  tree = await render(fitted);
  expect(tree.root.findAllByProps({ role: "tablist" })).toHaveLength(1);
  press(tree, "1");
  // An eight-line panel pages the nine fields two at a time beneath four packet lines.
  expect(text(tree)).toContain("Rows 1–2 of 9");
  press(tree, "Later fields");
  expect(text(tree)).toContain("Rows 3–4 of 9");
  act(() => tree.unmount());
  const narrow = shown(prepared, { columns: 40 });
  tree = await render(narrow);
  expect(text(tree).split("\n").some(line => line.endsWith("…"))).toBe(true);
  expect(tree.root.findByProps({ "aria-label": "Sampled packets" }).findAllByType("th").map(it => it.children.join(""))).toEqual(["#", "Protocol", "Status", "Inspected"]);
  act(() => tree.unmount());
});

it("states partial, unsupported and truncated facts honestly without inventing semantics", async () => {
  const tree = await render(shown(prepareSync(analysis())));
  let all = text(tree);
  expect(all).toContain("partial sample");
  expect(all).toContain(`pcapng 1,048,576 B · ${sha("a")} · index incomplete, 4 rows`);
  expect(all).toContain("not traffic coverage");
  expect(all).toContain("not decoded");
  expect(all).toContain("Select a sampled packet row to see its decoded header fields.");
  press(tree, "3");
  all = text(tree);
  expect(all).toContain("Packet 3 · interface 1 · link type 101 · unknown · unsupported · addresses not decoded");
  expect(all).toContain("link type or protocol not supported by this decoder · 1 decoder note");
  expect(all).toContain("No header fields were decoded from the inspected bytes");
  expect(all).toContain("Unsupported link type 101.");
  press(tree, "2");
  all = text(tree);
  expect(all).toContain("Packet 2 · interface 0 · link type 1 · UDP · truncated · 192.0.2.9:53 → 203.0.113.4:5353");
  expect(all).toContain("original bytes 116–4211 · captured 4,096 of 9,000 B · inspected 128 B at 116–243");
  expect(all).toContain("header bytes missing from the capture or the inspected prefix");
  press(tree, "Decoder");
  all = text(tree);
  for (const digest of [sha("b"), sha("c"), sha("d"), sha("a")]) expect(all).toContain(digest);
  expect(all).toContain("first 3 packets · header prefix 128 B");
  expect(all).toContain("grants no access to original bytes");
  press(tree, "Limits · 2");
  all = text(tree);
  for (const limit of limitations) expect(all).toContain(limit);
  expect(all).toContain("Not implemented: payload, options, flow, RTT, retransmission, decryption, reassembly");
  expect(all).not.toMatch(/timestamp|connection id|retransmitted|decrypted|dropped|complete traffic|:read/i);
  act(() => tree.unmount());
});

it("is honest about an empty capture and complete sample coverage", async () => {
  const empty = analysis({ index: indexData({ packetCount: "0", indexComplete: true, packets: [] }), packets: [], sampleComplete: true, limitations: [] });
  expect(accepted(empty)).toBe(true);
  const tree = await render(shown(prepareSync(empty)));
  const all = text(tree);
  expect(all).toContain("every container packet sampled");
  expect(all).toContain("No packets in this pcapng container · nothing sampled");
  expect(all).toContain("No decode statuses · no rows sampled");
  expect(all).toContain("not traffic coverage");
  expect(tree.root.findAllByType("input")).toHaveLength(0);
  expect(tree.root.findAllByProps({ role: "tab" }).map(it => it.children.join(""))).toEqual(["Decoder", "Limits · 0"]);
  press(tree, "Limits · 0");
  expect(text(tree)).toContain("The helper recorded no limitations");
  act(() => tree.unmount());
});

it("groups a selected packet's fields by layer and shows field coordinates without reading bytes", async () => {
  const tree = await render(shown(prepareSync(analysis())));
  press(tree, "1");
  expect(text(tree)).toContain("Packet 1 · interface 0 · link type 1 · TCP · decoded · 192.0.2.1:443 → 198.51.100.7:51000");
  expect(text(tree)).toContain("original bytes 40–99 · captured 60 of 60 B · inspected 60 B at 40–99");
  expect(text(tree)).toContain("9 header fields · select one for its original byte coordinates");
  const fields = tree.root.findByProps({ "aria-label": "Header fields of packet 1" });
  expect(fields.findAllByType("td").filter(it => String(it.props.className).startsWith("capture-analysis-layer")).map(it => it.children.join("")).filter(Boolean)).toEqual(["Ethernet", "IPv4", "TCP"]);
  press(tree, "Source address");
  expect(text(tree)).toContain("IPv4 Source address = 192.0.2.1 · original capture bytes 66–69 (4 bytes, packet offset 26) · coordinates only, bytes not read by this view");
  press(tree, "Flags");
  expect(text(tree)).toContain("TCP Flags = 0x12 · SYN,ACK · original capture bytes 87 (1 byte, packet offset 47)");
  press(tree, "2");
  expect(text(tree)).toContain("4 header fields · select one for its original byte coordinates");
  act(() => tree.unmount());
});

it("filters only supplied sample rows across facts, every field and notes, with bounded query text", async () => {
  const deep = withPacket(0, { fields: Array.from({ length: MAX_FIELDS }, (_, at) => f("Probe", `k${at}`, `Field ${at}`, at === MAX_FIELDS - 1 ? "needle-last" : String(at), String(40 + (at % 60)), 1)) });
  let tree = await render(shown(prepareSync(deep)));
  filter(tree, "needle-last");
  expect(sampleRows(tree)).toBe(1);
  expect(text(tree)).toContain("1 of 3 sampled rows match · showing 1–1 · search covers this sample only");
  act(() => tree.unmount());
  tree = await render(shown(prepareSync(analysis())));
  filter(tree, "syn");
  expect(labels(tree).filter(it => /^\d+$/.test(it))).toEqual(["1"]);
  filter(tree, "5353");
  expect(labels(tree).filter(it => /^\d+$/.test(it))).toEqual(["2"]);
  filter(tree, "link type 101");
  expect(labels(tree).filter(it => /^\d+$/.test(it))).toEqual(["3"]);
  filter(tree, "no-such-row");
  expect(text(tree)).toContain("No sampled rows match · 3 rows searched");
  filter(tree, "x".repeat(500));
  expect(tree.root.findByProps({ "aria-label": "Filter sampled packet rows" }).props.value).toHaveLength(MAX_QUERY);
  act(() => tree.unmount());
});

it("resets filter and selections when data is replaced, not when the same value resizes", async () => {
  const prepared = prepareSync(analysis());
  const tree = await render(shown(prepared));
  filter(tree, "if0"); press(tree, "1"); press(tree, "Source address");
  await act(async () => tree.update(draw(shown(prepared, { columns: 60 }))));
  expect(text(tree)).toContain("IPv4 Source address = 192.0.2.1");
  expect(tree.root.findByProps({ "aria-label": "Filter sampled packet rows" }).props.value).toBe("if0");
  await act(async () => tree.update(draw(shown(prepareSync(analysis())))));
  expect(text(tree)).not.toContain("IPv4 Source address = 192.0.2.1");
  expect(text(tree)).toContain("Select a sampled packet row to see its decoded header fields.");
  expect(tree.root.findByProps({ "aria-label": "Filter sampled packet rows" }).props.value).toBe("");
  act(() => tree.unmount());
});

it("has no links, frames, exports, commands or network side effects", async () => {
  const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
  const url = vi.spyOn(URL, "createObjectURL");
  const tree = await render(shown(prepareSync(analysis())));
  press(tree, "1"); press(tree, "Source address"); filter(tree, "tcp");
  press(tree, "Decoder"); press(tree, "Limits · 2");
  expect(tree.root.findAllByType("iframe")).toHaveLength(0);
  expect(tree.root.findAllByType("a")).toHaveLength(0);
  expect(tree.root.findAllByType("input").map(it => it.props.type)).toEqual(["search"]);
  expect(labels(tree).some(label => /copy|download|export|open|read|fetch|start|stop|capture/i.test(label))).toBe(false);
  expect(text(tree)).not.toMatch(/https?:|:read|Copy|Download|Export/);
  expect(fetch).not.toHaveBeenCalled();
  expect(url).not.toHaveBeenCalled();
  act(() => tree.unmount());
});
