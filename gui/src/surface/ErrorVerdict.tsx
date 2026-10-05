import { useLayoutEffect, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import { MonoLine, type Segment } from "./MonoLine";

/**
 * A failed verdict stays compact until its actual clipped text is asked for.
 *
 * `⌘click` on the message — the same modifier the command line uses for its source, on a different
 * element — grows it in place and again shrinks it; Enter or Space does the same for a focused
 * message. A plain click and a drag to select on the message never toggle, so copying works as it
 * always did; the visible toggle beside it is the discoverable way to do the same. Only a message
 * that is clipped or expanded offers the toggle, so a short failure keeps its one compact line.
 */
export function ErrorVerdict({ segments, onPeek }: { readonly segments: readonly Segment[]; /** ⌘click opens the whole failure in a plain window instead of growing it in place. */ readonly onPeek?: () => void }) {
  const line = useRef<HTMLPreElement>(null);
  const [clipped, setClipped] = useState(false);
  const [expanded, setExpanded] = useState(false);

  // Theme/font changes can alter text width without changing the observed box.
  useLayoutEffect(() => {
    const element = line.current;
    if (!expanded && element && element.clientWidth > 0) {
      setClipped(element.scrollWidth > element.clientWidth);
    }
  });

  useLayoutEffect(() => {
    const element = line.current;
    if (!element || expanded) return;
    let active = true;
    const measure = () => {
      if (active && element.clientWidth > 0) setClipped(element.scrollWidth > element.clientWidth);
    };
    measure();
    if (typeof ResizeObserver === "undefined") return () => { active = false; };
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => { active = false; observer.disconnect(); };
  }, [expanded]);

  const toggle = () => setExpanded(was => !was);
  const interactive = clipped || expanded || onPeek !== undefined;
  const onClick = (event: MouseEvent<HTMLElement>) => {
    if (!event.metaKey || event.ctrlKey || event.altKey || event.shiftKey || event.button !== 0) return;
    event.preventDefault(); event.stopPropagation();
    if (onPeek) onPeek(); else toggle();
  };
  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if ((event.key !== "Enter" && event.key !== " ") || event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
    // Space is also the cell's collapse key; the focused message answers first.
    event.preventDefault(); event.stopPropagation(); toggle();
  };

  return <div className={`cell-verdict-failure${expanded ? " cell-verdict-failure-expanded" : ""}`}>
    <div className="cell-verdict-disclosure"
      {...(interactive ? {
        role: "button", tabIndex: 0, "aria-expanded": expanded, onClick, onKeyDown,
        "aria-description": onPeek ? "⌘click to open the whole failure · Enter grows it here" : `⌘click to ${expanded ? "shorten" : "show all of"} the message · Enter when focused`,
      } : {})}>
      <MonoLine segments={segments} innerRef={line} className={`cell-verdict${expanded ? " cell-verdict-expanded" : ""}`} />
    </div>
    {(clipped || expanded) && <button type="button" className="cell-action cell-verdict-toggle" aria-expanded={expanded}
      onClick={event => { event.stopPropagation(); toggle(); }}>{expanded ? "▾ fold message" : "▸ show full message"}</button>}
  </div>;
}
