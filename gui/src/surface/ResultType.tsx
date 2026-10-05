import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { TypeShape } from "../protocol";
import { ellipsizeEnd } from "../presentation/columns";
import { typeLine } from "../presentation/type-shape";
import { compactType, typeOutline, typeOutlineSegments } from "./result-type";

/** Disclosure is anchored to the result, independent of execution and value inspection. */
export function ResultType({ shape, label, identity, omitFieldCount = false, onOpenChange }: { shape?: TypeShape; label?: string; identity: string; omitFieldCount?: boolean; onOpenChange?: (opened: boolean) => void }) {
  const anchor = useRef<HTMLDivElement>(null), trigger = useRef<HTMLButtonElement>(null), dialog = useRef<HTMLDivElement>(null);
  const [columns, setColumns] = useState(40), id = useId();
  const [opened, setOpened] = useState(false);
  const [position, setPosition] = useState({ top: 0, left: 8, width: 480 });
  const full = shape ? typeOutline(shape) : label;
  const close = (restore = false) => { setOpened(false); onOpenChange?.(false); if (restore) trigger.current?.focus({ preventScroll: true }); };
  useLayoutEffect(() => {
    const element = anchor.current;
    if (!element) return;
    let active = true;
    const measure = () => {
      if (!active || !element.clientWidth) return;
      const font = getComputedStyle(element), canvas = document.createElement("canvas").getContext("2d");
      if (canvas) canvas.font = `${font.fontWeight} ${font.fontSize} ${font.fontFamily}`;
      const advance = canvas ? canvas.measureText("0".repeat(100)).width / 100 : parseFloat(font.fontSize) * .6;
      const button = trigger.current && getComputedStyle(trigger.current);
      const inset = button ? [button.paddingLeft,button.paddingRight,button.borderLeftWidth,button.borderRightWidth].reduce((sum,value) => sum + (parseFloat(value) || 0),0) : 8;
      if (advance > 0) setColumns(Math.max(1, Math.floor((element.clientWidth - inset) / advance)));
    };
    measure();
    const resize = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(measure);
    resize?.observe(element);
    const surface = element.closest(".wes-terminal");
    const styles = typeof MutationObserver === "undefined" ? undefined : new MutationObserver(measure);
    if (surface) styles?.observe(surface, { attributes: true, attributeFilter: ["style", "class"] });
    document.fonts?.addEventListener("loadingdone", measure);
    return () => { active = false; resize?.disconnect(); styles?.disconnect(); document.fonts?.removeEventListener("loadingdone", measure); };
  }, []);
  const place = useCallback(() => {
    if (!anchor.current) return;
    const rect = anchor.current.getBoundingClientRect();
    const pane = anchor.current.closest(".split-pane")?.getBoundingClientRect();
    const inset = 12, viewportWidth = Math.max(1, window.innerWidth - inset * 2);
    const paneFits = pane && pane.width - inset * 2 >= 180;
    const leftBound = paneFits ? Math.max(inset, pane.left + inset) : inset;
    const rightBound = paneFits ? Math.min(window.innerWidth - inset, pane.right - inset) : window.innerWidth - inset;
    const font = getComputedStyle(dialog.current?.querySelector("pre") ?? anchor.current);
    const canvas = document.createElement("canvas").getContext("2d");
    if (canvas) canvas.font = `${font.fontWeight} ${font.fontSize} ${font.fontFamily}`;
    const contentWidth = Math.max(...(full ?? "").split("\n").map(line => canvas?.measureText(line).width ?? line.length * 8));
    const titleWidth = (canvas?.measureText(`Type · ${identity}`).width ?? identity.length * 8) + 160;
    const width = Math.min(560, viewportWidth, Math.max(1, rightBound - leftBound), Math.max(contentWidth + 28, titleWidth));
    const height = Math.min(dialog.current?.getBoundingClientRect().height ?? 320, window.innerHeight - inset * 2);
    const top = rect.bottom + height + inset <= window.innerHeight ? rect.bottom + 4 : rect.top - height - 4;
    setPosition({ top: Math.max(inset, Math.min(top, window.innerHeight - height - inset)), left: Math.max(leftBound, Math.min(rect.left, rightBound - width)), width });
  }, [full, identity]);
  useLayoutEffect(() => {
    if (!opened) return;
    place();
    dialog.current?.querySelector<HTMLElement>("pre")?.focus({ preventScroll: true });
    const resize = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(place);
    if (dialog.current) resize?.observe(dialog.current);
    return () => resize?.disconnect();
  }, [opened, place]);
  useLayoutEffect(() => { if (opened) place(); }, [opened, place, columns]);
  useEffect(() => {
    if (!opened) return;
    const outside = (event: Event) => {
      const node = event.target as Node;
      if (!dialog.current?.contains(node) && !anchor.current?.contains(node)) {
        const blank = event.type === "pointerdown" && !(node instanceof Element && node.closest("button,a,input,textarea,select,[tabindex]"));
        close(blank);
      }
    };
    const scroll = (event: Event) => { if (!dialog.current?.contains(event.target as Node)) place(); };
    document.addEventListener("pointerdown", outside, true);
    document.addEventListener("focusin", outside, true);
    window.addEventListener("scroll", scroll, true);
    window.addEventListener("resize", place);
    return () => {
      document.removeEventListener("pointerdown", outside, true);
      document.removeEventListener("focusin", outside, true);
      window.removeEventListener("scroll", scroll, true);
      window.removeEventListener("resize", place);
    };
  }, [opened, place]);
  const portal = anchor.current?.closest(".wes-terminal");
  return <div ref={anchor} className="result-type-slot">
    {full && <button ref={trigger} type="button" className="cell-action result-type" aria-label={`Type of ${identity}`} aria-haspopup="dialog" aria-expanded={opened} aria-controls={opened ? id : undefined} onClick={() => {setOpened(!opened);onOpenChange?.(!opened);}}>
      {shape ? compactType(shape, Math.max(1, columns), omitFieldCount) : ellipsizeEnd(label ?? "Unknown", Math.min(40, Math.max(1, columns)))}
    </button>}
    {opened && portal && createPortal(<div ref={dialog} id={id} role="dialog" aria-label={`Full type of ${identity}`} tabIndex={-1} className="result-type-popover" style={{ top: position.top, left: position.left, width: position.width }}
      onKeyDown={event => {
        if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(true); }
        if (event.key === "Tab") {
          const region = dialog.current?.querySelector<HTMLElement>("pre");
          const tools = dialog.current?.querySelectorAll<HTMLButtonElement>("button");
          if (!region || !tools?.length) return;
          const order = [region,...tools], at = order.indexOf(event.target as HTMLElement);
          const next = at + (event.shiftKey ? -1 : 1);
          if (at < 0) return;
          if (next >= 0 && next < order.length) {
            event.preventDefault(); order[next]?.focus({preventScroll:true});
          } else {
            const headerButtons = Array.from(trigger.current?.closest(".result-header")?.querySelectorAll<HTMLButtonElement>('button:not(:disabled):not([tabindex="-1"])') ?? []);
            const cellButtons = Array.from(trigger.current?.closest(".cell")?.querySelectorAll<HTMLElement>('button:not(:disabled):not([tabindex="-1"]),a[href],[tabindex="0"]') ?? []).filter(element => !element.closest("[hidden],[inert]") && element.getClientRects().length);
            const destination = event.shiftKey ? trigger.current : headerButtons[headerButtons.indexOf(trigger.current!) + 1] ?? cellButtons[cellButtons.indexOf(trigger.current!) + 1] ?? trigger.current;
            if (destination) { event.preventDefault(); close(); destination.focus({preventScroll:true}); }
          }
        }
      }}>
      <div className="result-type-title"><span>Type · {identity}</span><button type="button" className="cell-action" onClick={() => { void navigator.clipboard?.writeText(shape ? typeLine(shape) : label ?? "").catch(() => undefined); }}>copy type</button><button type="button" className="cell-action" aria-label="Close full type" onClick={() => close(true)}>close</button></div>
      <pre tabIndex={0} aria-label="Type structure">{shape ? typeOutlineSegments(shape).map((segment, at) => <span key={at} className={segment.role}>{segment.text}</span>) : full}</pre>
    </div>, portal)}
  </div>;
}
