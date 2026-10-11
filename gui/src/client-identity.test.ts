import { afterEach, expect, it, vi } from "vitest";
import { desktopClient, initializeDesktopClient } from "./client-identity";

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });
class NativeWindow extends EventTarget {
  __WES_DESKTOP__ = true;
  __WES_WINDOW_CLIENT__?: string;
}
it("waits for the owning native window before allowing engine startup", async () => {
  const window = new NativeWindow(); vi.stubGlobal("window", window);
  let ready = false;
  const pending = initializeDesktopClient().then(() => { ready = true; });
  await Promise.resolve(); expect(ready).toBe(false);
  window.__WES_WINDOW_CLIENT__ = "invalid";
  window.dispatchEvent(new Event("wes:window-client"));
  await Promise.resolve(); expect(ready).toBe(false);
  window.__WES_WINDOW_CLIENT__ = "c22f3f98-249d-4e10-b4ee-13a80d8ffeda";
  window.dispatchEvent(new Event("wes:window-client"));
  await pending;
  expect(desktopClient()).toBe(window.__WES_WINDOW_CLIENT__);
  await initializeDesktopClient();
});
it("fails startup if native ownership cannot be established, without substituting an ephemeral client", async () => {
  vi.useFakeTimers(); vi.stubGlobal("window", new NativeWindow());
  const pending = expect(initializeDesktopClient()).rejects.toThrow("Native window identity is unavailable");
  await vi.advanceTimersByTimeAsync(5000); await pending;
  expect(desktopClient()).toBeUndefined();
});
it("does not wait for a native callback in a browser", async () => {
  vi.stubGlobal("window", {});
  await initializeDesktopClient();
  expect(desktopClient()).toBeUndefined();
});
