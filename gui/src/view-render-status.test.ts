import { describe, expect, it } from "vitest";
import { frameRenderObservation, MAX_SCOPE_HOSTS, parseRenderScope, RenderHost, RenderStatusRegistry, type RenderScope } from "./view-render-status";
import type { ViewFrame } from "./value-views/instances";

const scope: RenderScope = { workspace: "research", generation: "g1", node: "chart", instance: "chart-instance" };
const host = (mode: "preview" | "expanded" | "window" = "preview") => new RenderHost(() => ({ digest: "contract-digest", mode }));

describe("render status registry", () => {
  it("should_KeepEveryMount_When_SeveralCanvasesShareOneInstance", () => {
    const registry = new RenderStatusRegistry(), preview = host("preview"), window = host("window");
    registry.register(scope, preview); registry.register(scope, window);
    preview.delivered(1, "r1"); preview.drew(1, "r1");
    const receipts = registry.receipts(scope);
    expect(receipts.map(r => r.host)).toEqual([preview.id, window.id]);
    expect(receipts[0]).toMatchObject({ mode: "preview", status: "drawn", ackSequence: 1, drawnInputRevision: "r1" });
    expect(receipts[1]).toMatchObject({ mode: "window", status: "loading", sentSequence: 0, ackSequence: 0, requestedInputRevision: null, drawnInputRevision: null, error: null });
    expect(preview.id).toMatch(/^[0-9a-f-]{36}$/);
  });
  it("should_IsolateReceipts_When_AnyScopeFieldDiffers", () => {
    const registry = new RenderStatusRegistry(), mounted = host();
    registry.register(scope, mounted);
    for (const other of [{ ...scope, workspace: "other" }, { ...scope, generation: "g2" }, { ...scope, node: "board" }, { ...scope, instance: "replaced" }])
      expect(registry.receipts(other)).toEqual([]);
    expect(registry.receipts(scope)).toHaveLength(1);
  });
  it("should_ReleaseEntries_When_CanvasUnmounts", () => {
    const registry = new RenderStatusRegistry(), first = host(), second = host();
    const releaseFirst = registry.register(scope, first), releaseSecond = registry.register(scope, second);
    releaseFirst(); releaseFirst();
    expect(registry.receipts(scope).map(r => r.host)).toEqual([second.id]);
    releaseSecond();
    expect(registry.receipts(scope)).toEqual([]);
    expect(registry.scopeCount).toBe(0);
  });
  it("should_NotCreateEntries_When_ReceiptsAreRead", () => {
    const registry = new RenderStatusRegistry();
    expect(registry.receipts(scope)).toEqual([]);
    expect(registry.scopeCount).toBe(0);
  });
  it("should_BoundHostsPerScope_When_TooManyCanvasesMount", () => {
    const registry = new RenderStatusRegistry(), hosts = Array.from({ length: MAX_SCOPE_HOSTS + 1 }, () => host());
    hosts.forEach(h => registry.register(scope, h));
    expect(registry.receipts(scope)).toHaveLength(MAX_SCOPE_HOSTS);
    expect(registry.receipts(scope).map(r => r.host)).not.toContain(hosts[MAX_SCOPE_HOSTS]!.id);
  });
  it("should_KeepFirstEnumErrorOnly_When_HostFails", () => {
    const failing = host();
    failing.delivered(1, "r1"); failing.failed("draw_timeout"); failing.failed("renderer_failed"); failing.drew(1, "r1");
    expect(failing.receipt()).toEqual({ host: failing.id, digest: "contract-digest", mode: "preview", status: "failed", requestedInputRevision: "r1",
      drawnInputRevision: null, sentSequence: 1, ackSequence: 0, error: "draw_timeout" });
    failing.restart();
    expect(failing.receipt()).toMatchObject({ status: "loading", error: null, sentSequence: 0, requestedInputRevision: null });
  });
  it("should_NotDowngradeDrawn_When_ReadyArrivesAfterAcknowledgement", () => {
    const drawn = host();
    drawn.delivered(1, "r1"); drawn.drew(1, "r1"); drawn.ready();
    expect(drawn.receipt().status).toBe("drawn");
  });
});

describe("render scope", () => {
  it("should_ParseExactScope_When_RequestIsWellFormed", () => {
    expect(parseRenderScope({ ...scope })).toEqual(scope);
  });
  it("should_Reject_When_RequestIsMalformed", () => {
    for (const input of [null, undefined, "text", JSON.stringify(scope), [], { ...scope, extra: 1 }, { ...scope, node: "" }, { ...scope, node: 4 },
      { ...scope, instance: "x".repeat(257) }, { ...scope, workspace: "a\nb" }, { workspace: "w", generation: "g", node: "n" }])
      expect(() => parseRenderScope(input)).toThrow("Invalid view render status request.");
  });
  it("should_ObserveOnlyLiveFrameEntries_When_ResolvingCanvasScope", () => {
    const frame = { root: "board", instances: [{ id: "board", instance: "board-instance" }, { id: "card", instance: "card-instance" }] } as unknown as ViewFrame;
    const observation = frameRenderObservation(new RenderStatusRegistry(), "research", "g1", frame);
    expect(observation.scope("view/card", "card-instance")).toEqual({ workspace: "research", generation: "g1", node: "card", instance: "card-instance" });
    expect(observation.scope("view/card", "stale-instance")).toBeUndefined();
    expect(observation.scope("view/card", undefined)).toBeUndefined();
    expect(observation.scope("view/board/child", "board-instance")).toBeUndefined();
  });
});
