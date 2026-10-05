/**
 * The renderer's half of "width is an input": how many display columns fit an element.
 *
 * Measured from the element's width and the mono advance of its font (7.8 px at 13 px PT Mono),
 * re-measured on resize. A resize changes the context, which re-runs `present()` — never `prepare()`.
 * Without layout (tests, a hidden pane) it answers the fallback so the tree is still finite.
 */
import { useLayoutEffect, useState, type RefObject } from "react";

/** PT Mono's advance at the surface's 13 px mono size. */
export const MONO_ADVANCE = 7.8;
/** Columns assumed where nothing can be measured. */
export const FALLBACK_COLUMNS = 100;

const measured = new Map<string,number>();
export function invalidateMonoMeasurements():void {measured.clear();}

/** The advance of the mono face, measured once from a canvas when there is one. */
export function monoAdvance(element?:HTMLElement|null): number {
  let font = '13px "PT Mono", "JetBrains Mono", ui-monospace, Menlo, monospace';
  if(element && typeof getComputedStyle==='function') {
    const style=getComputedStyle(element);
    const family=style.getPropertyValue?.('--type-mono-family').trim();
    const size=style.getPropertyValue?.('--type-mono-size').trim()||style.fontSize;
    if(size && family)font=`${size} ${family}`;
  }
  const cached=measured.get(font);if(cached!==undefined)return cached;
  try {
    const context = typeof document !== "undefined" ? document.createElement("canvas").getContext("2d") : null;
    if (context) {
      context.font = font;
      const width = context.measureText("0".repeat(100)).width / 100;
      if (width > 4 && width < 40) {measured.set(font,width);return width;}
    }
  } catch {
    // no canvas: fall back to the design system's number
  }
  return MONO_ADVANCE;
}

export function useColumns(ref: RefObject<HTMLElement | null>, fallback = FALLBACK_COLUMNS): number {
  const [columns, setColumns] = useState(fallback);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measure = () => {
      const width = element.clientWidth;
      if (width > 0) setColumns(Math.max(8, Math.floor(width / monoAdvance(element))));
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    const fonts=typeof document==='undefined'?undefined:document.fonts;
    let live=true;
    const refresh=()=>{if(live){invalidateMonoMeasurements();measure();}};
    fonts?.addEventListener?.('loadingdone',refresh);
    void fonts?.ready?.then(refresh);
    return () => {live=false;observer.disconnect();fonts?.removeEventListener?.('loadingdone',refresh);};
  }, [ref]);
  return columns;
}
