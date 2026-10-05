import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DescribeFailureContent, DescribeFailureDetails, describeFailureText, describeLineRanges, describeReportId, type DescribeFailureReport } from "./DescribeFailureDetails";

/* Synthetic reports only: invented locations and findings, never a user's. */

afterEach(() => vi.unstubAllGlobals());
const id = "12345678-1234-1234-1234-123456789abc";
const other = "abcdef12-1234-1234-1234-123456789abc";

const report = (over: Partial<DescribeFailureReport> = {}): DescribeFailureReport => ({
  id, location: "https://docs.example.test", discoveredLocation: "https://docs.example.test/guide", sourceDigest: "ab".repeat(32),
  report: { version: 1, message: "Blocked operation", omitted: 2,
    issues: [{ kind: "missing", operation: "GET /items", message: "<script>not markup</script>", lines: [{ start: 4, end: 5 }, { start: 9, end: 9 }] }] },
  ...over,
});

const answer = (value: unknown) => ({ ok: true, json: async () => value });
const toggle = (tree: ReactTestRenderer, open: boolean) => act(async () => { tree.root.findByType("details").props.onToggle({ currentTarget: { open } }); });
const shown = (tree: ReactTestRenderer) => JSON.stringify(tree.toJSON());

describe("the cell's fold", () => {
  it("should_ReadOnceWhenOpenedAndRenderFindingsAsWrappedText_When_TheReportIsSaved", async () => {
    // Arrange
    const fetch = vi.fn().mockResolvedValue(answer(report()));
    vi.stubGlobal("fetch", fetch);
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<DescribeFailureDetails id={id} />); });
    // Assert: closed, nothing is read
    expect(fetch).not.toHaveBeenCalled();
    // Act
    await toggle(tree, true);
    // Assert
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(JSON.parse(fetch.mock.calls[0]![1].body)).toEqual({ action: "describeFailure", id });
    expect(tree.root.findAllByType("pre")).toHaveLength(0);
    expect(tree.root.findByProps({ className: "mono-ink describe-failure-prose" }).children.join("")).toBe("<script>not markup</script>");
    expect(tree.root.findAllByType("script")).toHaveLength(0);
    expect(shown(tree)).toContain("lines 4–5, 9");
    expect(shown(tree)).toContain("GET /items");
    expect(shown(tree)).toContain("2 additional diagnostics omitted");
    expect(shown(tree)).toContain("⌘click the verdict");
    expect(tree.root.findByType("ol").props.className).toBe("describe-failure-issues");
    expect(tree.root.findByType("ol").props.tabIndex).toBe(0);
    // Act: closing and reopening does not read again
    await toggle(tree, false);
    await toggle(tree, true);
    expect(fetch).toHaveBeenCalledTimes(1);
    act(() => tree.unmount());
  });

  it("should_SayWhyAndRetryOnRequest_When_TheReportCannotBeRead", async () => {
    // Arrange
    const fetch = vi.fn()
      .mockResolvedValueOnce({ ok: false, text: async () => "Saved report unavailable" })
      .mockResolvedValueOnce(answer(report()));
    vi.stubGlobal("fetch", fetch);
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<DescribeFailureDetails id={id} />); });
    await toggle(tree, true);
    // Assert
    expect(tree.root.findByProps({ role: "alert" }).findByType("p").children.join("")).toContain("Saved report unavailable");
    // Act
    await act(async () => { tree.root.findByType("button").props.onClick(); });
    // Assert
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(tree.root.findAllByProps({ role: "alert" })).toHaveLength(0);
    expect(shown(tree)).toContain("Blocked operation");
    act(() => tree.unmount());
  });

  it("should_DropTheEarlierAnswer_When_TheReportIdChangesWhileReading", async () => {
    // Arrange: two reads in flight, the first for an id the cell no longer names
    const settle: Record<string, (value: unknown) => void> = {};
    const fetch = vi.fn((_: string, init: { body: string }) => new Promise(resolve => { settle[JSON.parse(init.body).id] = resolve; }));
    vi.stubGlobal("fetch", fetch);
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<DescribeFailureDetails id={id} />); });
    await toggle(tree, true);
    await act(async () => { tree.update(<DescribeFailureDetails id={other} />); });
    // Act
    await act(async () => { settle[id]!(answer(report({ report: { version: 1, message: "Stale answer", omitted: 0, issues: [] } }))); });
    // Assert
    expect(shown(tree)).not.toContain("Stale answer");
    expect(shown(tree)).toContain("Reading saved failure details");
    // Act
    await act(async () => { settle[other]!(answer(report({ id: other, report: { version: 1, message: "Current answer", omitted: 0, issues: [] } }))); });
    // Assert
    expect(shown(tree)).toContain("Current answer");
    expect(fetch).toHaveBeenCalledTimes(2);
    act(() => tree.unmount());
  });

  it("should_RejectArbitraryPathsAsReportReferences", () => {
    expect(describeReportId({ id: "e", code: "DSC002", message: "failure", causeId: "", issues: [{ path: "/describeReport", code: "DSC_REPORT", message: "../../secret" }] })).toBeUndefined();
    expect(describeReportId({ id: "e", code: "DSC002", message: "failure", causeId: "", issues: [{ path: "/describeReport", code: "DSC_REPORT", message: id }] })).toBe(id);
    expect(describeReportId(undefined)).toBeUndefined();
  });
});

describe("the report as text and as a screen", () => {
  it("should_CarryEveryFactInOrder_When_CopiedAsText", () => {
    // Arrange / Act
    const text = describeFailureText(report());
    // Assert
    expect(text).toBe([
      "Blocked operation",
      "source: https://docs.example.test",
      "discovered: https://docs.example.test/guide",
      `digest: ${"ab".repeat(32)}`,
      `report: ${id}`,
      "OpenAPI parsing and contract validation findings. No spec was saved.",
      "",
      "missing · GET /items · lines 4–5, 9\n  <script>not markup</script>",
      "",
      "2 additional diagnostics omitted by the report budget.",
    ].join("\n"));
    expect(describeLineRanges([{ start: 137, end: 137 }, { start: 162, end: 170 }])).toBe("137, 162–170");
  });

  it("should_ShowProvenanceOnlyInThePeek_And_KeepTheCellCompact", () => {
    // Arrange / Act
    let peek!: ReactTestRenderer;
    let cell!: ReactTestRenderer;
    act(() => { peek = create(<DescribeFailureContent details={report()} mode="peek" />); });
    act(() => { cell = create(<DescribeFailureContent details={report()} mode="cell" />); });
    // Assert
    expect(shown(peek)).toContain("ab".repeat(32));
    expect(shown(peek)).toContain("https://docs.example.test/guide");
    expect(shown(peek)).not.toContain("⌘click the verdict");
    expect(shown(cell)).not.toContain("ab".repeat(32));
    expect(shown(cell)).toContain("https://docs.example.test");
    expect(shown(cell)).not.toContain("runner");
    expect(cell.root.findByProps({ className: "describe-failure describe-failure-cell" })).toBeTruthy();
    act(() => { peek.unmount(); cell.unmount(); });
  });
});
