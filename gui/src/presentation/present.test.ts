import { describe, expect, it } from "vitest";
import type { StoredValue, TypeShape } from "../protocol";
import { ellipsizeMiddle, width } from "./columns";
import { shortIdentifier } from "./format";
import { PreparedCache, prepare, prepareSync } from "./prepare";
import { linesOf, present, runText, tailOf } from "./present";
import { Registry } from "./registry";
import type { Context, PresentationNode } from "./types";

/* Synthetic fixtures only: invented containers, hosts and values, never a user's workspace. */

const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const INT: TypeShape = { kind: "primitive", name: "INT" };
const BOOL: TypeShape = { kind: "primitive", name: "BOOL" };
const BYTES: TypeShape = { kind: "primitive", name: "BYTES" };
const DECIMAL: TypeShape = { kind: "primitive", name: "DECIMAL" };
const opt = (element: TypeShape): TypeShape => ({ kind: "option", element });
const record = (name: string, fields: Record<string, TypeShape>): TypeShape =>
  ({ kind: "record", name, fields: Object.entries(fields).map(([field, type]) => ({ name: field, type })) });
const b64 = (text: string) => btoa(String.fromCharCode(...new TextEncoder().encode(text)));
const HEX = "0123456789abcdef".repeat(4);

const context = (over: Partial<Context> = {}): Context => ({
  mode: "preview", columns: 118, lines: 6, density: "normal", locale: "en-GB", timeZone: "UTC", ...over,
});
const core = Registry.core();
const show = (value: Pick<StoredValue, "type" | "data">, over: Partial<Context> = {}, registry = core, facts = {}) =>
  present({ prepared: prepareSync(value), context: context(over), registry, facts });

/** Every line the tree draws, as plain text, in order — the renderer's job reduced to text. */
function textOf(node: PresentationNode): string[] {
  switch (node.kind) {
    case "line": return [runText(node.runs)];
    case "fields": return node.rows.flatMap((row) => {
      if (row.name === "") return textOf(row.node);
      const inner = textOf(row.node);
      return [`${row.name.padEnd(node.nameWidth)}  ${inner[0] ?? ""}`, ...inner.slice(1)];
    });
    case "table": return [
      node.columns.map((column) => column.name.padEnd(column.width)).join("  ").trimEnd(),
      ...node.rows.map((row) => row.map((cell, at) => runText(cell).padEnd(node.columns[at]!.width)).join("  ").trimEnd()),
    ];
    case "items": return node.lines && node.lines.length > 1 ? node.lines.map((line) => runText(line)) : [runText(node.items)];
    case "text": return node.lines.map((line) => runText(line));
    case "bytes": return [`${node.size} B · ${node.note}`];
    case "empty": return [node.text];
    case "process": return [...textOf(node.stdout).map((line, at) => (at === 0 ? `stdout  ${line}` : line)), ...textOf(node.stderr).map((line, at) => (at === 0 ? `stderr  ${line}` : line))];
    case "view": return node.children.flatMap(textOf);
    case "nested": return [`${node.disclosure === "folds" ? "▾" : "▸"} ${node.summary}`, ...(node.body ? textOf(node.body) : [])];
    default: return [node.kind];
  }
}

const statsType = record("DockerStatsSample", {
  container: TEXT, sequence: INT, timestamp_ns: opt(INT), cpu_percent: opt(DECIMAL),
  memory_usage_bytes: opt(INT), memory_limit_bytes: opt(INT), memory_percent: opt(DECIMAL), online_cpus: opt(INT), cpu_unavailable: opt(TEXT),
});
const some = (value: unknown) => ({ kind: "some", value });
const none = { kind: "none" };
const samples = (count: number) => Array.from({ length: count }, (_, at) => ({
  container: HEX, sequence: at + 1, timestamp_ns: some(1_700_000_000_000_000_000 + at * 1_000_000_000), cpu_percent: some(at % 7 / 10),
  memory_usage_bytes: some(1_900_000 + at), memory_limit_bytes: some(67_108_864), memory_percent: some(2.9), online_cpus: some(4), cpu_unavailable: none,
}));
const statsValue = (count: number) => ({ type: { kind: "list", element: statsType } as TypeShape, data: samples(count) });

describe("present is pure and budgeted", () => {
  it("should_ReturnTheSameTree_When_CalledTwiceWithTheSameInputs", () => {
    // Arrange
    const value = statsValue(20);
    // Act
    const first = show(value);
    const second = show(value);
    // Assert
    expect(second).toEqual(first);
  });

  it("should_KeepTheTreeShapeAndChangeOnlyMore_When_ModeChanges", () => {
    // Arrange
    const value = statsValue(120);
    // Act
    const preview = show(value);
    const expanded = show(value, { mode: "expanded", lines: 400 });
    const window = show(value, { mode: "window", lines: 4000 });
    // Assert
    expect([preview.root.kind, expanded.root.kind, window.root.kind]).toEqual(["table", "table", "table"]);
    expect(preview.root.more?.rows).toBe(115);
    expect(expanded.root.more?.rows).toBe(70);
    expect(window.root.more?.rows).toBe(70);
    expect(preview.lines).toBeLessThanOrEqual(6);
  });

  it("should_PageByFifty_When_TheWindowShowsALongList", () => {
    // Arrange
    const value = statsValue(213);
    // Act
    const first = show(value, { mode: "window", lines: 4000 });
    const second = show(value, { mode: "window", lines: 4000, rows: 100 });
    // Assert
    expect(first.root.kind === "table" && first.root.rows.length).toBe(50);
    expect(second.root.kind === "table" && second.root.rows.length).toBe(100);
    expect(second.root.more?.rows).toBe(113);
  });

  it("should_GiveTheTableItsCellsAndMarkTheKeyAndNumericColumns_When_ARecordListIsPresented", () => {
    // Arrange
    const order = record("Order", { id: INT, customer: TEXT, total: DECIMAL });
    const orders = [{ id: 10431, customer: "Northwind Ltd", total: 248 }, { id: 10432, customer: "Contoso GmbH", total: 1940 }, { id: 10433, customer: "Acme Retail", total: 86.5 }];
    // Act
    const shown = show({ type: { kind: "list", element: order } as TypeShape, data: orders }, { mode: "expanded", lines: 400 });
    // Assert
    expect(shown.root.kind).toBe("table");
    if (shown.root.kind !== "table") return;
    expect(shown.root.rows).toHaveLength(3);
    expect(shown.root.rows[0]).toHaveLength(3);
    expect(shown.root.columns.map((column) => [column.name, column.key, column.numeric])).toEqual([["id", true, true], ["customer", false, false], ["total", false, true]]);
    expect(runText(shown.root.rows[1]![1]!)).toBe("Contoso GmbH");
    expect(shown.root.rows.every((row) => row.every((cell) => !runText(cell).endsWith(" ")))).toBe(true);
  });

  it("should_LeaveATablesRowsToTheTableItself_When_TheTailIsAsked", () => {
    // Arrange
    const expanded = show(statsValue(120), { mode: "expanded", lines: 400, columns: 66 });
    // Act
    const whole = runText(tailOf(expanded));
    const paged = runText(tailOf(expanded, true));
    // Assert: the table draws `+70 rows · show 20 more` under itself; the tail never repeats it.
    expect(expanded.root.more?.rows).toBe(70);
    expect(whole).not.toContain("rows");
    expect(paged).not.toContain("rows");
    expect(paged).not.toContain("not shown:");
  });

  it("should_ShowTheLastWindow_When_TheStreamWasStopped", () => {
    // Arrange / Act
    const stopped = show(statsValue(213), {}, core, { stopped: true });
    // Assert
    expect(stopped.root.kind === "table" && stopped.root.offset).toBe(208);
    expect(textOf(stopped.root).at(-1)).toMatch(/\s213\s/);
  });

  it("should_KeepRowsFieldsAndColumnsApart_When_CountingWhatWasLeftOut", () => {
    // Arrange / Act
    const table = show(statsValue(40), { columns: 66 });
    // Assert
    expect(table.root.more?.rows).toBe(35);
    expect(table.root.more?.columns).toBeUndefined();
    expect(table.root.more?.fields).toBeUndefined();
  });

  it("should_NotStateATotal_When_OnlyAWindowOfTheValueWasRead", () => {
    // Arrange / Act
    const partial = show(statsValue(40), {}, core, { whole: false });
    // Assert
    expect(partial.root.more?.exact).toBe(false);
  });
});

describe("value rules", () => {
  it("should_ReadExitOneAndStderr_When_AProcessOutputIsPresented", () => {
    // Arrange
    const value = { type: record("ProcessOutput", { exitCode: INT, stdout: BYTES, stderr: BYTES }), data: { exitCode: 1, stdout: "", stderr: b64("cat: /nowhere: No such file\n") } };
    // Act
    const shown = show(value);
    // Assert
    expect(shown.root.kind).toBe("process");
    expect(runText(shown.summary.facts)).toBe("exit 1");
    expect(shown.summary.facts[0]?.tone).toBe("warn");
    expect(textOf(shown.root)).toEqual(["stdout  empty", "stderr  cat: /nowhere: No such file"]);
    expect(shown.root.kind === "process" && shown.root.stderr.kind === "text" && shown.root.stderr.newline).toBe(true);
    expect(shown.notices).toContain("Bytes read as utf-8");
  });

  it("should_DrawOneRowWithAReturnMark_When_ATextEndsInANewline", () => {
    // Arrange
    const value = { type: record("", { version: TEXT }), data: { version: "3.22.6\n" } };
    // Act
    const shown = show(value);
    // Assert
    expect(shown.root.kind).toBe("fields");
    expect(textOf(shown.root)).toEqual(["version  3.22.6 ⏎"]);
    expect(shown.lines).toBe(1);
  });

  it("should_PackScalarsAndNestTheRows_When_ARecordHoldsATable", () => {
    // Arrange
    const line = record("DockerLogLine", { stream: TEXT, timestamp_ns: opt(INT), text: TEXT });
    const tail = record("DockerLogTail", { container: TEXT, requested_tail: INT, returned: INT, truncated: BOOL, rows: { kind: "list", element: line } });
    const value = { type: tail, data: { container: HEX, requested_tail: 10, returned: 2, truncated: false,
      rows: [{ stream: "stderr", timestamp_ns: some(1_700_000_000_123_000_000), text: "applet not found" }, { stream: "stdout", timestamp_ns: some(1_700_000_000_124_000_000), text: "fixture started" }] } };
    // Act
    const lines = textOf(show(value).root);
    // Assert
    expect(lines[0]).toBe(`container ${HEX.slice(0, 8)}…${HEX.slice(-6)}   requested_tail 10   returned 2   truncated false`);
    expect(lines[1]).toMatch(/^rows\s+▾ List<DockerLogLine> · 2$/);
    expect(lines.slice(2).join("\n")).toContain("stream  timestamp_ns");
    expect(lines.join("\n")).toContain("1700000000123000000");
  });

  it("should_KeepNanosecondsARawNumber_When_NoTypeOrEntryDeclaresAUnit", () => {
    // Arrange
    const value = { type: record("Timing", { duration_ns: INT, timestamp_ns: INT }), data: { duration_ns: 5_000_000, timestamp_ns: 1_700_000_000_000_000_000 } };
    // Act
    const lines = textOf(show(value).root);
    // Assert
    expect(lines).toEqual(["duration_ns   5000000", "timestamp_ns  1700000000000000000"]);
  });

  it("should_ShortenIdentifiersInPreviewAndShowThemWhole_When_Expanded", () => {
    // Arrange
    const value = { type: record("Receipt", { container: TEXT }), data: { container: HEX } };
    // Act / Assert
    expect(textOf(show(value).root)).toEqual([`container  ${HEX.slice(0, 8)}…${HEX.slice(-6)}`]);
    expect(textOf(show(value, { mode: "expanded", lines: 400 }).root)).toEqual([`container  ${HEX}`]);
  });

  it("should_KeepEveryColumnInTheTypesOrder_When_TheBlockIsNarrow", () => {
    // Arrange / Act
    const narrow = show(statsValue(3), { columns: 66 });
    const wide = show(statsValue(3), { columns: 200 });
    const declared = statsType.kind === "record" ? statsType.fields.map((field) => field.name) : [];
    // Assert: width never drops a column; the renderer scrolls the table instead
    expect(narrow.root.kind === "table" && narrow.root.columns.map((c) => c.name)).toEqual(declared);
    expect(wide.root.kind === "table" && wide.root.columns.map((c) => c.name)).toEqual(declared);
    expect(narrow.root.more?.columns).toBeUndefined();
    expect(narrow.root.kind === "table" && narrow.root.columns[0]?.key).toBe(true);
  });

  it("should_CutACellAtFortyColumnsInACell_And_AtTwoHundredInTheWindow", () => {
    // Arrange
    const long = "x".repeat(300);
    const value = { type: { kind: "list", element: record("Line", { id: INT, text: TEXT }) } as TypeShape, data: [{ id: 1, text: long }] };
    // Act
    const inCell = show(value, { mode: "expanded", lines: 40, columns: 80 });
    const inWindow = show(value, { mode: "window", lines: 40, columns: 80 });
    // Assert
    expect(inCell.root.kind === "table" && inCell.root.columns[1]?.width).toBe(40);
    expect(inWindow.root.kind === "table" && inWindow.root.columns[1]?.width).toBe(200);
  });

  it("should_UnwrapOptions_When_SomeAndNoneArePresented", () => {
    // Arrange
    const value = { type: record("", { a: opt(INT), b: opt(TEXT) }), data: { a: some(7), b: none } };
    // Act / Assert
    expect(textOf(show(value).root)).toEqual(["a  7", "b  none"]);
  });

  it("should_SayEmptyAndNoItems_When_TextAndListsAreEmpty", () => {
    expect(textOf(show({ type: TEXT, data: "" }).root)).toEqual(["empty"]);
    expect(textOf(show({ type: { kind: "list", element: INT }, data: [] }).root)).toEqual(["no items"]);
  });

  it("should_KeepNumericListsAsItems", () => {
    // Arrange / Act
    const shown = show({ type: { kind: "list", element: INT }, data: [3, 1, 4, 1, 5] });
    // Assert
    expect(shown.root.kind).toBe("items");
    expect(shown.offers).not.toContain("chart");
  });

  it("should_PackScalarItemsOnOneLine_When_Previewing_And_ListThemPerLine_When_Expanded", () => {
    // Arrange
    const value = { type: { kind: "list", element: TEXT } as TypeShape, data: ["agent-docker-stress-20260925-k8m2", "default", "qa-alpine", "scratch"] };
    // Act
    const preview = show(value, { columns: 60 });
    const expanded = show(value, { mode: "expanded", lines: 40 });
    const paged = show(value, { mode: "expanded", lines: 40, rows: 2 });
    // Assert
    expect(preview.root.kind).toBe("items");
    expect(textOf(preview.root)).toEqual(["agent-docker-stress-20260925-k8m2 · default"]);
    expect(preview.root.more).toEqual({ items: 2, exact: true });
    expect(textOf(expanded.root)).toEqual(["agent-docker-stress-20260925-k8m2", "default", "qa-alpine", "scratch"]);
    expect(expanded.root.more).toBeUndefined();
    expect(textOf(paged.root)).toEqual(["agent-docker-stress-20260925-k8m2", "default"]);
    expect(paged.root.more).toEqual({ items: 2, exact: true });
  });

  it("should_FoldANestedTableToItsSummary_When_NoRowFitsThePreview_And_CountItsRowsOnce", () => {
    // Arrange: a record whose list gets the header line but no row under a three-line budget
    const line: TypeShape = { kind: "record", name: "DockerLogLine", fields: [{ name: "stream", type: TEXT }, { name: "text", type: TEXT }] };
    const type: TypeShape = { kind: "record", name: "DockerLogTail", fields: [{ name: "container", type: TEXT }, { name: "returned", type: INT }, { name: "rows", type: { kind: "list", element: line } }] };
    const rows = Array.from({ length: 10 }, (_, at) => ({ stream: "stderr", text: `line ${at}` }));
    const value = { type, data: { container: HEX, returned: 10, rows } };
    // Act
    const tight = show(value, { lines: 3 });
    const roomy = show(value, { lines: 8 });
    // Assert
    const folded = tight.root.kind === "fields" ? tight.root.rows.map((row) => row.node).find((node) => node.kind === "nested" && node.body === undefined) : undefined;
    expect(folded).toBeDefined();
    expect(folded?.disclosure).toBe("opens");
    expect(tailOf(tight).map((run) => run.text).join("")).toContain("+10 rows");
    expect(tailOf(tight).map((run) => run.text).join("")).not.toContain("+20");
    expect(textOf(roomy.root).some((text) => text.includes("line 0"))).toBe(true);
    // An opened nested table counts its own left-out rows under itself, not in the tail.
    const nested = roomy.root.kind === "fields" ? roomy.root.rows.map((row) => row.node).find((node) => node.kind === "nested" && node.body?.kind === "table") : undefined;
    expect(nested?.kind === "nested" && nested.body?.more?.rows).toBeGreaterThan(0);
    expect(tailOf(roomy).map((run) => run.text).join("")).not.toContain("rows");
  });

  it("should_CutALongScalarWithAnOpener_When_Previewing_And_WrapItWhole_When_ExpandedOrOpened", () => {
    // Arrange: a record whose one field is a long single-line type expression
    const expression = "{ container: Text, image: Text, logs: List<{ stream: Text, timestamp_ns: Int, text: Text }>, truncated: Bool }";
    const value = { type: { kind: "unknown" } as TypeShape, data: { id: "id1007", type: expression } };
    // Act
    const preview = show(value, { columns: 60 });
    const expanded = show(value, { mode: "expanded", columns: 60, lines: 40 });
    const opened = show(value, { columns: 60, open: new Set(["/type"]) });
    const folded = show(value, { mode: "expanded", columns: 60, lines: 40, closed: new Set(["/type"]) });
    const row = (shown: ReturnType<typeof show>) => (shown.root.kind === "fields" ? shown.root.rows.find((it) => it.name === "type")?.node : undefined);
    // Assert: one line with the columns it lost in preview; a foldable block otherwise
    expect(row(preview)?.kind).toBe("line");
    expect(row(preview)?.more?.chars).toBeGreaterThan(0);
    expect(row(expanded)?.kind).toBe("text");
    expect(row(expanded)?.disclosure).toBe("folds");
    expect(textOf(expanded.root).join(" ").replace(/\s+/g, " ")).toContain("truncated: Bool }");
    expect(row(opened)?.kind).toBe("text");
    expect(row(folded)?.kind).toBe("line");
  });

  it("should_LeaveRoomForTheOpener_When_AScalarIsCutOrWrapped", () => {
    // Arrange: a field 60 columns wide in a 60-column block whose name takes 6 of them
    const expression = "{ container: Text, image: Text, logs: List<{ stream: Text, timestamp_ns: Int, text: Text }>, truncated: Bool }";
    const value = { type: { kind: "unknown" } as TypeShape, data: { id: "id1007", type: expression } };
    const valueColumns = 60 - "type".length - 2;
    const lineWidth = (node: PresentationNode | undefined) => (node?.kind === "line" ? width(runText(node.runs)) : node?.kind === "text" ? Math.max(...node.lines.map((line) => width(runText(line)))) : -1);
    // Act
    const cut = show(value, { columns: 60 });
    const wrapped = show(value, { mode: "expanded", columns: 60, lines: 40 });
    const row = (shown: ReturnType<typeof show>) => (shown.root.kind === "fields" ? shown.root.rows.find((it) => it.name === "type")?.node : undefined);
    // Assert: the ▸ or ▾ the renderer draws takes two columns, so the value is two columns narrower
    expect(lineWidth(row(cut))).toBeLessThanOrEqual(valueColumns - 2);
    expect(lineWidth(row(cut))).toBeGreaterThan(valueColumns - 6);
    expect(row(cut)?.more?.chars).toBe(width(expression) - (valueColumns - 2));
    expect(lineWidth(row(wrapped))).toBeLessThanOrEqual(valueColumns - 2);
    expect(lineWidth(row(wrapped))).toBeGreaterThan(valueColumns - 6);
  });

  it("should_ShowAHandOpenedValueWhole_When_Previewing_And_LeaveTheRestOfThePreviewAsItWas", () => {
    // Arrange: a preview of three lines whose type field needs more than that on its own
    const expression = "{ container: Text, image: Text, logs: List<{ stream: Text, timestamp_ns: Int, text: Text }>, truncated: Bool }";
    const value = { type: { kind: "unknown" } as TypeShape, data: { id: "id1007", container: HEX, type: expression, names: ["a", "b"] } };
    // Act
    const before = show(value, { columns: 60, lines: 3 });
    const opened = show(value, { columns: 60, lines: 3, open: new Set(["/type"]) });
    const row = (shown: ReturnType<typeof show>) => (shown.root.kind === "fields" ? shown.root.rows.find((it) => it.name === "type")?.node : undefined);
    const others = (shown: ReturnType<typeof show>) => (shown.root.kind === "fields" ? shown.root.rows.filter((it) => it.name !== "type").map((it) => [it.name, textOf(it.node)]) : []);
    // Assert: the type is whole, in preview form; the identifier stays short and the packed line is the same
    expect(row(opened)?.kind).toBe("text");
    const block = row(opened);
    expect(block?.kind === "text" ? block.lines.map(runText).join("").replace(/\s+/g, " ") : "").toContain("truncated: Bool }");
    expect(block?.more).toBeUndefined();
    expect(others(opened)).toEqual(others(before));
    expect(textOf(opened.root).join(" ")).toContain(shortIdentifier(HEX));
  });

  it("should_PresentADescribedType_AsOneLineOrOpenedFieldByField_WhereverItAppears", () => {
    // Arrange: what :inspect carries for a value's type — the wire's own shape, as data
    const described = { kind: "record", name: "", fields: [
      { name: "container", type: { kind: "primitive", name: "TEXT" } },
      { name: "logs", type: { kind: "list", element: { kind: "record", name: "DockerLogLine", fields: [{ name: "stream", type: { kind: "primitive", name: "TEXT" } }, { name: "text", type: { kind: "primitive", name: "TEXT" } }] } } } ] };
    const value = { type: { kind: "unknown" } as TypeShape, data: { available: true, node: "id1007", type: described } };
    // Act
    const preview = show(value, { columns: 110 });
    const opened = show(value, { mode: "window", columns: 80, lines: 200 });
    // Assert: one line where it fits (no field-name rule involved), the structure where it is shown whole
    expect(textOf(preview.root).some((line) => line.includes("{ container: Text, logs: List<DockerLogLine> }"))).toBe(true);
    const row = preview.root.kind === "fields" ? preview.root.rows.find((it) => it.name === "type")?.node : undefined;
    expect(row?.more?.lines).toBeGreaterThan(0);
    const lines = textOf(opened.root);
    expect(lines.some((line) => /^type\s+\{$/.test(line))).toBe(true);
    expect(lines.some((line) => line.trim() === "container: Text")).toBe(true);
    expect(lines.some((line) => line.trim() === "logs: List<DockerLogLine {")).toBe(true);
    expect(lines.some((line) => line.trim() === "stream: Text")).toBe(true);
  });

  it("should_CollapseDeeperRecords_When_TheBudgetOrDepthRunsOut", () => {
    // Arrange
    const host = { name: "qa-host", cpu: 4 };
    const value = { type: { kind: "unknown" } as TypeShape, data: { cluster: { name: "qa", region: { name: "west", zone: { name: "west-1", rack: { name: "r1", host } } } } } };
    // Act
    const preview = textOf(show(value).root);
    const expanded = textOf(show(value, { mode: "expanded", lines: 400 }).root);
    // Assert
    expect(preview.some((line) => /^zone\s+▸ /.test(line))).toBe(true);
    expect(expanded.some((line) => /^rack\s+▸ /.test(line))).toBe(true);
    expect(expanded.some((line) => /^zone\s+▾ /.test(line))).toBe(true);
  });
});

describe("http", () => {
  const headers = { kind: "list", element: record("HttpHeader", { name: TEXT, value: TEXT }) } as TypeShape;
  const httpType = record("HttpResponse", { status: INT, version: TEXT, headers, body: BYTES });
  const response = (body: string, extra: { name: string; value: string }[] = []) => ({
    type: httpType,
    data: { status: 200, version: "HTTP/1.1", headers: [{ name: "content-type", value: "application/json" }, ...extra], body },
  });

  it("should_PresentAJsonBodyAsFields_When_TheBodyParses", () => {
    // Arrange / Act
    const shown = show(response(b64('{"ok":true,"value":7}')));
    // Assert
    expect(shown.root.kind).toBe("view");
    expect(shown.root.kind === "view" && shown.root.children[0]!.kind).toBe("fields");
    expect(shown.root.kind === "view" && shown.root.view).toBe("http");
    expect(runText(shown.summary.facts)).toBe("200");
    expect(shown.offers).toContain("http");
  });

  it("should_TakeTheStructuralRule_When_AnHttpResponseHasOtherFields", () => {
    // Arrange
    const odd = { type: record("HttpResponse", { code: INT, text: TEXT }), data: { code: 200, text: "fine" } };
    // Act / Assert
    expect(show(odd).root.kind).toBe("fields");
  });

  it("should_DecodeGzipOnceAndNotAgain_When_TheContextChanges", async () => {
    // Arrange
    const stream = new Blob([new TextEncoder().encode('{"items":[1,2,3]}')]).stream().pipeThrough(new CompressionStream("gzip"));
    const gz = new Uint8Array(await new Response(stream).arrayBuffer());
    const value: StoredValue = { ...response(btoa(String.fromCharCode(...gz)), [{ name: "content-encoding", value: "gzip" }]), provenance: {} };
    const cache = new PreparedCache();
    let landed = 0;
    // Act
    const first = cache.read("gen:h1", value, () => { landed += 1; });
    await new Promise((resolve) => setTimeout(resolve, 50));
    const settled = cache.read("gen:h1", value);
    const again = cache.read("gen:h1", value);
    // Assert
    expect(first.pending).toBe(true);
    expect(landed).toBe(1);
    expect(settled).toBe(again);
    const narrow = present({ prepared: settled, context: context({ columns: 40 }), registry: core });
    const wide = present({ prepared: settled, context: context({ columns: 200 }), registry: core });
    expect(narrow.root.kind === "view" && narrow.root.children[0]!.kind).not.toBe("line");
    expect(wide.root.kind).toBe("view");
    expect(landed).toBe(1);
  });

  it("should_ExplainAndKeepTheStoredBytes_When_TheBodyIsNotText", async () => {
    // Arrange
    const value = { ...response(btoa("\u0000\u0001\u0002binary"), []), provenance: {} };
    (value.data.headers as { name: string; value: string }[])[0] = { name: "content-type", value: "application/octet-stream" };
    // Act
    const prepared = await prepare(value);
    const shown = present({ prepared, context: context(), registry: core });
    // Assert
    expect(shown.root.kind === "view" && shown.root.children[0]!.kind).toBe("bytes");
    expect((value.data as { body: string }).body).toBe(btoa("\u0000\u0001\u0002binary"));
  });
});

describe("drawing descriptions", () => {
  it("keeps retired wrappers as structural fields instead of discarding their outer data", () => {
    const shown = show({ type: { kind: "unknown" }, data: { view: "custom", content: { view: "text", text: "inside" }, state: "outside" } });
    expect(shown.root.kind).toBe("fields");
    expect(JSON.stringify(shown.root)).toContain("outside");
  });

  it("leaves untyped legacy drawings as ordinary structural data", () => {
    const shown = show({ type: { kind: "unknown" }, data: { view: "histogram", bins: [{ lower: "0", upper: "1", count: 3 }], total: 3 } });
    expect(shown.root.kind).toBe("fields");
  });

  it("should_FallBackHonestly_When_TheDrawingNameIsUnknown", () => {
    const shown = show({ type: { kind: "unknown" }, data: { view: "sankey", flows: 3 } });
    expect(shown.root.kind).toBe("fields");
  });
});

describe("columns", () => {
  it("should_CountDisplayColumns_When_TextHoldsWideCharacters", () => {
    expect(width("東京")).toBe(4);
    expect(width("🚀")).toBe(2);
    expect(width("é")).toBe(1);
    expect(width("Çağrı")).toBe(5);
  });

  it("should_KeepBothEnds_When_ANameIsEllipsizedInTheMiddle", () => {
    const one = ellipsizeMiddle("$very_long_result_name_number_one", 14);
    const two = ellipsizeMiddle("$very_long_result_name_number_two", 14);
    expect(one).not.toBe(two);
    expect(width(one)).toBe(14);
  });
});

describe("lines", () => {
  it("should_CountTheLinesTheRendererDraws_When_ATreeIsBuilt", () => {
    const shown = show(statsValue(3));
    expect(linesOf(shown.root)).toBe(textOf(shown.root).length);
  });
});

describe("structural table cell disclosure", () => {
  const value = { type: { kind: "unknown" } as TypeShape, data: [
    { id: 1, options: [{ schemes: ["key", "secret"] }, { schemes: ["basic"] }], "a/b~": { label: "escaped" }, empty: [], none: { kind: "none" } },
    { id: 2, options: [{ schemes: ["other"] }] },
  ] };
  const table = (over: Partial<Context> = {}, facts = {}) => {
    const root = show(value, { mode: "window", lines: 100, ...over }, core, facts).root;
    if (root.kind !== "table") throw new Error("expected table");
    return root;
  };
  it("keeps structural cells folded in every mode and gives rows/escaped keys distinct paths", () => {
    for (const mode of ["preview", "expanded", "window"] as const) {
      const root = table({ mode });
      expect(root.details?.[0]?.[0]).toBeUndefined();
      expect(root.details?.[0]?.[1]).toMatchObject({ kind: "nested", path: "/0/options", disclosure: "opens" });
      expect(root.details?.[1]?.[1]?.path).toBe("/1/options");
      expect(root.details?.[0]?.[2]?.path).toBe("/0/a~1b~0");
      expect(root.details?.[0]?.[3]).toBeUndefined();
      expect(root.details?.[0]?.[4]).toBeUndefined();
    }
  });
  it("opens arbitrary-depth cells independently and can fold them again", () => {
    const root = table({ open: new Set(["/0/options", "/0/options/0/schemes"]) });
    const detail = root.details![0]![1]!;
    if (detail.kind !== "nested" || detail.body?.kind !== "table") throw new Error("expected nested table");
    const schemes = detail.body.details![0]![0]!;
    expect(schemes).toMatchObject({ disclosure: "folds", body: { kind: "items", lines: [[{ text: "key", tone: "literal" }], [{ text: "secret", tone: "literal" }]] } });
    expect(root.details![1]![1]!.disclosure).toBe("opens");
    expect(table({ open: new Set(["/0/options"]), closed: new Set(["/0/options"]) }).details![0]![1]!.disclosure).toBe("opens");
  });
  it("pages large nested lists without expanding or paging siblings", () => {
    const prepared = prepareSync({ type: { kind: "unknown" }, data: [{ entries: Array.from({ length: 125 }, (_, i) => ({ id: i })), sibling: ["hidden"] }] });
    const result = present({ prepared, registry: core, context: context({ open: new Set(["/0/entries"]), pages: new Map([["/0/entries", 2]]) }) });
    if (result.root.kind !== "table") throw new Error("table");
    const detail = result.root.details![0]![0]!;
    if (detail.kind !== "nested" || detail.body?.kind !== "table") throw new Error("nested table");
    expect(detail.body.pagination).toEqual({ offset: 100, shown: 25, total: 125 });
    expect(runText(detail.body.rows[0]![0]!)).toBe("100");
    expect(result.root.details![0]![1]!.disclosure).toBe("opens");
  });
  it("discovers fields in the visible nested page rather than only the first page", () => {
    const prepared = prepareSync({ type: { kind: "unknown" }, data: [{ rows: [...Array.from({ length: 50 }, () => ({ early: true })), { late: "visible" }] }] });
    const result = present({ prepared, registry: core, context: context({ open: new Set(["/0/rows"]), pages: new Map([["/0/rows", 1]]) }) });
    if (result.root.kind !== "table") throw new Error("table");
    const detail = result.root.details![0]![0]!;
    if (detail.kind !== "nested" || detail.body?.kind !== "table") throw new Error("nested table");
    expect(detail.body.columns.map(column => column.name)).toEqual(["late"]);
    expect(runText(detail.body.rows[0]![0]!)).toBe("visible");
  });
  it("uses source row indexes in a stopped stream window", () => {
    const root = table({ mode: "preview", lines: 2 }, { stopped: true });
    expect(root.offset).toBe(1);
    expect(root.more?.rows).toBe(1);
    expect(root.details![0]![1]!.path).toBe("/1/options");
  });
});

describe("tables arranged by hand", () => {
  const orders = record("Order", { id: TEXT, customer: TEXT, status: TEXT, note: TEXT });
  const rows = Array.from({ length: 60 }, (_, at) => ({ id: `ord_${at}`, customer: at % 10 === 3 ? "Mert Yılmaz" : `Customer ${at}`, status: "open", note: "x".repeat(60) }));
  const value = { type: { kind: "list", element: orders } as TypeShape, data: rows };
  const table = (over: Partial<Context>) => {
    const root = show(value, { mode: "expanded", lines: 400, ...over }).root;
    if (root.kind !== "table") throw new Error("table");
    return root;
  };

  it("should_MatchEveryRowBeforePaging_When_AFilterIsGiven", () => {
    // Act
    const root = table({ filters: new Map([["", "YILMAZ"]]) });
    // Assert
    expect(root.total).toBe(6);
    expect(root.rows.map((row) => runText(row[0]!))).toEqual(["ord_3", "ord_13", "ord_23", "ord_33", "ord_43", "ord_53"]);
    expect(root.arrangement.filter).toEqual({ query: "YILMAZ", matched: 6, of: 60 });
  });

  it("should_ShowMoreRowsOfThatTable_When_RowsWereAskedForUnderIt", () => {
    // Arrange
    const tight = show(value, { mode: "preview", lines: 6 }).root;
    // Act
    const asked = show(value, { mode: "preview", lines: 6, shown: new Map([["", 25]]) }).root;
    // Assert
    expect(tight.kind === "table" && tight.rows.length).toBeLessThan(6);
    expect(asked.kind === "table" && asked.rows.length).toBe(25);
    expect(asked.more?.rows).toBe(35);
    expect(asked.kind === "table" && asked.arrangement.step).toBe(20);
    expect(table({ mode: "window" }).arrangement.step).toBe(50);
  });

  it("should_DropHiddenColumnsButNeverTheKey_When_TheTypeWasArranged", () => {
    // Act
    const root = table({ tables: { "type:Order": { hidden: ["id", "note"], widths: { customer: 6 } } } });
    // Assert
    expect(root.columns.map((column) => column.name)).toEqual(["id", "customer", "status"]);
    expect(root.arrangement).toMatchObject({ key: "type:Order", hidden: ["note"], columns: ["id", "customer", "status", "note"] });
    const customer = root.columns[1]!;
    expect(customer).toMatchObject({ width: 6, sized: true, label: "custo…" });
    expect(runText(root.rows[3]![1]!)).toBe("Mert …");
  });
});
it("shows mixed nested lists structurally without inventing a contract and respects the line budget",()=>{
  const data=[1,["alpha","beta"],{status:503},[]];
  const prepared=prepareSync({type:{kind:"list",element:{kind:"unknown"}},data});
  const output=present({prepared,context:context({mode:"window",lines:30}),registry:Registry.core()});
  const shown=JSON.stringify(output.root);expect(shown).toContain("alpha");expect(shown).toContain("beta");expect(shown).toContain("status");expect(shown).toContain("no items");
  const small=present({prepared,context:context({lines:2}),registry:Registry.core()});
  expect(linesOf(small.root)).toBeLessThanOrEqual(2);expect(JSON.stringify(small.root)).toContain("more");
});
