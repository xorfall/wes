/** Delayed full variable names, outside clipped transcript cells. */
import { useEffect, useId, useState } from "react";
import { createPortal } from "react-dom";

/** How long the pointer rests on something before its hint appears: long enough to read past it. */
export const HINT_DELAY_MS = 700;
/** Moving straight from one hinted element to another still waits, but not the whole delay. */
export const HINT_SWITCH_MS = 250;
/** How long a hint lingers after the pointer leaves, so a small slip does not flicker it. */
export const HINT_LINGER_MS = 100;
/** How long a pointer hint stays at most; long enough to read twice, short enough to get out of the way. */
export const HINT_STAY_MS = 4000;
const HINT_GAP = 8;
const HINT_MARGIN = 12;

export interface HintPlace { readonly left: number; readonly top: number }

/** Below the element's left edge, like the platform's own tooltips; kept inside the viewport. */
export function placeHint(rect: Pick<DOMRect, "left" | "bottom" | "top">, hint: { readonly width: number; readonly height: number }, viewport: { readonly width: number; readonly height: number }): HintPlace {
  const left = Math.max(HINT_MARGIN, Math.min(viewport.width - hint.width - HINT_MARGIN, rect.left));
  const below = rect.bottom + HINT_GAP;
  const top = below + hint.height + HINT_MARGIN <= viewport.height ? below : Math.max(HINT_MARGIN, rect.top - HINT_GAP - hint.height);
  return { left, top };
}

/** What the layer decides on one pointer or focus event, with the timing it wants. */
export type HintStep =
  | { readonly kind: "none" }
  | { readonly kind: "arm"; readonly after: number }
  | { readonly kind: "show"; readonly stay?: number }
  | { readonly kind: "hide"; readonly after: number }
  | { readonly kind: "keep" };

/**
 * The pointer came over `element` (nothing hinted when `undefined`) while `shown` was on screen.
 *
 * Coming back onto the shown element within its linger keeps it. Leaving for something unhinted
 * lets it linger and go. A different hinted element re-arms: a shorter wait when a hint is already
 * up, the full one otherwise, and never an instant switch — the old hint goes first.
 */
export function pointerOver(shown: object | undefined, element: object | undefined): HintStep {
  if (element !== undefined && element === shown) return { kind: "keep" };
  if (element === undefined) return shown ? { kind: "hide", after: HINT_LINGER_MS } : { kind: "none" };
  return { kind: "arm", after: shown ? HINT_SWITCH_MS : HINT_DELAY_MS };
}

/** Focus reached `element`: only focus the keyboard gave shows a hint, and that one stays. */
export function focusReached(keyboard: boolean): HintStep {
  return keyboard ? { kind: "show" } : { kind: "none" };
}

/** The pointer moved while `shown` is up: a hint whose element went away or is no longer under it goes. */
export function pointerMoved(shown: { readonly isConnected: boolean; contains(node: unknown): boolean } | undefined, under: unknown): HintStep {
  if (!shown) return { kind: "none" };
  if (!shown.isConnected || !shown.contains(under)) return { kind: "hide", after: 0 };
  return { kind: "keep" };
}

const hintOf = (target: EventTarget | null): HTMLElement | undefined => {
  const element = target instanceof Element ? target.closest<HTMLElement>("[data-variable-name]") : null;
  return element && element.dataset.variableName ? element : undefined;
};

/** Whether focus is the keyboard's, which the platform marks as visible. Unknown counts as not. */
const keyboardFocused = (element: Element): boolean => {
  try { return element.matches(":focus-visible"); } catch { return false; }
};

export function Hints() {
  const id = useId();
  const [shown, setShown] = useState<{ readonly element: HTMLElement; readonly text: string }>();
  const [place, setPlace] = useState<HintPlace>();

  useEffect(() => {
    // Without a document to listen at (a test renderer), there is nothing to follow.
    if (typeof document === "undefined" || typeof document.addEventListener !== "function") return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let stay: ReturnType<typeof setTimeout> | undefined;
    let current: HTMLElement | undefined;
    const clear = () => { if (timer) clearTimeout(timer); timer = undefined; };
    const hide = () => { clear(); if (stay) clearTimeout(stay); stay = undefined; current = undefined; setShown(undefined); setPlace(undefined); };
    const showNow = (element: HTMLElement, by: "pointer" | "keyboard") => {
      hide();
      current = element; setShown({ element, text: element.dataset.variableName ?? "" });
      if (by === "pointer") stay = setTimeout(hide, HINT_STAY_MS);
    };
    const follow = (step: HintStep, element: HTMLElement | undefined, by: "pointer" | "keyboard") => {
      if (step.kind === "keep") { clear(); return; }
      if (step.kind === "none") return;
      if (step.kind === "show") { if (element) showNow(element, by); return; }
      clear();
      if (step.kind === "arm") { if (element) timer = setTimeout(() => showNow(element, by), step.after); return; }
      if (step.after === 0) hide(); else timer = setTimeout(hide, step.after);
    };
    const over = (event: MouseEvent) => { const element = hintOf(event.target); follow(pointerOver(current, element), element, "pointer"); };
    const out = (event: MouseEvent) => {
      const element = hintOf(event.target);
      if (!element) return;
      if (event.relatedTarget instanceof Node && element.contains(event.relatedTarget)) return;
      clear();
      if (current === element) timer = setTimeout(hide, HINT_LINGER_MS);
    };
    const move = (event: MouseEvent) => { if (current) follow(pointerMoved(current, event.target instanceof Node ? event.target : null), undefined, "pointer"); };
    const focusIn = (event: FocusEvent) => {
      const element = hintOf(event.target);
      if (element) follow(focusReached(keyboardFocused(element)), element, "keyboard");
    };
    const focusOut = (event: FocusEvent) => { if (hintOf(event.target) === current) hide(); };
    document.addEventListener("mouseover", over);
    document.addEventListener("mouseout", out);
    document.addEventListener("mousemove", move);
    document.addEventListener("focusin", focusIn);
    document.addEventListener("focusout", focusOut);
    document.addEventListener("mousedown", hide, true);
    document.addEventListener("keydown", hide, true);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("resize", hide);
    return () => {
      hide();
      document.removeEventListener("mouseover", over);
      document.removeEventListener("mouseout", out);
      document.removeEventListener("mousemove", move);
      document.removeEventListener("focusin", focusIn);
      document.removeEventListener("focusout", focusOut);
      document.removeEventListener("mousedown", hide, true);
      document.removeEventListener("keydown", hide, true);
      window.removeEventListener("scroll", hide, true);
      window.removeEventListener("resize", hide);
    };
  }, []);

  // The hint's own size is known only once drawn: place it invisibly first, then where it fits.
  const measure = (node: HTMLDivElement | null) => {
    if (!node || !shown || place) return;
    setPlace(placeHint(shown.element.getBoundingClientRect(), { width: node.offsetWidth, height: node.offsetHeight }, { width: window.innerWidth, height: window.innerHeight }));
  };
  useEffect(() => {
    if (!shown) return;
    shown.element.setAttribute("aria-describedby", id);
    return () => shown.element.removeAttribute("aria-describedby");
  }, [shown, id]);

  if (!shown || typeof document === "undefined" || !document.body) return null;
  return createPortal(
    <div id={id} ref={measure} role="tooltip" className="variable-name-tooltip surface-hint"
      style={place ? { left: place.left, top: place.top } : { left: 0, top: 0, visibility: "hidden" }}>{shown.text}</div>,
    document.body,
  );
}
