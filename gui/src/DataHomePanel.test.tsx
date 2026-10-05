import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { DataHomePanel, openDataHome } from "./DataHomePanel";
import { flushDesktopPreferences } from "./desktop-preferences";
vi.mock("./desktop-preferences", () => ({ flushDesktopPreferences: vi.fn().mockResolvedValue(undefined) }));
let tree: ReactTestRenderer | undefined;
beforeEach(() => { vi.stubGlobal("window", { __WES_DESKTOP__: true }); vi.mocked(flushDesktopPreferences).mockReset().mockResolvedValue(undefined); });
afterEach(() => { if (tree) act(() => tree!.unmount()); tree = undefined; vi.unstubAllGlobals(); });

it("does not offer desktop folder control to an ordinary browser", async () => {
  vi.stubGlobal("window", {}); const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
  await act(async () => { tree = create(<DataHomePanel />); });
  expect(tree!.toJSON()).toBeNull(); expect(fetch).not.toHaveBeenCalled();
});
it("shows the current root and keeps the current pane available after a refused switch", async () => {
  const fetch = vi.fn().mockResolvedValueOnce({ ok: true, json: async () => ({ path: "/fixture/one", identity: "one" }) })
    .mockResolvedValueOnce({ ok: false, json: async () => ({ message: "Close Shell terminal sessions first." }) });
  vi.stubGlobal("fetch", fetch);
  await act(async () => { tree = create(<DataHomePanel />); });
  expect(tree!.root.findByType("input").props.value).toBe("/fixture/one");
  await act(async () => { tree!.root.findByType("input").props.onChange({ target: { value: "/fixture/two" } }); });
  await act(async () => { tree!.root.findByType("form").props.onSubmit({ preventDefault() {} }); });
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(JSON.parse(fetch.mock.calls[1]![1].body)).toEqual({ path: "/fixture/two" });
  expect(tree!.root.findByProps({ role: "status" }).children.join("")).toContain("Close Shell");
  expect(tree!.root.findByType("code").children.join("")).toBe("/fixture/one");
  expect(tree!.root.findByType("button").props.disabled).toBe(false);
});
it("flushes UI preferences before admitting the switch and leaves navigation to the native owner", async () => {
  const order: string[] = [];
  vi.mocked(flushDesktopPreferences).mockImplementation(async () => { order.push("saved"); });
  const fetch = vi.fn().mockImplementation(async () => { order.push("switch"); return { ok: true }; });
  vi.stubGlobal("fetch", fetch);
  await openDataHome("/fixture/new");
  expect(order).toEqual(["saved", "switch"]);
  expect(fetch).toHaveBeenCalledOnce();
});
it("does not switch after an unconfirmed preference write and never retries automatically", async () => {
  const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
  vi.mocked(flushDesktopPreferences).mockRejectedValueOnce(new Error("preferences not saved"));
  await expect(openDataHome("/fixture/new")).rejects.toThrow("preferences not saved");
  expect(fetch).not.toHaveBeenCalled();
});
it("reports an unavailable current home and keeps the opening action disabled", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: false }));
  await act(async () => { tree = create(<DataHomePanel />); });
  expect(tree!.root.findByProps({ role: "status" }).children.join("")).toContain("could not be read");
  expect(tree!.root.findByType("button").props.disabled).toBe(true);
});
