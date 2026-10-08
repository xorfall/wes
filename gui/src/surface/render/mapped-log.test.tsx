import { act, create } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { foldRows, isLogValue, logRows, LogView, type LogRow } from "./LogView";
import { ExactNumber } from "../../exact-json";
import { parseEntry, Registry } from "../../presentation/registry";
import type { StoredValue } from "../../protocol";

/** A CI log entry as a data-home file would declare it: identity, time, level, scope and flags by name. */
const ENTRY = `version: 1
type: CiLogLine
applies: [list]
fields:
  artifact: LogArtifactKey
  ordinal: Int
  step: Text
  time: Option<Instant>
  level: Option<Text>
  text: Text
  truncated: Bool
present:
  kind: log
  mapping:
    key: [artifact.digest, ordinal]
    text: text
    time: time
    level: level
    scope: step
    flags: [truncated]
`;

const some = (value: unknown) => ({ kind: "some", value });
const none = { kind: "none" };
const artifact = (digest: string) => ({ run: new ExactNumber("37326724446"), attempt: new ExactNumber("1"), job: "macOS", origin: "failed-jobs", digest });
const line = (digest: string, ordinal: number, step: string, text: string, extra: Record<string, unknown> = {}) => ({
  artifact: artifact(digest), ordinal: new ExactNumber(String(ordinal)), step,
  time: some("2026-10-05T15:01:20.268123456Z"), level: none, text, truncated: false, ...extra,
});
const type = { kind: "list", element: { kind: "record", name: "CiLogLine", fields: [
  { name: "artifact", type: { kind: "record", name: "LogArtifactKey", fields: [] } },
  { name: "ordinal", type: { kind: "primitive", name: "INT" } },
  { name: "step", type: { kind: "primitive", name: "TEXT" } },
  { name: "time", type: { kind: "option", element: { kind: "primitive", name: "INSTANT" } } },
  { name: "level", type: { kind: "option", element: { kind: "primitive", name: "TEXT" } } },
  { name: "text", type: { kind: "primitive", name: "TEXT" } },
  { name: "truncated", type: { kind: "primitive", name: "BOOL" } },
] } } as StoredValue["type"];
const value: StoredValue = { type, provenance: {}, data: [
  line("a1", 43, "Offline examples", "[.] python3 examples/view-packages/check.py"),
  line("a1", 44, "Offline examples", "usage: check.py [-h] --binary BINARY", { time: none }),
  line("a1", 45, "Offline examples", "check.py: error: the following arguments are required: --binary", { level: some("error") }),
  line("a1", 45, "Offline examples", "a duplicate key is one row"),
  line("a2", 45, "Require every selected job to succeed", "ValueError: engine-and-examples: expected success, got failure", { level: some("error"), truncated: true }),
  { artifact: artifact("a2"), ordinal: new ExactNumber("46"), step: "x", time: none, level: none },
] };

beforeEach(() => { vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} }); });
afterEach(() => { vi.unstubAllGlobals(); });

describe("a declared log mapping", () => {
  it("parses and is found only for the entry's list type", () => {
    const entry = parseEntry(ENTRY, "home:ci-log.yaml");
    expect(entry.kind).toBe("log");
    expect(entry.log).toEqual({ key: ["artifact.digest", "ordinal"], text: "text", time: "time", level: "level", scope: "step", flags: ["truncated"] });
    expect(parseEntry(ENTRY.replace("    scope: step", "    scope: step\n    group: step"), "home:ci-log.yaml").log?.group).toBe("step");
    const registry = Registry.core().withHome([{ name: "ci-log.yaml", text: ENTRY }]);
    expect(registry.logMapping(type, value.data)).toEqual(entry.log);
    expect(registry.logMapping(type.kind === "list" ? type.element : type, {})).toBeUndefined();
    expect(Registry.core().logMapping(type, value.data)).toBeUndefined();
  });

  it("refuses mappings that name nothing, guess, or point outside the declared fields", () => {
    const bad = (mapping: string, extra = "") => () => parseEntry(ENTRY.replace(/present:[\s\S]*$/, `present:\n  kind: log\n  mapping: ${mapping}\n${extra}`), "home:x.yaml");
    expect(bad("{key: ordinal}")).toThrow("mapping text must name a field");
    expect(bad("{key: [], text: text}")).toThrow("mapping key must name at least one field");
    expect(bad("{key: ordinal, text: text, colour: level}")).toThrow("unknown mapping role: colour");
    expect(bad("{key: ordinal, text: message}")).toThrow("field message is not declared in fields");
    expect(bad("{key: ordinal, text: text, timeUnit: days}")).toThrow("mapping timeUnit must be ns, us, ms or s");
    expect(() => parseEntry(ENTRY.replace("applies: [list]", "applies: [record]"), "home:x.yaml")).toThrow("a log entry applies to lists");
    expect(() => parseEntry(ENTRY.replace("kind: log", "kind: table"), "home:x.yaml")).toThrow("mapping is only for kind: log");
  });

  it("reads rows through the mapping: declared identity, Option time and level, flags; unreadable rows are dropped", () => {
    const mapping = parseEntry(ENTRY, "home:ci-log.yaml").log!;
    expect(isLogValue(value)).toBe(false);
    const rows = logRows(value, mapping);
    expect(rows.map((row) => row.key)).toEqual(['["a1","43"]', '["a1","44"]', '["a1","45"]', '["a2","45"]']);
    expect(rows.map((row) => row.time)).toEqual(["15:01:20.268", "—", "15:01:20.268", "15:01:20.268"]);
    expect(rows[0]!.exactTime).toBe("2026-10-05T15:01:20.268123456Z");
    expect(rows.map((row) => row.stream)).toEqual(["", "", "error", "error"]);
    expect(rows.map((row) => row.warn)).toEqual([false, false, true, true]);
    expect(rows[3]!.flags).toEqual(["truncated"]);
    expect(rows.map((row) => row.scope)).toEqual(["Offline examples", "Offline examples", "Offline examples", "Require every selected job to succeed"]);
    expect(logRows(value)).toEqual([]);
  });

  it("draws a scope separator where the scope changes, offers only the channels the rows carry, and counts unreadable and repeated rows apart", () => {
    const mapping = parseEntry(ENTRY, "home:ci-log.yaml").log!;
    let tree!: ReturnType<typeof create>;
    act(() => { tree = create(<LogView value={value} mode="expanded" mapping={mapping} />); });
    expect(tree.root.findAll((node) => node.props.role === "separator").map((node) => node.children.join(""))).toEqual(["Offline examples", "Require every selected job to succeed"]);
    expect(tree.root.findByProps({ "aria-label": "Log stream" }).findAllByType("option").map((option) => option.children.join(""))).toEqual(["all", "error"]);
    const notices = tree.root.findAll((node) => node.props.role === "status").map((node) => node.children.join(""));
    expect(notices).toContain("1 unreadable log event");
    expect(notices).toContain("1 log event repeats a key already shown");
    act(() => tree.unmount());
  });

  it("keeps a chosen channel selected, and its select available, after its rows leave the window", () => {
    const mapping = parseEntry(ENTRY, "home:ci-log.yaml").log!;
    let tree!: ReturnType<typeof create>;
    act(() => { tree = create(<LogView value={value} mode="expanded" mapping={mapping} />); });
    act(() => tree.root.findByProps({ "aria-label": "Log stream" }).props.onChange({ target: { value: "error" } }));
    const calm: StoredValue = { ...value, data: [line("a3", 1, "Offline examples", "all quiet")] };
    act(() => tree.update(<LogView value={calm} mode="expanded" mapping={mapping} />));
    const select = tree.root.findByProps({ "aria-label": "Log stream" });
    expect(select.props.value).toBe("error");
    expect(select.findAllByType("option").map((option) => option.children.join(""))).toEqual(["all", "error"]);
    act(() => tree.unmount());
  });

  it("leaves Docker's events on their own declared shape", () => {
    const docker: StoredValue = { type: { kind: "list", element: { kind: "record", name: "DockerLogEvent", fields: [] } }, provenance: {}, data: [
      { container: "c", sequence: 1, received_at_ns: new ExactNumber("1790955000123456789"), timestamp_ns: null, stream: "stdout", text: "hello", partial: false, lossy: false, line_truncated: false }] };
    const mapping = parseEntry(ENTRY, "home:ci-log.yaml").log!;
    expect(logRows(docker, mapping).map((row) => row.key)).toEqual(['["c","1"]']);
    expect(logRows(docker)[0]!.exactTime).toBe("1790955000123456789 ns");
    expect(logRows(value, { key: ["ordinal"], text: "text", time: "ordinal", timeUnit: "s", flags: [] })[0]!.exactTime).toBe("43000000000 ns");
  });

  it("folds a group to its first row unless it holds a warning, lets the reader toggle, and reads through folds when filtering", () => {
    const row = (key: string, group: string | undefined, warn = false): LogRow => ({ key, sequence: key, time: "—", received: false, stream: "", text: key, flags: [], warn, ...(group ? { group } : {}) });
    const rows = [row("1", undefined), row("2", "2"), row("3", "2"), row("4", "2"), row("5", "5"), row("6", "5", true), row("7", "7")];
    const none = new Map<string, boolean>();
    const folds = foldRows(rows, none, false);
    expect(folds.shown.map((r) => r.key)).toEqual(["1", "2", "5", "6", "7"]);
    expect(folds.hidden).toBe(2);
    expect([folds.folded("2"), folds.folded("5")]).toEqual([true, false]);
    expect(foldRows(rows, new Map([["2", false], ["5", true]]), false).shown.map((r) => r.key)).toEqual(["1", "2", "3", "4", "5", "7"]);
    expect(foldRows(rows, none, true).shown).toHaveLength(7);
  });

  it("toggles a group from its handle and counts what is folded", () => {
    const grouped: StoredValue = { ...value, type: { ...type, element: { ...(type as { element: object }).element, fields: [...((type as { element: { fields: unknown[] } }).element.fields), { name: "group", type: { kind: "option", element: { kind: "primitive", name: "INT" } } }] } } as StoredValue["type"],
      data: [43, 44, 45, 46].map((n) => line("a1", n, "Offline examples", `line ${n}`, { group: n === 43 ? none : some(new ExactNumber("44")) })) };
    const mapping = { ...parseEntry(ENTRY, "home:ci-log.yaml").log!, group: "group" };
    expect(logRows(grouped, mapping).map((r) => r.group)).toEqual([undefined, "44", "44", "44"]);
    let tree!: ReturnType<typeof create>;
    act(() => { tree = create(<LogView value={grouped} mode="expanded" mapping={mapping} />); });
    const texts = () => tree.root.findAll((node) => node.props.className === "log-message").map((node) => String(node.children[0]));
    expect(texts()).toEqual(["line 43", "line 44"]);
    // An ungrouped row keeps the fold slot, so sequences stay aligned with grouped rows.
    expect(tree.root.findAll((node) => node.type === "span" && node.props.className === "log-fold")).toHaveLength(1);
    expect(tree.root.findAll((node) => node.props.className === "mono-dim log-counts")[0]!.children.join("")).toContain("2 folded");
    const handle = tree.root.findByProps({ className: "log-fold cell-action" });
    expect(handle.props["aria-expanded"]).toBe(false);
    act(() => handle.props.onClick());
    expect(texts()).toEqual(["line 43", "line 44", "line 45", "line 46"]);
    act(() => tree.unmount());
  });
});
