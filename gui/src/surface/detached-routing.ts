import { detachedPurpose } from "./open-route";

declare global { interface Window { __WES_RETARGET__?: (url: string) => void } }

/** Retarget only an existing detached purpose. Dirty editors may hold the transition for review. */
export function followDetachedRoutes(initial: string, draw: (hash: string) => void): () => void {
  const purpose = detachedPurpose(initial);
  if (!purpose) return () => undefined;
  let current = initial;
  let attempt = 0;
  let active = true;
  const requested = (hash: string) => {
    if (detachedPurpose(hash) !== purpose) {
      if (hash !== current) window.history.replaceState(null, "", current);
      return;
    }
    const ticket = ++attempt;
    if (hash === current) return;
    const base = current;
    const proceed = () => {
      if (!active || ticket !== attempt || current !== base) return;
      current = hash;
      window.history.replaceState(null, "", hash);
      draw(hash);
    };
    const event = new CustomEvent("wes:before-route", { cancelable: true, detail: { hash, proceed } });
    if (window.dispatchEvent(event)) proceed();
    else window.history.replaceState(null, "", current);
  };
  const changed = () => requested(window.location.hash);
  const retarget = (address: string) => {
    const url = new URL(address, window.location.href);
    if (url.origin === window.location.origin) requested(url.hash);
  };
  window.__WES_RETARGET__ = retarget;
  window.addEventListener?.("hashchange", changed);
  return () => { active = false; attempt++; window.removeEventListener?.("hashchange", changed); if (window.__WES_RETARGET__ === retarget) delete window.__WES_RETARGET__; };
}
