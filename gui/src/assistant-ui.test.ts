import { describe, expect, it, vi } from "vitest";
import { AssistantUi, CommandDraft, decodeUiRequest, type UiOperation, type UiRequest } from "./assistant-ui";
import { RenderHost, RenderStatusRegistry } from "./view-render-status";
const request = (id: string, operation: UiOperation): UiRequest => ({ id, operation });
const wire = (value: unknown) => value as UiRequest;
it("reads layout without an editor and acknowledges tab creation once even while pending", async () => {
  const bridge = new AssistantUi();
  expect(bridge.handle(request("missing", { kind: "layout_read" }))).toMatchObject({ ok: false });
  let finish!: (value: unknown) => void;
  const open = vi.fn(() => new Promise(resolve => { finish = resolve; }));
  const detach = bridge.attachLayout({ read: () => ({ panes: [{ id: "p1" }] }), open });
  expect(bridge.handle(request("layout", { kind: "layout_read" }))).toMatchObject({ ok: true, layout: { panes: [{ id: "p1" }] } });
  const tab = request("tab", { kind: "tab_open", workspace: "shared", pane: "p1", activate: false });
  const pending = bridge.handle(tab);
  // Field order on the wire does not change identity; the decoded request is canonical.
  expect(bridge.handle(wire({ operation: { activate: false, pane: "p1", workspace: "shared", kind: "tab_open" }, id: "tab" }))).toBe(pending);
  finish({ panes: [] });
  await expect(pending).resolves.toMatchObject({ ok: true });
  expect(open).toHaveBeenCalledTimes(1);
  expect(open).toHaveBeenCalledWith({ workspace: "shared", pane: "p1", activate: false });
  expect(bridge.handle(request("tab", { kind: "tab_open", workspace: "shared", pane: "p1", activate: true }))).toMatchObject({ ok: false, error: expect.stringContaining("reused") });
  expect(open).toHaveBeenCalledTimes(1);
  detach();
  expect(bridge.handle(request("detached", { kind: "layout_read" }))).toMatchObject({ ok: false });
});
describe("typed UI request contract", () => {
  const scope = { workspace: "research", generation: "g1", node: "chart", instance: "chart-instance" };
  it("should_DecodeEachOperation_When_FieldsAreExact", () => {
    const operations: UiOperation[] = [{ kind: "draft_read" }, { kind: "draft_update", text: "", revision: "r1" }, { kind: "layout_read" },
      { kind: "tab_open", workspace: "shared", pane: "p1", activate: true }, { kind: "view_render_status", scope }];
    for (const operation of operations) expect(decodeUiRequest({ id: "x", operation })).toEqual({ id: "x", operation });
  });
  it.each([
    ["legacy action packet", { id: "a", action: "read", text: null, revision: null }],
    ["legacy text payload", { id: "a", operation: { kind: "tab_open", text: JSON.stringify({ workspace: "w", pane: "p", activate: false }) } }],
    ["unknown kind", { id: "a", operation: { kind: "draft_delete" } }],
    ["inherited kind", { id: "a", operation: { kind: "toString" } }],
    ["extra operation field", { id: "a", operation: { kind: "draft_read", text: "" } }],
    ["extra request field", { id: "a", operation: { kind: "layout_read" }, revision: null }],
    ["missing field", { id: "a", operation: { kind: "draft_update", text: "x" } }],
    ["wrong field type", { id: "a", operation: { kind: "tab_open", workspace: "w", pane: "p", activate: "true" } }],
    ["unsafe workspace", { id: "a", operation: { kind: "tab_open", workspace: "../w", pane: "p", activate: false } }],
    ["control in pane", { id: "a", operation: { kind: "tab_open", workspace: "w", pane: "p\n", activate: false } }],
    ["empty id", { id: "", operation: { kind: "layout_read" } }],
    ["oversized id", { id: "i".repeat(257), operation: { kind: "layout_read" } }],
    ["array operation", { id: "a", operation: [] }],
    ["textual scope", { id: "a", operation: { kind: "view_render_status", scope: JSON.stringify(scope) } }],
    ["not an object", "draft_read"],
  ])("should_RefuseBeforeAnyEffect_When_RequestIs_%s", (_name, input) => {
    const bridge = new AssistantUi(new RenderStatusRegistry()), update = vi.fn(), open = vi.fn();
    bridge.attach({ read: () => ({ text: "kept", revision: "r" }), update });
    bridge.attachLayout({ read: () => ({}), open });
    expect(bridge.handle(wire(input))).toMatchObject({ ok: false });
    expect(update).not.toHaveBeenCalled();
    expect(open).not.toHaveBeenCalled();
  });
  it("should_NotConsumeId_When_RequestIsMalformed", () => {
    const bridge = new AssistantUi();
    bridge.attachLayout({ read: () => ({ panes: [] }), open: vi.fn() });
    expect(bridge.handle(wire({ id: "same", operation: { kind: "layout_read", extra: 1 } }))).toMatchObject({ ok: false });
    expect(bridge.handle(request("same", { kind: "layout_read" }))).toMatchObject({ ok: true });
  });
});
describe("view render status bridge", () => {
  const scope = { workspace: "research", generation: "g1", node: "chart", instance: "chart-instance" };
  it("should_ReturnMountedReceipts_When_ScopeIsValid_WithoutEditorOrLayout", () => {
    const registry = new RenderStatusRegistry(), host = new RenderHost(() => ({ digest: "contract", mode: "window" }));
    registry.register(scope, host); host.delivered(1, "r1"); host.drew(1, "r1");
    const bridge = new AssistantUi(registry), draft = new CommandDraft(); draft.write("kept");
    bridge.attach({ read: () => draft.read(), update: vi.fn() });
    const reply = bridge.handle(request("status", { kind: "view_render_status", scope }));
    expect(reply).toEqual({ ok: true, ...scope, hosts: [{ host: host.id, digest: "contract", mode: "window", status: "drawn", requestedInputRevision: "r1",
      drawnInputRevision: "r1", sentSequence: 1, ackSequence: 1, error: null }] });
    expect(draft.read().text).toBe("kept");
    expect(registry.scopeCount).toBe(1);
  });
  it("should_ReturnEmptyHosts_When_NoCanvasIsMounted", () => {
    const registry = new RenderStatusRegistry(), bridge = new AssistantUi(registry);
    expect(bridge.handle(request("none", { kind: "view_render_status", scope }))).toEqual({ ok: true, ...scope, hosts: [] });
    expect(registry.scopeCount).toBe(0);
  });
  it("should_ReturnEmptyHosts_When_ScopeNamesAnotherWorkspace", () => {
    const registry = new RenderStatusRegistry(), bridge = new AssistantUi(registry);
    registry.register(scope, new RenderHost(() => ({ digest: "contract", mode: "preview" })));
    expect(bridge.handle(request("other", { kind: "view_render_status", scope: { ...scope, workspace: "default" } }))).toMatchObject({ ok: true, hosts: [] });
  });
  it("should_Refuse_When_ScopeIsMalformed", () => {
    const bridge = new AssistantUi(new RenderStatusRegistry());
    expect(bridge.handle(wire({ id: "bad", operation: { kind: "view_render_status", scope: { ...scope, extra: true } } }))).toEqual({ ok: false, error: "Invalid view render status request." });
    expect(bridge.handle(wire({ id: "empty", operation: { kind: "view_render_status" } }))).toMatchObject({ ok: false });
  });
});
describe("assistant editor acknowledgement", () => {
  it("rejects concurrent typing and scope changes without losing the user's text", () => {
    const draft = new CommandDraft(); draft.write("user draft");
    const original = draft.read();
    draft.write("user kept typing");
    expect(() => draft.replace("agent draft", original.revision)).toThrow("Draft changed");
    expect(draft.read().text).toBe("user kept typing");
    const before = draft.read(); draft.setScope("another workspace");
    expect(() => draft.replace("wrong workspace", before.revision)).toThrow("Draft changed");
  });
  it("writes only to the attached editor and repeats acknowledgements without applying twice", () => {
    const bridge = new AssistantUi(), draft = new CommandDraft();
    expect(bridge.handle(request("absent", { kind: "draft_read" }))).toMatchObject({ ok: false });
    const detach = bridge.attach({ read: () => draft.read(), update: (text, revision) => draft.replace(text, revision) });
    const original = bridge.handle(request("read", { kind: "draft_read" })) as { revision: string };
    const change = request("write", { kind: "draft_update", text: ":calc { return 42; }", revision: original.revision });
    const reply = bridge.handle(change);
    expect(reply).toMatchObject({ ok: true, text: ":calc { return 42; }" });
    draft.write("new human draft");
    expect(bridge.handle(change)).toEqual(reply);
    expect(draft.read().text).toBe("new human draft");
    expect(bridge.handle(request("write", { kind: "draft_update", text: "reused ID", revision: original.revision }))).toMatchObject({ ok: false });
    detach();
    expect(bridge.handle(request("detached", { kind: "draft_read" }))).toMatchObject({ ok: false });
  });
  it("rejects oversized UTF-8 drafts and request capacity before applying effects", () => {
    const bridge = new AssistantUi(), draft = new CommandDraft(), update = vi.fn((text: string, revision: string) => draft.replace(text, revision));
    bridge.attach({ read: () => draft.read(), update });
    expect(bridge.handle(request("large", { kind: "draft_update", text: "€".repeat(30_000), revision: draft.read().revision }))).toMatchObject({ ok: false, error: "Draft exceeds 64 KiB." });
    expect(update).not.toHaveBeenCalled();
    draft.write("x".repeat(65 * 1024));
    expect(bridge.handle(request("read-large", { kind: "draft_read" }))).toMatchObject({ ok: false });
    draft.write("");
    for (let i = 0; i < 255; i++) bridge.handle(request(`read-${i}`, { kind: "draft_read" }));
    expect(bridge.handle(request("full", { kind: "draft_update", text: "no", revision: draft.read().revision }))).toMatchObject({ ok: false });
    expect(update).not.toHaveBeenCalled();
    expect(draft.read().text).toBe("");
  });
});
