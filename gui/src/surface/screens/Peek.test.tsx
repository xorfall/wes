import { failureText } from "../../failure-text";
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import type { StoredValue } from "../../protocol";
import { SourceView } from "../SourceView";
import { PeekScreen, peekOf, peekText } from "./Peek";

const orders: StoredValue = {
  type: { kind: "list", element: { kind: "record", name: "", fields: [{ name: "id", type: { kind: "primitive", name: "INT" } }] } },
  data: [{ id: 1 }, { id: 2 }],
  provenance: {},
};
const source = ":calc { return [{ id: 1 }, { id: 2 }]; } > orders";

function draw(what: "type" | "value" | "source" | "error", failure?: string): ReactTestRenderer {
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<PeekScreen top={[]} subject={[{ text: "$orders", role: "mono-ref" }]} what={what} value={orders} source={source} failure={failure} />); });
  return tree;
}

afterEach(() => vi.unstubAllGlobals());

describe("one piece of a result in a plain window", () => {
  it("should_ShowTheTypesStructureAndCopyTheSameText_When_TheTypeIsPeekedAt", async () => {
    // Arrange
    const writeText = vi.fn(() => Promise.resolve());
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    const tree = draw("type");
    // Act
    await act(async () => tree.root.findByProps({ "aria-label": "Copy to the clipboard" }).props.onClick());
    // Assert
    expect(tree.root.findByProps({ className: "inspection-text peek-text" }).children.join("")).toBe("List<{\n  id: Int\n}>");
    expect(writeText).toHaveBeenCalledWith("List<{\n  id: Int\n}>");
    expect(tree.root.findByProps({ "aria-label": "Copy to the clipboard" }).children.join("")).toBe("✓ copied");
    act(() => tree.unmount());
  });

  it("should_ShowTheWholeFailureAndCopyIt_When_TheErrorIsPeekedAt", async () => {
    // Arrange
    const writeText = vi.fn(() => Promise.resolve());
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    const failure = "CAL005: div divisor must not be zero";
    const tree = draw("error", failure);
    // Act
    await act(async () => tree.root.findByProps({ "aria-label": "Copy to the clipboard" }).props.onClick());
    // Assert
    expect(tree.root.findByProps({ className: "inspection-text peek-text" }).children.join("")).toBe(failure);
    expect(writeText).toHaveBeenCalledWith(failure);
    act(() => tree.unmount());
  });

  it("should_ShowTheFormattedSourceWithoutAHead_When_TheSourceIsPeekedAt", () => {
    // Arrange / Act
    const tree = draw("source");
    // Assert
    expect(tree.root.findByType(SourceView).props.source).toBe(":calc {\n  return [{\n    id: 1\n  }, {\n    id: 2\n  }];\n} > orders");
    expect(tree.root.findByType(SourceView).props.head).toBe(false);
    act(() => tree.unmount());
  });

  it("should_CopyTheValueAsJson_When_TheValueIsPeekedAt", () => {
    expect(peekText("value", orders)).toBe('[\n  {\n    "id": 1\n  },\n  {\n    "id": 2\n  }\n]');
    expect(peekText("type", undefined)).toBe("");
  });
});

describe("a failure with a saved describe report", () => {
  const reportId = "12345678-1234-1234-1234-123456789abc";
  const failureRecord = { id: "e", code: "DSC002", message: "Describe rejected", causeId: "", issues: [{ path: "/describeReport", code: "DSC_REPORT", message: reportId }] };
  const saved = { id: reportId, location: "https://docs.example.test", discoveredLocation: "https://docs.example.test", sourceDigest: "cd".repeat(32),
    report: { version: 1, message: "Blocked operation", omitted: 0, issues: [{ kind: "missing", operation: "GET /items", message: "auth is not documented", lines: [{ start: 4, end: 5 }] }] } };
  const failure = "DSC002 · Describe rejected";

  it("should_ReadTheReportOnOpening_And_CopyItWithTheFailure_When_TheErrorIsPeekedAt", async () => {
    // Arrange
    const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => saved });
    const writeText = vi.fn((_text: string) => Promise.resolve());
    vi.stubGlobal("fetch", fetch);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    let tree!: ReactTestRenderer;
    // Act
    await act(async () => { tree = create(<PeekScreen top={[]} subject={[{ text: "$spec", role: "mono-ref" }]} what="error" failure={failure} failureRecord={failureRecord} />); });
    // Assert: read at once, drawn as wrapped text beside the engine's own line
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(JSON.parse(fetch.mock.calls[0]![1].body)).toEqual({ action: "describeFailure", id: reportId });
    expect(tree.root.findByProps({ className: "inspection-text peek-text" }).children.join("")).toBe(failureText(failure, failureRecord));
    const drawn = JSON.stringify(tree.toJSON());
    expect(drawn).toContain("Blocked operation");
    expect(drawn).toContain("GET /items");
    expect(drawn).toContain("lines 4–5");
    expect(drawn).toContain("cd".repeat(32));
    // Act
    await act(async () => tree.root.findByProps({ "aria-label": "Copy to the clipboard" }).props.onClick());
    // Assert
    const copied = writeText.mock.calls[0]![0];
    expect(copied.startsWith(`${failureText(failure, failureRecord)}\n\nBlocked operation\n`)).toBe(true);
    expect(copied).toContain(`/describeReport · DSC_REPORT: ${reportId}`);
    expect(copied).toContain("missing · GET /items · lines 4–5\n  auth is not documented");
    act(() => tree.unmount());
  });

  it("should_CopyOnlyTheFailure_When_TheReportIsNotReadYet", async () => {
    // Arrange
    vi.stubGlobal("fetch", vi.fn(() => new Promise(() => undefined)));
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<PeekScreen top={[]} subject={[]} what="error" failure={failure} failureRecord={failureRecord} />); });
    // Assert
    expect(JSON.stringify(tree.toJSON())).toContain("Reading saved failure details");
    expect(peekText("error", undefined, undefined, failure, undefined)).toBe(failure);
    act(() => tree.unmount());
  });

  it("drops loaded details and clipboard content when a rerun clears the report reference", async () => {
    const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => saved });
    const writeText = vi.fn((_text: string) => Promise.resolve());
    vi.stubGlobal("fetch", fetch);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<PeekScreen top={[]} subject={[]} what="error" failure={failure} failureRecord={failureRecord} />); });
    expect(JSON.stringify(tree.toJSON())).toContain("Blocked operation");
    await act(async () => { tree.update(<PeekScreen top={[]} subject={[]} what="error" failure="New failure without a saved report" />); });
    expect(JSON.stringify(tree.toJSON())).not.toContain("Blocked operation");
    await act(async () => tree.root.findByProps({ "aria-label": "Copy to the clipboard" }).props.onClick());
    expect(writeText).toHaveBeenLastCalledWith("New failure without a saved report");
    expect(fetch).toHaveBeenCalledTimes(1);
    act(() => tree.unmount());
  });

  it("should_OfferRetry_When_TheReportCannotBeRead", async () => {
    // Arrange
    const fetch = vi.fn().mockResolvedValueOnce({ ok: false, text: async () => "Saved report unavailable" }).mockResolvedValueOnce({ ok: true, json: async () => saved });
    vi.stubGlobal("fetch", fetch);
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<PeekScreen top={[]} subject={[]} what="error" failure={failure} failureRecord={failureRecord} />); });
    expect(JSON.stringify(tree.toJSON())).toContain("Saved report unavailable");
    // Act
    await act(async () => { tree.root.findByProps({ children: "retry reading details" }).props.onClick(); });
    // Assert
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(JSON.stringify(tree.toJSON())).toContain("Blocked operation");
    act(() => tree.unmount());
  });

  it("should_NotReadAnything_When_TheFailureHasNoReport_Or_AnotherPieceIsPeekedAt", async () => {
    // Arrange
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);
    const legacy = { id: "e", code: "CAL005", message: "div divisor must not be zero", causeId: "", issues: [] };
    // Act
    const generic = draw("error", "CAL005: div divisor must not be zero");
    let described!: ReactTestRenderer;
    await act(async () => { described = create(<PeekScreen top={[]} subject={[]} what="error" failure="CAL005: div divisor must not be zero" failureRecord={legacy} />); });
    let sourced!: ReactTestRenderer;
    await act(async () => { sourced = create(<PeekScreen top={[]} subject={[]} what="source" source=":calc { return 1; }" failureRecord={failureRecord} />); });
    // Assert
    expect(fetch).not.toHaveBeenCalled();
    expect(generic.root.findByProps({ className: "inspection-text peek-text" }).children.join("")).toBe("CAL005: div divisor must not be zero");
    expect(described.root.findAllByProps({ className: "peek-describe-failure" })).toHaveLength(0);
    act(() => { generic.unmount(); described.unmount(); sourced.unmount(); });
  });
});

describe("the material of a peek", () => {
  it("should_CarryTheFailureRecord_When_TheEngineGaveOne_So_ThePeekCanReadItsReport", () => {
    // Arrange
    const failureRecord = { id: "e", code: "DSC002", message: "Describe rejected", causeId: "", issues: [{ path: "/describeReport", code: "DSC_REPORT", message: "12345678-1234-1234-1234-123456789abc" }] };
    const node = { id: "n3", command: ":describe file:synthetic.json provider:demo", dependsOn: [], state: "failed" as const, kept: false, provenance: {}, cautions: [], failure: "DSC002 · Describe rejected", failureRecord };
    // Act / Assert
    expect(peekOf(node, undefined)).toEqual({ source: node.command, failure: node.failure, failureRecord });
  });

  it("should_CarryTheFailureAndTheCommand_When_ANodeFailed_So_TheWindowAndTheScreenShowTheSame", () => {
    // Arrange: a failed node, as the workspace holds it (synthetic)
    const node = { id: "n1", command: ":calc { return 1; }", dependsOn: [], state: "failed" as const, kept: false, provenance: {}, cautions: [], failure: "CAL005: div divisor must not be zero" };
    // Act
    const material = peekOf(node, undefined);
    // Assert
    expect(material).toEqual({ source: node.command, failure: node.failure });
    expect(peekText("error", undefined, material.source, material.failure)).toBe(node.failure);
  });

  it("should_CarryTheValueAndNoFailure_When_ANodeIsReady", () => {
    // Arrange
    const node = { id: "n2", command: source, dependsOn: [], state: "ready" as const, kept: true, provenance: {}, cautions: [] };
    // Act / Assert
    expect(peekOf(node, orders)).toEqual({ value: orders, source });
    expect(peekOf(undefined, undefined)).toEqual({});
  });
});

it("shows and copies the engine stale explanation without presenting it as failure", async () => {
  const staleReason = { code: "restore_not_retained", message: "No retained result was available when the workspace reopened. The command was not rerun." };
  const node = { id: "spec", command: ":describe file:synthetic.json provider:demo", dependsOn: [], state: "stale" as const, kept: false, provenance: {}, cautions: [], staleReason };
  const material = peekOf(node, undefined);
  expect(material.staleReason).toBe(staleReason.message);
  expect(material.failure).toBeUndefined();
  const writeText = vi.fn(() => Promise.resolve());
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<PeekScreen top={[]} subject={[]} what="value" {...material} readStatus={<p>Unavailable</p>} />); });
  expect(tree.root.findByProps({ role: "status" }).children.join("")).toBe(`Stale: ${staleReason.message}`);
  await act(async () => tree.root.findByProps({ "aria-label": "Copy to the clipboard" }).props.onClick());
  expect(writeText).toHaveBeenCalledWith(`Stale: ${staleReason.message}`);
  act(() => tree.unmount());
});
