import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { act, create, type ReactTestInstance } from "react-test-renderer";
import { valueViewModules } from "./registry";
import { decodeContract } from "./definition";
import spanTimeline, { prepare } from "../../../views/span-timeline/View";
import { definition, type Input } from "../../../views/span-timeline/contract";

const context = { mode: "expanded" as const, instance: null };
const render = (input: Input, mode: "preview" | "expanded" | "window" = "expanded") =>
  renderToStaticMarkup(<spanTimeline.Component input={input} state={undefined as never} revision={0} emit={() => {}} slots={{}} context={{ ...context, mode }} />);

/*
 * Independently synthetic run over a 40-minute range: an invented docs lane that succeeds, a
 * packaging lane whose assemble step fails, a gate lane that never started and a lane still
 * running. Every name, time and limit is made up for this test.
 */
const run: Input = {
  view: "span-timeline",
  title: "synthetic-run-a · attempt 1",
  range: "2031-04-02T08:00:00Z/2031-04-02T08:40:00Z",
  lanes: [
    { id: "docs", label: "Docs preview", status: "success", note: null, limits: [], spans: [
      { id: "docs-1", label: "Render pages", status: "success", start: "2031-04-02T08:00:30Z", end: "2031-04-02T08:01:30Z" },
    ] },
    { id: "pack", label: "Packaging", status: "failure", note: null,
      limits: [
        { at: "2031-04-02T08:50:00Z", label: "50m ref. limit", source: "synthetic stage policy" },
        { at: "2031-04-02T08:20:00Z", label: "20m marker", source: "test" },
        { at: "2031-04-02T07:50:00Z", label: "early marker", source: "test" },
      ],
      spans: [
        { id: "pack-1", label: "Fetch inputs", status: "success", start: "2031-04-02T08:02:00Z", end: "2031-04-02T08:14:00Z" },
        { id: "pack-2", label: "Assemble bundle with packer", status: "failure", start: "2031-04-02T08:18:00Z", end: "2031-04-02T08:23:30Z" },
        { id: "pack-3", label: "Cleanup", status: "skipped", start: "2031-04-02T08:23:30Z", end: "2031-04-02T08:23:30Z" },
      ] },
    { id: "gate", label: "Release gate", status: "cancelled", note: "not started by a scheduler · no execution", limits: [], spans: [] },
    { id: "live", label: "Still running", status: "running", note: null, limits: [], spans: [
      { id: "live-1", label: "Open step", status: "running", start: "2031-04-02T08:30:00Z", end: null },
    ] },
  ],
};

describe("SpanTimeline", () => {
  it("is a packaged view matched by its declared input", () => {
    const module = valueViewModules.named("span-timeline");
    expect(module?.definition?.name).toBe("SpanTimeline");
    expect(() => decodeContract(definition, definition.input, run)).not.toThrow();
    for (const bad of [
      { ...run, view: "timeline" },
      { ...run, lanes: [{ ...run.lanes[0]!, status: "red" }] },
      { ...run, lanes: [{ ...run.lanes[0]!, spans: [{ id: "x", label: "x", status: "success", start: "2031-04-02T08:00:30Z" }] }] },
    ]) expect(() => decodeContract(definition, definition.input, bad)).toThrow();
  });

  it("places spans on one scale, keeps open spans open, and refuses a span that ends before it starts", () => {
    const model = prepare(run);
    const pack = model.lanes[1]!;
    expect(pack.spans.map((s) => [s.left, Math.round(s.width * 100) / 100])).toEqual([[5, 30], [45, 13.75], [58.75, 0]]);
    expect(model.lanes[3]!.spans[0]!.open).toBe(true);
    expect(model.lanes[3]!.spans[0]!.unended).toBe(false);
    expect(model.lanes[3]!.spans[0]!.width).toBeCloseTo(25, 2);
    expect(pack.limits.map((l) => l.inside)).toEqual([false, true, false]);
    expect(model.total).toBe("40m 00s");
    expect(() => prepare({ ...run, lanes: [{ ...run.lanes[0]!, spans: [{ id: "x", label: "x", status: "success", start: "2031-04-02T08:10:00Z", end: "2031-04-02T08:05:00Z" }] }] })).toThrow("ends before it starts");
  });

  it("says why a lane has no spans, names a span's status and length, and shows only limits inside the range", () => {
    const html = render(run);
    expect(html).toContain("not started by a scheduler · no execution");
    expect(html).toContain("Assemble bundle with packer: failed, 5m 30s");
    expect(html).toContain("Open step: still running, no end yet");
    expect(html).toContain("20m marker");
    expect(html).not.toContain("50m ref. limit");
    expect(html).not.toContain("early marker");
    expect(html).toContain('data-status="skipped"');
  });

  it("marks a finished span without a recorded end at its start and never reads it as running", () => {
    const lost: Input = { ...run, lanes: [{ id: "lost", label: "Lost timing", status: "failure", note: null, limits: [], spans: [
      { id: "lost-1", label: "Publish report", status: "failure", start: "2031-04-02T08:10:00Z", end: null },
    ] }] };
    const span = prepare(lost).lanes[0]!.spans[0]!;
    expect([span.left, span.width, span.open, span.unended, span.length]).toEqual([25, 0, false, true, undefined]);
    const html = render(lost);
    expect(html).toContain('aria-label="Publish report: failed, end not recorded"');
    expect(html).toContain('title="Publish report · failed · end not recorded"');
    expect(html).toContain("data-unended");
    expect(html).not.toContain("data-open");
    expect(html).not.toContain("no end yet");
    let tree!: ReturnType<typeof create>;
    act(() => { tree = create(<spanTimeline.Component input={lost} state={undefined as never} revision={0} emit={() => {}} slots={{}} context={context} />); });
    act(() => tree.root.findAll((node) => node.type === "button" && node.props["aria-label"]?.startsWith("Publish report"))[0]!.props.onClick());
    const text = (node: ReactTestInstance | string): string => typeof node === "string" ? node : node.children.map(text).join("");
    const detail = text(tree.root.findByProps({ className: "span-timeline-detail" }));
    expect(detail).toContain("08:10:00 → end not recorded");
    expect(detail).not.toContain("no end yet");
    act(() => tree.unmount());
  });

  it("steps ticks over the whole extent and draws at most nine, however long the range", () => {
    const ticks = (range: string) => prepare({ ...run, range, lanes: [] }).ticks;
    expect(ticks(run.range).map((t) => t.text)).toEqual(["0m", "5m", "10m", "15m", "20m", "25m", "30m", "35m", "40m"]);
    expect(ticks("2031-04-02T08:00:00Z/2031-04-02T08:00:07Z").map((t) => t.text)).toEqual(["0s", "1s", "2s", "3s", "4s", "5s", "6s", "7s"]);
    expect(ticks("2031-04-02T08:00:00Z/2031-04-02T21:00:00Z").map((t) => t.text)).toEqual(["0h", "2h", "4h", "6h", "8h", "10h", "12h"]);
    const centuries = ticks("1900-01-01T00:00:00Z/2100-01-01T00:00:00Z");
    expect(centuries.map((t) => t.text)).toEqual(["0d", "10000d", "20000d", "30000d", "40000d", "50000d", "60000d", "70000d"]);
    expect(centuries.every((t, i) => t.left >= 0 && t.left <= 100 && (i === 0 || t.left > centuries[i - 1]!.left))).toBe(true);
  });

  it("colours by the declared status vocabulary and escapes labels", () => {
    const html = render({ ...run, lanes: [{ ...run.lanes[0]!, label: "<script>", spans: [] }] });
    expect(html).toContain("&lt;script&gt;");
    expect(render(run)).toContain('class="span-timeline-span status-bad"');
  });

  it("keeps a preview to four lanes and counts the rest", () => {
    const html = render({ ...run, lanes: [...run.lanes, { ...run.lanes[0]!, id: "fifth" }] }, "preview");
    expect(html).toContain("+1 lanes");
  });

  it("names a chosen span below the lanes; choosing it again keeps it, clear and Escape forget it", () => {
    const keys = new Map<string, (event: { key: string; preventDefault: () => void }) => void>();
    vi.stubGlobal("window", { addEventListener: (name: string, fn: never) => keys.set(name, fn), removeEventListener: (name: string) => keys.delete(name) });
    let tree!: ReturnType<typeof create>;
    act(() => { tree = create(<spanTimeline.Component input={run} state={undefined as never} revision={0} emit={() => {}} slots={{}} context={context} />); });
    const span = () => tree.root.findAll((node) => node.type === "button" && node.props["aria-label"]?.startsWith("Assemble bundle"))[0]!;
    const detail = () => tree.root.findAll((node) => node.props.className === "span-timeline-detail");
    expect(detail()).toHaveLength(0);
    act(() => span().props.onClick());
    expect(span().props["aria-current"]).toBe("true");
    expect(JSON.stringify(tree.toJSON())).toContain("08:18:00");
    expect(JSON.stringify(tree.toJSON())).toContain("08:23:30");
    expect(JSON.stringify(tree.toJSON())).toContain("5m 30s");
    act(() => span().props.onClick());
    expect(span().props["aria-current"]).toBe("true");
    act(() => tree.root.findByProps({ "aria-label": "Clear the chosen span" }).props.onClick());
    expect(detail()).toHaveLength(0);
    act(() => span().props.onClick());
    const prevent = vi.fn();
    act(() => keys.get("keydown")!({ key: "Escape", preventDefault: prevent }));
    expect(detail()).toHaveLength(0);
    expect(prevent).toHaveBeenCalledOnce();
    expect(keys.has("keydown")).toBe(false);
    vi.unstubAllGlobals();
    act(() => tree.unmount());
  });

  it("keeps a choice while focus is elsewhere and only reads it as inactive", () => {
    let tree!: ReturnType<typeof create>;
    const view = (active: boolean) => <spanTimeline.Component input={run} state={undefined as never} revision={0} emit={() => {}} slots={{}} context={{ ...context, active }} />;
    act(() => { tree = create(view(true)); });
    const span = () => tree.root.findAll((node) => node.type === "button" && node.props["aria-label"]?.startsWith("Assemble bundle"))[0]!;
    act(() => span().props.onClick());
    act(() => tree.update(view(false)));
    expect(span().props["aria-current"]).toBe("true");
    expect(tree.root.findByProps({ className: "span-timeline" }).props["data-active"]).toBe(false);
    act(() => tree.update(view(true)));
    act(() => span().props.onClick());
    expect(span().props["aria-current"]).toBe("true");
    act(() => tree.unmount());
  });
});
