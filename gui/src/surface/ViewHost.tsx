import { useLayoutEffect, useRef, useState, type ReactNode } from "react";

/** A cell owns output geometry. Views own content; the engine owns retained data. */
export function ViewHost({ stream = false, identity, mode, children, rows, onResize, resizeLabel }: {
  stream?: boolean; identity?: string; resizeLabel?: string; mode: string; children: ReactNode; rows?: number; onResize?: (rows: number | undefined) => void;
}) {
  const viewport = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const ratchet=useRef(0);
  const [bounds, setBounds] = useState({declared:false,min:6,max:40,preferred:14});
  useLayoutEffect(() => {
    const layout=content.current?.querySelector<HTMLElement>(".view-layout:not(.view-layout-nested)");
    const declared=Boolean(layout || content.current?.querySelector?.(".value-instance-block"));
    const min=Number(layout?.dataset.minRows)||6,max=Number(layout?.dataset.maxRows)||40,preferred=Number(layout?.dataset.preferredRows)||14;
    setBounds(old=>old.declared===declared && old.min===min && old.max===max && old.preferred===preferred ? old : {declared,min,max,preferred});
  });
  const bounded = mode === "expanded" && Boolean(onResize);
  const clamp = (value: number) => Math.max(bounds.min, Math.min(bounds.max, Math.round(value)));
  const rowHeight = () => {
    const line = viewport.current ? parseFloat(getComputedStyle(viewport.current).lineHeight) : 0;
    return Number.isFinite(line) && line > 0 ? line : 21;
  };
  const currentRows = () => rows ?? ((viewport.current?.getBoundingClientRect().height ?? 294) / rowHeight());

  useLayoutEffect(() => { ratchet.current=0;if(viewport.current)viewport.current.scrollTop=0; },[identity,mode]);
  useLayoutEffect(()=>{
    const box=viewport.current,body=content.current;
    if(!stream || mode!=="expanded" || rows!==undefined || !box || !body || body.querySelector?.(".log-view,.value-table-block,.value-instance-block"))return;
    const measure=()=>{ratchet.current=Math.min(14*rowHeight(),Math.max(ratchet.current,body.getBoundingClientRect().height));box.style.height=`${ratchet.current}px`;};
    measure();const observer=typeof ResizeObserver==="undefined"?undefined:new ResizeObserver(measure);observer?.observe(body);return()=>observer?.disconnect();
  },[stream,mode,rows,children]);
  return <div className={`view-host${bounded ? " view-host-resizable" : ""}${bounds.declared ? " view-host-declared" : ""}`} style={bounded && rows!==undefined ? {["--output-height" as string]:`${clamp(rows)}lh`} : undefined}>
    <div ref={viewport} className={`view-host-viewport${mode === "expanded" ? " view-host-expanded" : ""}${stream ? " view-host-stream" : ""}${bounded && !bounds.declared ? " view-host-builtin" : ""}`}
      style={bounded && rows !== undefined ? { height: `${clamp(rows)}lh`, maxHeight: `${bounds.max}lh`, ["--stream-rows" as string]:clamp(rows) } : undefined}
      role="group" aria-label="Data viewport">
      <div ref={content} className="view-host-content">{children}</div>
    </div>
    {bounded && <div tabIndex={0} className="result-resize" data-tooltip="Height · ↑↓ · Home: auto" role="slider" aria-orientation="vertical" aria-label={resizeLabel ? `Result height of ${resizeLabel}` : "Result height"} aria-valuemin={bounds.min} aria-valuemax={bounds.max} aria-valuenow={clamp(rows ?? currentRows())}
      aria-valuetext={rows === undefined ? `Automatic height, ${bounds.declared ? bounds.preferred : "up to 14"} rows` : `${rows} rows`} title="Drag to resize · arrow keys change height · Home resets · End for maximum"
      onKeyDown={event => {
        if (event.key === "Home") { event.preventDefault(); event.stopPropagation(); onResize?.(undefined); return; }
        const next = event.key === "End" ? bounds.max : event.key === "ArrowUp" ? currentRows() + 1 : event.key === "ArrowDown" ? currentRows() - 1 : undefined;
        if (next !== undefined) { event.preventDefault(); event.stopPropagation(); onResize?.(clamp(next)); }
      }}
      onPointerDown={event => {
        if (event.button !== 0) return;
        event.preventDefault(); event.stopPropagation();
        const grip = event.currentTarget, start = event.clientY, initial = currentRows(), line = rowHeight();
        grip.focus({ preventScroll: true });
        grip.setPointerCapture(event.pointerId);
        const move = (next: PointerEvent) => { if(next.pointerId===event.pointerId) onResize?.(clamp(initial + (next.clientY - start) / line)); };
        const stop = () => { grip.removeEventListener("pointermove", move); grip.removeEventListener("lostpointercapture", stop); grip.removeEventListener("pointerup", stop); grip.removeEventListener("pointercancel", stop); if(grip.hasPointerCapture(event.pointerId))grip.releasePointerCapture(event.pointerId); };
        grip.addEventListener("pointermove", move);
        grip.addEventListener("lostpointercapture", stop);
        grip.addEventListener("pointerup", stop);
        grip.addEventListener("pointercancel", stop);
      }} />}
  </div>;
}
