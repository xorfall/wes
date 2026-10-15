import { act, create, type ReactTestRenderer, type ReactTestRendererJSON } from "react-test-renderer";
import { afterEach, expect, it, vi } from "vitest";
import { ExactNumber } from "../../exact-json";
import { prepareSync, type Prepared } from "../../presentation/prepare";
import { present } from "../../presentation/present";
import { Registry } from "../../presentation/registry";
import type { Context } from "../../presentation/types";
import type { StoredValue, TypeShape } from "../../protocol";
import { captureStatusViewModule, MAX_WINDOW } from "./module";

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });
const context: Context = { mode: "window", columns: 120, lines: 4000, density: "normal", locale: "en-GB", timeZone: "UTC" };
const p = (name: string): TypeShape => ({ kind: "primitive", name });
const rec = (name: string, fields: Record<string, TypeShape>): TypeShape => ({ kind: "record", name, fields: Object.entries(fields).map(([n, type]) => ({ name: n, type })) });
const profileType = rec("CaptureProfile", { name: p("TEXT"), backend: p("TEXT"), interface: p("TEXT"), filter: p("TEXT"), durationMs: p("INT"),
  maxBytes: p("INT"), packets: p("INT"), snapLength: p("INT"), promiscuous: p("BOOL") });
const statusType = (name = "CaptureStatus") => rec(name, { profile: profileType, phase: p("TEXT"), rawBytes: p("TEXT"), packetCount: p("TEXT"), truncatedPackets: p("TEXT"),
  drops: { kind: "option", element: p("TEXT") }, containerComplete: p("BOOL"), indexComplete: p("BOOL"), coverageKnown: p("BOOL") });
const windowType: TypeShape = { kind: "list", element: statusType() };

const profile = (change: Record<string, unknown> = {}) => ({ name: "edge-https", backend: "/usr/local/bin/dumpcap", interface: "en0", filter: "tcp port 443",
  durationMs: 30000, maxBytes: 10485760, packets: 1000, snapLength: 96, promiscuous: false, ...change });
const status = (change: Record<string, unknown> = {}) => ({ profile: profile(), phase: "ready", rawBytes: "24040", packetCount: "120", truncatedPackets: "37",
  drops: { kind: "none" }, containerComplete: true, indexComplete: true, coverageKnown: false, ...change });
/** The native initial summary: nothing acknowledged yet, neither completeness fact claimed. */
const opening = { phase: "opening", rawBytes: "0", packetCount: "0", truncatedPackets: "0", containerComplete: false, indexComplete: false };
const single = (change: Record<string, unknown> = {}, name?: string): StoredValue => ({ type: statusType(name), data: status(change) } as StoredValue);
const rolling = (count: number, last: Record<string, unknown> = {}): StoredValue => ({ type: windowType, data: Array.from({ length: count }, (_, at) => status({
  packetCount: String((at + 1) * 10), rawBytes: String(24 + 16 * (at + 1) * 10 + 100), truncatedPackets: "0", ...(at === count - 1 ? last : {}) })) } as StoredValue);
const accepted = (value: StoredValue) => captureStatusViewModule.matches(value.type, value.data);

const shown = (prepared: Prepared, overrides: Partial<Context> = {}) => {
  const node = present({ prepared, facts: { state: "ready" }, context: { ...context, ...overrides }, registry: Registry.core() }).root;
  if (node.kind !== "view" || node.view !== "capture-status") throw new Error("Expected capture status presentation");
  return node;
};
const draw = (node: ReturnType<typeof shown>) => { const Component = captureStatusViewModule.Component; return <Component model={node.model} children={node.children} renderChild={() => null} />; };
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
const mount = async (node: ReturnType<typeof shown>) => { let tree!: ReactTestRenderer; await act(async () => { tree = create(draw(node)); }); return tree; };

it("matches status structure and data, never a record name, and refuses malformed or contradictory facts", () => {
  expect(accepted(single())).toBe(true);
  expect(accepted(single({}, "Renamed"))).toBe(true);
  expect(accepted(single({ profile: profile({ maxBytes: new ExactNumber("536870912"), durationMs: new ExactNumber("1") }) }))).toBe(true);
  expect(accepted(single({ drops: { kind: "some", value: "0" } }))).toBe(true);
  expect(accepted(single(opening))).toBe(true);
  expect(captureStatusViewModule.matches(rec("CaptureStatus", { phase: p("TEXT") }), status())).toBe(false);
  expect(captureStatusViewModule.matches(statusType(), [status()])).toBe(false);
  const forged: Record<string, unknown>[] = [
    { coverageKnown: true }, { coverageKnown: "false" }, { phase: "running" }, { phase: "Ready" },
    { rawBytes: "01" }, { rawBytes: "18446744073709551616" }, { packetCount: 120 }, { truncatedPackets: "121" },
    { packetCount: "1001" }, { rawBytes: "1943" }, { drops: "0" }, { drops: null }, { drops: { kind: "some", value: "01" } }, { drops: { kind: "some", value: 0 } },
    { containerComplete: 1 }, { indexComplete: null },
  ];
  for (const change of forged) expect(accepted(single(change))).toBe(false);
  const profiles: Record<string, unknown>[] = [
    { name: "" }, { name: "n".repeat(65) }, { name: "a\tb" }, { backend: "dumpcap" }, { backend: "./dumpcap" }, { backend: "/usr/bin/" }, { backend: "/" },
    { backend: "C:\\Program Files\\Wireshark\\dumpcap.exe" }, { backend: "/usr/bin/dumpcap\n" }, { interface: "" },
    { interface: "en0\n" }, { interface: "e".repeat(1025) }, { interface: "-D" }, { interface: "eth0:1" }, { interface: "/dev/pipe" },
    { interface: "@remote" }, { interface: "rpcap\\x" }, { filter: "f".repeat(4097) }, { filter: "tcp\u0000" }, { filter: "\u0000" },
    { durationMs: 0 }, { durationMs: 120001 }, { durationMs: 1.5 }, { maxBytes: 23 }, { maxBytes: 536870913 }, { packets: 0 }, { packets: 1000001 },
    { snapLength: 0 }, { snapLength: 65536 }, { promiscuous: "no" }, { durationMs: new ExactNumber("1e3") },
  ];
  for (const change of profiles) expect(accepted(single({ profile: profile(change) }))).toBe(false);
  expect(accepted(single({ profile: profile({ name: "n".repeat(64), filter: "f".repeat(4096), snapLength: 65535, maxBytes: 536870912, packets: 1000000, durationMs: 120000 }) }))).toBe(true);
  expect(accepted(single({ profile: profile({ maxBytes: 24 }), rawBytes: "24", packetCount: "0", truncatedPackets: "0" }))).toBe(true);
  // Only the filter may be empty: it is the explicit absence of a filter; empty names, interfaces and backends stay malformed.
  expect(accepted(single({ profile: profile({ filter: "" }) }))).toBe(true);
  for (const change of [{ name: "" }, { interface: "" }, { backend: "" }]) expect(accepted(single({ profile: profile({ filter: "", ...change }) }))).toBe(false);
  const fallback = present({ prepared: prepareSync(single({ coverageKnown: true })), facts: { state: "ready" }, context, registry: Registry.core() }).root;
  expect(fallback.kind === "view" && fallback.view === "capture-status").toBe(false);
});

it("bounds the raw prefix by the frozen size limit and the 512-packet index by the packet count", () => {
  // Default profile maxBytes is 10,485,760.
  expect(accepted(single({ rawBytes: "10485760" }))).toBe(true);
  expect(accepted(single({ rawBytes: "10485761" }))).toBe(false);
  expect(accepted(single({ profile: profile({ maxBytes: 536870912 }), rawBytes: "536870912" }))).toBe(true);
  expect(accepted(single({ profile: profile({ maxBytes: 536870912 }), rawBytes: "536870913" }))).toBe(false);
  expect(accepted(single({ packetCount: "512", indexComplete: true }))).toBe(true);
  expect(accepted(single({ packetCount: "513", indexComplete: true }))).toBe(false);
  expect(accepted(single({ packetCount: "513", indexComplete: false }))).toBe(true);
  expect(accepted(single({ packetCount: "120", indexComplete: false }))).toBe(true);
  expect(accepted({ type: windowType, data: [status(), status({ rawBytes: "10485761" })] } as StoredValue)).toBe(false);
});

it("ties completeness to the acknowledged container boundary, as the provider reports it", async () => {
  expect(accepted(single({ containerComplete: true, indexComplete: true }))).toBe(true);
  expect(accepted(single({ containerComplete: true, indexComplete: false }))).toBe(true);
  expect(accepted(single({ containerComplete: false, indexComplete: true }))).toBe(false);
  expect(accepted(single({ ...opening, containerComplete: true }))).toBe(false);
  expect(accepted(single({ ...opening, rawBytes: "23", containerComplete: true }))).toBe(false);
  expect(accepted(single({ ...opening, rawBytes: "24", containerComplete: true }))).toBe(true);
  expect(accepted(single({ ...opening, indexComplete: true }))).toBe(false);
  expect(accepted(single({ phase: "ended", containerComplete: false, indexComplete: false }))).toBe(false);
  expect(accepted(single({ phase: "ended" }))).toBe(true);

  // A genuine incomplete prefix: a record cut mid-way leaves neither fact claimed, and that is reported as such.
  let tree = await mount(shown(prepareSync(single({ phase: "incomplete", rawBytes: "24050", containerComplete: false, indexComplete: false }))));
  expect(text(tree)).toContain("phase incomplete");
  expect(text(tree)).toContain("120 packets · 24,050 B · 37 truncated · snap length 96 requested");
  expect(text(tree)).toContain("Container not complete · packet index not complete · complete only when every packet has a row, at most 512");
  act(() => tree.unmount());

  tree = await mount(shown(prepareSync(single(opening))));
  expect(text(tree)).toContain("phase opening");
  expect(text(tree)).toContain("0 packets · 0 B · none truncated");
  expect(text(tree)).toContain("Drops unknown · traffic coverage not established");
  expect(text(tree)).toContain("Container not complete · packet index not complete");
  expect(text(tree)).toContain("Reported status · not proof of a live capture or of stored bytes");
  act(() => tree.unmount());

  // A clean 1,000-packet capture keeps its full counts; its index is not complete because not every packet has a row.
  tree = await mount(shown(prepareSync(single({ phase: "ended", rawBytes: "120024", packetCount: "1000", truncatedPackets: "0", indexComplete: false }))));
  expect(text(tree)).toContain("1,000 packets · 120,024 B · none truncated");
  expect(text(tree)).toContain("Container complete · packet index not complete · complete only when every packet has a row, at most 512");
  expect(text(tree)).not.toMatch(/first 512/);
  expect(text(tree)).toContain("Drops unknown");
  act(() => tree.unmount());
});

it("validates every status of a rolling window, including counters that may only grow within one capture", () => {
  expect(accepted({ type: windowType, data: [] } as StoredValue)).toBe(true);
  expect(accepted(rolling(5))).toBe(true);
  expect(accepted({ type: windowType, data: [status(), status({ phase: "zzz" })] } as StoredValue)).toBe(false);
  expect(accepted({ type: windowType, data: [status({ packetCount: "120" }), status({ packetCount: "119" })] } as StoredValue)).toBe(false);
  expect(accepted({ type: windowType, data: [status({ drops: { kind: "some", value: "4" } }), status({ drops: { kind: "some", value: "3" } })] } as StoredValue)).toBe(false);
  // After a reported end, a same-profile status may be a new capture starting over; a different profile always may.
  expect(accepted({ type: windowType, data: [status({ phase: "ended" }), status(opening)] } as StoredValue)).toBe(true);
  expect(accepted({ type: windowType, data: [status(), status({ profile: profile({ name: "other" }), packetCount: "1", rawBytes: "40", truncatedPackets: "0" })] } as StoredValue)).toBe(true);
  expect(accepted({ type: windowType, data: Array.from({ length: MAX_WINDOW + 1 }, () => status()) } as StoredValue)).toBe(false);
  expect(accepted({ type: { kind: "list", element: profileType }, data: [status()] } as StoredValue)).toBe(false);
});

it("keeps u64 counters exact and distinguishes unknown drops from zero, without claiming traffic coverage", async () => {
  const max = "18446744073709551615";
  // Drops are not bounded by the recorded packets or bytes; the raw prefix stays within the profile's limit.
  let tree = await mount(shown(prepareSync(single({ profile: profile({ maxBytes: 536870912 }), rawBytes: "536870912", packetCount: "1000",
    truncatedPackets: "0", indexComplete: false, drops: { kind: "some", value: max } }))));
  expect(text(tree)).toContain("1,000 packets · 536,870,912 B · none truncated");
  expect(text(tree)).toContain("18,446,744,073,709,551,615 drops reported · traffic coverage not established");
  act(() => tree.unmount());
  tree = await mount(shown(prepareSync(single({ drops: { kind: "none" } }))));
  expect(text(tree)).toContain("Drops unknown · traffic coverage not established");
  expect(text(tree)).not.toMatch(/0 drops|no drops/);
  act(() => tree.unmount());
  tree = await mount(shown(prepareSync(single({ drops: { kind: "some", value: "0" } }))));
  expect(text(tree)).toContain("0 drops reported · traffic coverage not established");
  expect(text(tree)).not.toContain("Drops unknown");
  expect(text(tree)).not.toMatch(/coverage (complete|known|established)|all traffic|nothing missed/i);
  act(() => tree.unmount());
});

it("counts truncation as a fact beside the requested snap length, separately from drops and the frozen profile", async () => {
  const tree = await mount(shown(prepareSync(single())));
  expect(text(tree)).toContain("120 packets · 24,040 B · 37 truncated · snap length 96 requested");
  expect(text(tree)).not.toMatch(/truncated by|caused by|because of/);
  expect(text(tree)).toContain("Drops unknown");
  expect(text(tree)).toContain("Container complete · packet index complete · every packet has a row");
  expect(text(tree)).toContain("Interface en0 · filter tcp port 443");
  expect(text(tree)).toContain("Reported status · not proof of a live capture or of stored bytes");
  press(tree, "Profile");
  const all = text(tree);
  for (const fact of ["edge-https", "/usr/local/bin/dumpcap · recorded path, not run by this view", "30,000 ms", "10,485,760 B", "1,000", "96 B requested", "not requested", "frozen"]) expect(all).toContain(fact);
  expect(all).not.toContain("longer packets are stored truncated");
  act(() => tree.unmount());
});

it("presents an empty filter as no filter in the scope and the profile", async () => {
  const tree = await mount(shown(prepareSync(single({ profile: profile({ filter: "" }) }))));
  expect(text(tree)).toContain("Interface en0 · no filter");
  expect(text(tree)).not.toContain("· filter");
  press(tree, "Profile");
  const filterRow = tree.root.findAllByType("tr").find(row => row.findByType("th").children.join("") === "Filter")!;
  expect(filterRow.findByType("td").children.join("")).toBe("none · no filter");
  act(() => tree.unmount());
});

it("states that original recording reads require physical join, whatever phase is reported", async () => {
  for (const phase of ["ready", "stopping", "ended", "incomplete"]) {
    const tree = await mount(shown(prepareSync(single({ phase, ...(phase === "incomplete" ? { containerComplete: false, indexComplete: false } : {}) }))));
    expect(text(tree)).toContain("Reported status · not proof of a live capture or of stored bytes");
    press(tree, "Profile");
    expect(text(tree)).toContain("Physical join required · :read bytes of the original recording · frozen snapshot · an ended phase is not proof");
    expect(text(tree)).not.toMatch(/\b(joined|ready to read|can be read|readable now)\b/i);
    expect(tree.root.findAllByType("button").map(it => it.children.join(""))).toEqual(["Status", "Profile"]);
    act(() => tree.unmount());
  }
});

it("shows the recorded Unix backend path as inert data and leaves a non-Unix path to the generic view", async () => {
  const tree = await mount(shown(prepareSync(single())));
  press(tree, "Profile");
  expect(text(tree)).toContain("/usr/local/bin/dumpcap · recorded path, not run by this view");
  expect(text(tree)).not.toMatch(/\b(Run|Start|Launch|Open|available|installed)\b/);
  expect(tree.root.findAllByType("button").map(it => it.children.join(""))).toEqual(["Status", "Profile"]);
  act(() => tree.unmount());
  // The provider's frozen profile admits only absolute Unix paths; anything else is not this contract.
  const windows = present({ prepared: prepareSync(single({ profile: profile({ backend: "C:\\Program Files\\Wireshark\\dumpcap.exe" }) })), facts: { state: "ready" }, context, registry: Registry.core() }).root;
  expect(windows.kind === "view" && windows.view === "capture-status").toBe(false);
});

it("waits on an empty window and shows the most recent status of a rolling window with bounded history", async () => {
  let tree = await mount(shown(prepareSync({ type: windowType, data: [] } as StoredValue)));
  expect(text(tree)).toContain("Waiting for a reported capture status");
  expect(text(tree)).toContain("Empty window · no profile or readiness is known yet");
  expect(tree.root.findAllByProps({ role: "tablist" })).toHaveLength(0);
  expect(text(tree)).not.toMatch(/edge-https|ready|packets/);
  act(() => tree.unmount());
  const preview = shown(prepareSync({ type: windowType, data: [] } as StoredValue), { mode: "preview", lines: 1 });
  expect(preview.ownLines).toBeLessThanOrEqual(1);

  tree = await mount(shown(prepareSync(rolling(3, { phase: "ended", containerComplete: true }))));
  expect(text(tree)).toContain("phase ended");
  expect(text(tree)).toContain("latest of 3 reported statuses");
  expect(text(tree)).toContain("30 packets");
  expect(text(tree)).toContain("Reported status · selected or restored values are not a live capture");
  press(tree, "History · 3");
  expect(tree.root.findAllByProps({ role: "tabpanel" })[0]!.findAllByType("button").map(it => it.children.join(""))).toEqual(["3", "2", "1"]);
  expect(text(tree)).toContain("Newest 3 of 3 reported statuses");
  act(() => tree.unmount());
});

it("uses local tabs and history selection with keyboard support and no actions or side effects", async () => {
  const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
  const url = vi.spyOn(URL, "createObjectURL");
  const tree = await mount(shown(prepareSync(rolling(4))));
  const focus = vi.fn(), preventDefault = vi.fn(), stopPropagation = vi.fn();
  const key = (name: string) => act(() => tree.root.findByProps({ role: "tablist" }).props.onKeyDown({ key: name, preventDefault, stopPropagation, currentTarget: { querySelectorAll: () => [{ focus }, { focus }, { focus }] } }));
  key("r"); expect(preventDefault).not.toHaveBeenCalled();
  key("ArrowRight");
  expect(tree.root.findByProps({ role: "tabpanel" }).props["aria-label"]).toBe("Profile");
  key("End");
  expect(tree.root.findByProps({ role: "tabpanel" }).props["aria-label"]).toBe("History · 4");
  expect(stopPropagation).toHaveBeenCalledTimes(2);
  press(tree, "2");
  expect(text(tree)).toContain("Status 2 · container complete · packet index complete · same profile");
  const labels = tree.root.findAllByType("button").map(it => it.children.join(""));
  expect(labels.every(label => /^\d+$/.test(label) || ["Status", "Profile", "History · 4"].includes(label))).toBe(true);
  expect(text(tree)).not.toMatch(/\b(Start|Stop|Keep|Export|Copy|Download|Annotate)\b/);
  expect(tree.root.findAllByType("iframe")).toHaveLength(0);
  expect(tree.root.findAllByType("a")).toHaveLength(0);
  expect(tree.root.findAllByType("input")).toHaveLength(0);
  expect(fetch).not.toHaveBeenCalled();
  expect(url).not.toHaveBeenCalled();
  act(() => tree.unmount());
});

it("drops a selected history row when the input is replaced or withdrawn, but not on resize", async () => {
  const prepared = prepareSync(rolling(4));
  const tree = await mount(shown(prepared));
  press(tree, "History · 4"); press(tree, "3");
  await act(async () => tree.update(draw(shown(prepared, { columns: 60 }))));
  expect(text(tree)).toContain("Status 3 · container");
  await act(async () => tree.update(draw(shown(prepareSync(rolling(5))))));
  expect(text(tree)).not.toContain("Status 3 · container");
  expect(text(tree)).toContain("Newest 5 of 5 reported statuses");
  expect(tree.root.findAllByProps({ "aria-pressed": true })).toHaveLength(0);
  await act(async () => tree.update(draw(shown(prepareSync({ type: windowType, data: [] } as StoredValue)))));
  expect(text(tree)).toContain("Waiting for a reported capture status");
  expect(text(tree)).not.toMatch(/Status 3|edge-https/);
  act(() => tree.unmount());
});

it("spends only the lines its host grants, in preview and window", async () => {
  for (const lines of [1, 2, 3, 4]) {
    const node = shown(prepareSync(single()), { mode: "preview", lines });
    expect(node.ownLines).toBeLessThanOrEqual(lines);
    const tree = await mount(node);
    expect(tree.root.findAllByType("p").length + 1).toBeLessThanOrEqual(Math.max(1, lines));
    expect(tree.root.findAllByProps({ role: "tablist" })).toHaveLength(0);
    act(() => tree.unmount());
  }
  const crowded = shown(prepareSync(single()), { lines: 3 });
  expect(crowded.ownLines).toBe(3);
  let tree = await mount(crowded);
  expect(tree.root.findAllByType("p")).toHaveLength(2);
  expect(tree.root.findAllByProps({ role: "tablist" })).toHaveLength(0);
  act(() => tree.unmount());

  const tight = shown(prepareSync(rolling(20)), { lines: 8 });
  expect(tight.ownLines).toBe(8);
  tree = await mount(tight);
  expect(tree.root.findByProps({ role: "tabpanel" }).findAllByType("p")).toHaveLength(5);
  press(tree, "Profile");
  // Six panel lines: five profile rows and the count of the five that need more height.
  let panel = tree.root.findByProps({ role: "tabpanel" });
  expect(panel.findAllByType("tr")).toHaveLength(5);
  expect(panel.findAllByType("p").map(it => it.children.join(""))).toEqual(["5 more rows need more height"]);
  press(tree, "History · 20");
  expect(tree.root.findAllByType("tr")).toHaveLength(1 + 4);
  expect(text(tree)).toContain("Newest 4 of 20 reported statuses");
  act(() => tree.unmount());

  // Four panel lines: three facts, then the hidden-fact count ahead of a short disclaimer kept whole in the title; the scope yields first.
  const squeezed = shown(prepareSync(single()), { lines: 6 });
  expect(squeezed.ownLines).toBe(6);
  tree = await mount(squeezed);
  panel = tree.root.findByProps({ role: "tabpanel" });
  const lines = panel.findAllByType("p");
  const facts = lines.map(it => it.children.join(""));
  expect(facts).toHaveLength(4);
  expect(facts[3]).toBe("2 facts hidden · status snapshot");
  expect(lines[3]!.props.title).toBe("Reported status · not proof of a live capture or of stored bytes");
  expect(text(tree)).not.toContain("Interface en0");
  expect(text(tree)).not.toContain("Reported status · not proof");
  act(() => tree.unmount());

  // Five panel lines: every fact fits, the disclaimer ahead of the scope, and nothing is reported hidden.
  tree = await mount(shown(prepareSync(single()), { lines: 7 }));
  panel = tree.root.findByProps({ role: "tabpanel" });
  expect(panel.findAllByType("p").map(it => it.children.join("")).slice(3)).toEqual(["Reported status · not proof of a live capture or of stored bytes", "Interface en0 · filter tcp port 443"]);
  expect(text(tree)).not.toContain("need more height");
  press(tree, "Profile");
  panel = tree.root.findByProps({ role: "tabpanel" });
  expect(panel.findAllByType("tr")).toHaveLength(4);
  expect(panel.findAllByType("p").map(it => it.children.join(""))).toEqual(["6 more rows need more height"]);
  act(() => tree.unmount());
});

it("keeps the hidden-fact count and the physical join requirement visible first at narrow widths", async () => {
  const disclaimer = "Reported status · not proof of a live capture or of stored bytes";
  const join = "Physical join required · :read bytes of the original recording · frozen snapshot · an ended phase is not proof";
  for (const columns of [30, 40]) {
    // Four panel lines: the drawn omission line, not only its title, carries the count within the column budget.
    let tree = await mount(shown(prepareSync(single()), { lines: 6, columns }));
    const omission = tree.root.findByProps({ role: "tabpanel" }).findAllByType("p")[3]!;
    const drawn = omission.children.join("");
    expect(drawn).toMatch(/^2 facts hidden · /);
    expect([...drawn].length).toBeLessThanOrEqual(columns);
    if (columns === 40) expect(drawn).toBe("2 facts hidden · status snapshot");
    expect(omission.props.title).toBe(disclaimer);
    act(() => tree.unmount());

    // Whatever phase is reported, the drawn Snapshot cell leads with the requirement; the title keeps every qualifier.
    for (const phase of ["ready", "ended"]) {
      tree = await mount(shown(prepareSync(single({ phase })), { columns }));
      press(tree, "Profile");
      const cell = tree.root.findAllByType("td").find(it => it.props.title === join)!;
      const visible = cell.children.join("");
      expect(visible).toMatch(/^Physical join required · /);
      expect([...visible].length).toBeLessThanOrEqual(columns);
      expect(visible).not.toMatch(/\b(joined|ready to read|can be read|readable now)\b/i);
      act(() => tree.unmount());
    }
  }
});
