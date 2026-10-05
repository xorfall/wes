import { describe, expect, it, vi } from "vitest";
import { AssistantEditor, CommandDraft, type EditorRequest } from "./assistant-editor";
const request = (id: string, action: string, text: string | null = null, revision: string | null = null): EditorRequest => ({ id, action, text, revision });
it("reads layout without an editor and acknowledges tab creation once even while pending", async () => {
  const bridge = new AssistantEditor();
  expect(bridge.handle(request("missing", "layout"))).toMatchObject({ ok: false });
  let finish!: (value: unknown) => void;
  const open = vi.fn(() => new Promise(resolve => { finish = resolve; }));
  const detach = bridge.attachLayout({ read: () => ({ panes: [{ id: "p1" }] }), open });
  expect(bridge.handle(request("layout", "layout"))).toMatchObject({ ok: true, layout: { panes: [{ id: "p1" }] } });
  const tab = request("tab", "tab", JSON.stringify({ pane: "p1", workspace: "shared", activate: false }));
  const pending = bridge.handle(tab);
  expect(bridge.handle(tab)).toBe(pending);
  finish({ panes: [] });
  await expect(pending).resolves.toMatchObject({ ok: true });
  expect(open).toHaveBeenCalledTimes(1);
  expect(bridge.handle({ ...tab, text: "different" })).toMatchObject({ ok: false });
  detach();
  expect(bridge.handle(request("detached", "layout"))).toMatchObject({ ok: false });
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
    const bridge = new AssistantEditor(), draft = new CommandDraft();
    expect(bridge.handle(request("absent", "read"))).toMatchObject({ ok: false });
    const detach = bridge.attach({ read: () => draft.read(), update: (text, revision) => draft.replace(text, revision) });
    const original = bridge.handle(request("read", "read")) as { revision: string };
    const change = request("write", "update", ":calc { return 42; }", original.revision);
    const reply = bridge.handle(change);
    expect(reply).toMatchObject({ ok: true, text: ":calc { return 42; }" });
    draft.write("new human draft");
    expect(bridge.handle(change)).toEqual(reply);
    expect(draft.read().text).toBe("new human draft");
    expect(bridge.handle({ ...change, text: "reused ID" })).toMatchObject({ ok: false });
    detach();
    expect(bridge.handle(request("detached", "read"))).toMatchObject({ ok: false });
  });
  it("rejects oversized UTF-8 drafts and request capacity before applying effects", () => {
    const bridge = new AssistantEditor(), draft = new CommandDraft();
    bridge.attach({ read: () => draft.read(), update: (text, revision) => draft.replace(text, revision) });
    expect(bridge.handle(request("large", "update", "€".repeat(30_000), draft.read().revision))).toMatchObject({ ok: false });
    draft.write("x".repeat(65 * 1024));
    expect(bridge.handle(request("read-large", "read"))).toMatchObject({ ok: false });
    draft.write("");
    for (let i = 0; i < 254; i++) bridge.handle(request(`read-${i}`, "read"));
    expect(bridge.handle(request("full", "update", "no", draft.read().revision))).toMatchObject({ ok: false });
    expect(draft.read().text).toBe("");
  });
});
