import { afterEach, expect, it, vi } from "vitest";
import { followDetachedRoutes } from "./detached-routing";

let stop: (() => void) | undefined;
afterEach(() => { stop?.(); stop = undefined; vi.unstubAllGlobals(); });
function fixture(hash: string) {
  const events = new EventTarget();
  const location = { hash, href: `http://synthetic.test/${hash}`, origin: "http://synthetic.test" };
  const replaceState = vi.fn((_state: unknown, _title: string, next: string) => { location.hash = next; });
  const window = Object.assign(events, { location, history: { replaceState }, __WES_RETARGET__: undefined as ((url: string) => void) | undefined });
  vi.stubGlobal("window", window);
  const draw = vi.fn();
  stop = followDetachedRoutes(hash, draw);
  return { window, draw, replaceState };
}
it("retargets the same detached purpose and origin without replacing its document", () => {
  const f = fixture("#graph?workspace=alpha");
  f.window.__WES_RETARGET__!("http://synthetic.test/#stale?workspace=beta");
  expect(f.draw).toHaveBeenCalledExactlyOnceWith("#stale?workspace=beta");
  f.window.__WES_RETARGET__!("http://other.test/#graph?workspace=secret");
  f.window.__WES_RETARGET__!("http://synthetic.test/#spec?workspace=beta");
  expect(f.draw).toHaveBeenCalledOnce();
  stop!(); stop = undefined;
  expect(f.window.__WES_RETARGET__).toBeUndefined();
});
it("holds a dirty retarget and restores the old address until the editor approves", () => {
  const f = fixture("#spec?workspace=alpha");
  let proceed: (() => void) | undefined;
  f.window.addEventListener("wes:before-route", event => {
    event.preventDefault(); proceed = (event as CustomEvent<{ proceed(): void }>).detail.proceed;
  });
  f.window.location.hash = "#spec?workspace=beta";
  f.window.dispatchEvent(new Event("hashchange"));
  expect(f.draw).not.toHaveBeenCalled();
  expect(f.window.location.hash).toBe("#spec?workspace=alpha");
  proceed!();
  expect(f.draw).toHaveBeenCalledExactlyOnceWith("#spec?workspace=beta");
  expect(f.window.location.hash).toBe("#spec?workspace=beta");
});
it("keeps source/peek/result in one purpose and carries no old node across a workspace change", () => {
  const f = fixture("#peek/item/json?workspace=alpha");
  f.window.__WES_RETARGET__!("http://synthetic.test/#open?workspace=beta");
  expect(f.draw).toHaveBeenCalledExactlyOnceWith("#open?workspace=beta");
});
it("restores an invalid or different-purpose hash and rejects superseded editor approvals", () => {
  const f = fixture("#spec?workspace=alpha");
  f.window.location.hash = "#graph?workspace=beta";
  f.window.dispatchEvent(new Event("hashchange"));
  expect(f.window.location.hash).toBe("#spec?workspace=alpha");
  const approvals: (() => void)[] = [];
  f.window.addEventListener("wes:before-route", event => { event.preventDefault(); approvals.push((event as CustomEvent<{ proceed(): void }>).detail.proceed); });
  f.window.__WES_RETARGET__!("http://synthetic.test/#spec?workspace=beta");
  f.window.__WES_RETARGET__!("http://synthetic.test/#spec?workspace=gamma");
  approvals[0]!();
  expect(f.draw).not.toHaveBeenCalled();
  approvals[1]!();
  expect(f.draw).toHaveBeenCalledExactlyOnceWith("#spec?workspace=gamma");
});
