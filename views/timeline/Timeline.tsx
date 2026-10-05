import { memo, useEffect, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import type {State,Event} from "./contract";
interface TimePort { readonly state:State; readonly emit:(event:Event)=>void }
import { eventRef, sameItem, sampleRef, TIMELINE_LIMITS, type TimelineModel, type Sample, type EventRecord, type ItemRef } from "./model";
import { displayViewport } from "./navigation";
import { atRatio, clock, contains, fitRange, fitNanos, instant, nanos, ratio, span, type TimeRange } from "@wes/view-sdk";
import "./timeline.css";

/** Geometry in CSS pixels. Every coordinated member uses the same gutter rule so plots align. */
const GUTTER = 100, NARROW_GUTTER = 64, NARROW_WIDTH = 520, RIGHT = 24;
const PLOT_HEIGHT = { standalone: { preferred: 180, floor: 120 }, member: { preferred: 96, floor: 88 } } as const;
const LANE = 44, AXIS = 24, PAD = 8, MARKER_GAP = 10, LABEL_CHAR = 7.2, CLUSTER_LABELS = 8;
const COLORS = ["var(--info)", "var(--primary)", "var(--warning)", "var(--success)"];
const US = 1_000n, MS = 1_000_000n, S = 1_000_000_000n, MIN = 60n * S, H = 60n * MIN, DAY = 24n * H;
const TIME_STEPS = [
  ...[1n, US, MS].flatMap(unit => [1n, 2n, 5n, 10n, 20n, 50n, 100n, 200n, 500n].map(k => k * unit)),
  ...[1n, 2n, 5n, 10n, 15n, 30n].map(k => k * S), ...[1n, 2n, 5n, 10n, 15n, 30n].map(k => k * MIN),
  ...[1n, 2n, 3n, 6n, 12n].map(k => k * H), ...[1n, 2n, 7n, 14n, 30n, 90n, 180n, 365n].map(k => k * DAY),
];
const PLOT_HELP = "Drag or press Shift with the arrow keys to select an interval. Arrow keys move the cursor, + and − zoom, Escape cancels a drag or clears the selection. Times are UTC; intervals include the start and exclude the end.";

export const gutterFor = (width: number) => width < NARROW_WIDTH ? NARROW_GUTTER : GUTTER;
export const xAt = (t: bigint, viewport: TimeRange, width = 1000, left = gutterFor(width)) => left + ratio(t, viewport) * (width - RIGHT - left);
export function lowerBound<T extends { t: bigint }>(items: readonly T[], t: bigint): number {
  let a = 0, b = items.length; while (a < b) { const m = (a + b) >>> 1; if (items[m]!.t < t) a = m + 1; else b = m; } return a;
}
function nearest(samples: readonly Sample[], at: bigint): Sample | undefined {
  const i = lowerBound(samples, at), a = samples[i - 1], b = samples[i];
  return !a ? b : !b ? a : at - a.t <= b.t - at ? a : b;
}
const floorDiv = (a: bigint, b: bigint) => a / b - (a % b < 0n ? 1n : 0n);
const labelChars = (step: bigint) => step >= DAY ? 10 : step >= MIN ? 5 : step >= S ? 8 : step >= MS ? 12 : step >= US ? 15 : 18;
/** Tick instants on whole steps; the step is the smallest whose labels do not collide. */
export function timeTicks(viewport: TimeRange, plotWidth: number): { step: bigint; ticks: bigint[] } {
  const start = nanos(viewport.start)!, end = nanos(viewport.end)!, size = end - start;
  if (size === 0n) return { step: 1n, ticks: [start] };
  const fits = (step: bigint) => size / step <= BigInt(Math.max(1, Math.floor(plotWidth / (labelChars(step) * 6.8 + 24))));
  const step = TIME_STEPS.find(fits) ?? (size / DAY / 4n + 1n) * DAY, ticks: bigint[] = [];
  for (let t = -floorDiv(-start, step) * step; t <= end && ticks.length < 100; t += step) ticks.push(t);
  return { step, ticks };
}
export function tickLabel(t: bigint, step: bigint): string {
  const at = instant(t);
  if (step >= DAY) return at.slice(0, at.indexOf("T"));
  const [time, fraction = ""] = clock(at).split(".");
  if (step >= MIN) return time!.slice(0, 5);
  if (step >= S) return time!;
  return `${time}.${fraction.padEnd(9, "0").slice(0, step >= MS ? 3 : step >= US ? 6 : 9)}`;
}
/** Display-only duration; exact instants stay in labels and inspection. */
export function duration(ns: bigint): string {
  const a = ns < 0n ? -ns : ns;
  const [unit, size] = a < US ? ["ns", 1n] : a < MS ? ["µs", US] : a < S ? ["ms", MS] : a < MIN ? ["s", S] : a < H ? ["min", MIN] : a < DAY ? ["h", H] : ["d", DAY];
  return `${new Intl.NumberFormat("en-US", { maximumSignificantDigits: 3 }).format(Number(a) / Number(size))} ${unit}`;
}
function rangeLabel(r: TimeRange) {
  const sameDay = r.start.slice(0, r.start.indexOf("T")) === r.end.slice(0, r.end.indexOf("T"));
  return `${sameDay ? `${clock(r.start)}–${clock(r.end)}` : `${r.start} – ${r.end}`} UTC · ${duration(span(r))}`;
}

interface ValueScale { readonly lo: number; readonly hi: number; readonly ticks: readonly number[] }
function valueScale(model: TimelineModel, plotHeight: number): ValueScale {
  let lo = Infinity, hi = -Infinity;
  for (const series of model.series) for (const point of series.samples) if (point.value !== null) { lo = Math.min(lo, point.value); hi = Math.max(hi, point.value); }
  if (!Number.isFinite(lo)) { lo = 0; hi = 1; }
  if (lo >= 0 && lo <= hi / 2) lo = 0;
  if (lo === hi) { lo -= Math.max(1, Math.abs(lo) * .1); hi += Math.max(1, Math.abs(hi) * .1); }
  const raw = (hi - lo) / Math.max(2, Math.floor(plotHeight / 36)), magnitude = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map(k => k * magnitude).find(k => k >= raw)!;
  lo = Math.floor(lo / step) * step; hi = Math.ceil(hi / step) * step;
  const ticks: number[] = []; for (let i = 0; lo + i * step <= hi + step / 2 && i < 16; i++) ticks.push(lo + i * step);
  return { lo, hi, ticks };
}
function valueLabel(v: number, narrow: boolean) {
  if (v !== 0 && Math.abs(v) < 1e-3) return v.toExponential(1);
  return new Intl.NumberFormat("en-US", { maximumSignificantDigits: 4, notation: narrow || Math.abs(v) >= 1e5 ? "compact" : "standard" }).format(v);
}
const yAt = (v: number, scale: ValueScale, plotHeight: number) => PAD + (scale.hi - v) / (scale.hi - scale.lo) * (plotHeight - 2 * PAD);

/** Path geometry is memoized separately from cursor/selection updates. Null samples break the path. */
const SeriesPaths = memo(function SeriesPaths({ model, viewport, scale, width, left, plotHeight }: { model: TimelineModel; viewport: TimeRange; scale: ValueScale; width: number; left: number; plotHeight: number }) {
  return <g className="timeline-geometry">
    {model.series.map((series, si) => {
      const begin = Math.max(0, lowerBound(series.samples, nanos(viewport.start)!) - 1);
      const end = Math.min(series.samples.length, lowerBound(series.samples, nanos(viewport.end)!) + 1);
      let d = "", drawing = false;
      for (let i = begin; i < end; i++) {
        const p = series.samples[i]!;
        if (p.value === null) { drawing = false; continue; }
        d += `${drawing ? "L" : "M"}${xAt(p.t, viewport, width, left).toFixed(2)},${yAt(p.value, scale, plotHeight).toFixed(2)}`; drawing = true;
      }
      return <path key={series.id} d={d} stroke={COLORS[si % COLORS.length]} className="timeline-series" />;
    })}
  </g>;
});
function zoom(viewport: TimeRange, factor: bigint, divide: bigint, limit: TimeRange) {
  const size = span(viewport) * factor / divide, middle = nanos(viewport.start)! + span(viewport) / 2n;
  return fitNanos(middle - size / 2n, middle + (size + 1n) / 2n, limit);
}
function useWidth<T extends Element>() {
  const ref = useRef<T>(null); const [width, setWidth] = useState(1000);
  useEffect(() => {
    const node = ref.current; if (!node || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(entries => { const w = entries[0]?.contentRect.width; if (w && w > 0) setWidth(Math.max(160, Math.round(w))); });
    observer.observe(node); return () => observer.disconnect();
  }, []);
  return { ref, width };
}

export function TimeToolbar({ range, port }: { range: TimeRange; port: TimePort }) {
  const viewport = displayViewport(port.state), empty = span(range) === 0n;
  const pan = (direction: bigint) => {
    const delta = span(viewport) / 4n * direction;
    port.emit({ kind: "viewport", range: fitNanos(nanos(viewport.start)! + delta, nanos(viewport.end)! + delta, range) });
  };
  return <div className="timeline-toolbar" role="group" aria-label="Time navigation">
    <button title="Fit the query range" onClick={() => port.emit({ kind: "viewport", range })} disabled={empty}>Fit</button>
    <button className="timeline-glyph" aria-label="Zoom out" title="Zoom out (−)" onClick={() => port.emit({ kind: "viewport", range: zoom(viewport, 2n, 1n, range) })} disabled={empty}>−</button>
    <button className="timeline-glyph" aria-label="Zoom in" title="Zoom in (+)" onClick={() => port.emit({ kind: "viewport", range: zoom(viewport, 1n, 2n, range) })} disabled={span(viewport) < 2n}>+</button>
    <button className="timeline-glyph" aria-label="Earlier" title="Move earlier" onClick={() => pan(-1n)} disabled={empty}>←</button>
    <button className="timeline-glyph" aria-label="Later" title="Move later" onClick={() => pan(1n)} disabled={empty}>→</button>
    <button title="Zoom to the selected interval" disabled={!port.state.selection || span(port.state.selection) === 0n} onClick={() => port.state.selection && port.emit({ kind: "viewport", range: fitRange(port.state.selection, range) })}>Zoom to selection</button>
    <button title="Clear the selected interval (Escape in a plot)" disabled={!port.state.selection} onClick={() => port.emit({ kind: "selection", range: null })}>Clear selection</button>
    <span className="timeline-range" title={`${viewport.start} to ${viewport.end}, half-open`}>{rangeLabel(viewport)}</span>
  </div>;
}

interface Cluster { readonly x0: number; x1: number; readonly items: EventRecord[] }
function labelCounts(items: readonly EventRecord[]) {
  const counts = new Map<string, number>(); for (const e of items) counts.set(e.label, (counts.get(e.label) ?? 0) + 1);
  return [...counts];
}
function clusterSummary(items: readonly EventRecord[]) {
  const counts = labelCounts(items), shown = counts.slice(0, CLUSTER_LABELS).map(([label, n]) => `${label}: ${n}`);
  if (counts.length > CLUSTER_LABELS) shown.push(`${counts.length - CLUSTER_LABELS} more labels`);
  return `${items.length} events · ${shown.join(" · ")}`;
}
function readoutText(model: TimelineModel, cursor: string | null, nearestSamples: readonly { series: TimelineModel["series"][number]; p: Sample }[]) {
  if (!cursor) return `${model.series.reduce((n, s) => n + s.samples.length, 0)} samples · ${model.events.length} ${model.events.length === 1 ? "event" : "events"}${model.series.some(s => s.samples.some(p => p.value === null)) ? " · gaps break the line" : ""}`;
  const head = `cursor ${clock(cursor)} UTC`;
  if (!contains(model.coverage, cursor)) return `${head} · not held by this source`;
  if (!nearestSamples.length) return `${head} · no samples`;
  return [head, ...nearestSamples.map(({ series, p }) => {
    const delta = p.t - nanos(cursor)!, offset = delta === 0n ? "at the cursor" : `${duration(delta)} ${delta < 0n ? "before" : "after"} the cursor`;
    const value = p.value === null ? "gap" : `${new Intl.NumberFormat("en-US", { maximumSignificantDigits: 6 }).format(p.value)}${series.unit ? ` ${series.unit}` : ""}`;
    return `${series.label}: ${value} · nearest sample ${clock(p.at)} UTC, ${offset}`;
  })].join(" · ");
}

export function TimelinePlot({ model, port, coordinated = false, control, onInspect }: { model: TimelineModel; port: TimePort; coordinated?: boolean; control?: ReactNode; onInspect?:()=>void }) {
  const { ref, width } = useWidth<SVGSVGElement>(), left = gutterFor(width), right = width - RIGHT;
  const viewport = useMemo(() => displayViewport(port.state), [port.state.viewport, model.range]);
  const geometry = PLOT_HEIGHT[coordinated ? "member" : "standalone"];
  const strip=coordinated && model.preview;
  const plotHeight = model.series.length ? strip ? 24 : model.preview ? geometry.floor : geometry.preferred : 0;
  const laneHeight = strip ? plotHeight ? 0 : 24 : model.events.length || !model.series.length ? LANE : 0, bodyHeight = plotHeight + laneHeight;
  const height = bodyHeight + (coordinated ? 0 : AXIS), laneMiddle = strip ? bodyHeight / 2 : plotHeight + LANE / 2;
  const scale = useMemo(() => valueScale(model, plotHeight), [model, plotHeight]);
  const time = useMemo(() => timeTicks(viewport, right - left), [viewport, right, left]);
  const x = (t: bigint) => xAt(t, viewport, width, left);
  const gesture = useRef<{ at: string; pointer: number }>();
  const atPointer = (e: PointerEvent<SVGSVGElement>) => {
    const box = e.currentTarget.getBoundingClientRect();
    return atRatio(viewport, (e.clientX - box.left - box.width * left / width) / (box.width * (right - left) / width));
  };
  const ordered = (a: string, b: string): TimeRange => nanos(a)! <= nanos(b)! ? { start: a, end: b } : { start: b, end: a };
  const cursor = port.state.cursor, selected = port.state.selectedItem;
  const clusters = useMemo(() => {
    const groups: Cluster[] = [];
    const start = lowerBound(model.events, nanos(viewport.start)!), end = lowerBound(model.events, nanos(viewport.end)!);
    for (let i = start; i < end; i++) {
      const event = model.events[i]!, at = xAt(event.t, viewport, width, left), last = groups[groups.length - 1];
      if (last && at - last.x1 < MARKER_GAP) { last.items.push(event); last.x1 = at; } else groups.push({ x0: at, x1: at, items: [event] });
    }
    return groups;
  }, [model, viewport, width, left]);
  const [rove, setRove] = useState<string | null>(null), [open, setOpen] = useState<string | null>(null);
  const markers = useRef<(SVGGElement | null)[]>([]),focusedMarker=useRef(false);
  useEffect(()=>{if(!focusedMarker.current || !rove)return;const index=clusters.findIndex(cluster=>cluster.items.some(event=>event.id===rove));if(index>=0)markers.current[index]?.focus();else {ref.current?.focus();focusedMarker.current=false;}},[clusters,rove]);
  const roveIndex = Math.max(0, clusters.findIndex(c => c.items.some(e => e.id === rove)));
  const openIndex = open === null ? -1 : clusters.findIndex(c => c.items.length > 1 && c.items.some(e => e.id === open));
  const openCluster = openIndex < 0 ? undefined : clusters[openIndex];
  const nearestSamples = useMemo(() => cursor === null || !contains(model.coverage, cursor) ? [] : model.series.flatMap(series => {
    const p = nearest(series.samples, nanos(cursor)!); return p ? [{ series, p }] : [];
  }), [model, cursor]);
  const activate = (cluster: Cluster) => {
    const first = cluster.items[0]!, last = cluster.items[cluster.items.length - 1]!;
    if (cluster.items.length > 1) { port.emit({ kind: "selection", range: { start: first.at, end: instant(last.t + 1n) } }); setOpen(first.id); }
    port.emit({ kind: "item", item: eventRef(model, first) }); setRove(first.id); onInspect?.();
  };
  const markerKey = (e: KeyboardEvent<SVGGElement>, index: number) => {
    if (e.key === "ArrowLeft" || e.key === "ArrowRight" || e.key === "Home" || e.key === "End") {
      const next = e.key === "Home" ? 0 : e.key === "End" ? clusters.length - 1 : Math.max(0, Math.min(clusters.length - 1, index + (e.key === "ArrowRight" ? 1 : -1)));
      setRove(clusters[next]!.items[0]!.id); markers.current[next]?.focus();
    } else if (e.key === "Enter" || e.key === " ") activate(clusters[index]!);
    else if (e.key === "Escape" && openCluster) setOpen(null);
    else return;
    e.preventDefault(); e.stopPropagation();
  };
  const band = (r: TimeRange) => { const a = Math.max(left, x(nanos(r.start)!)), b = Math.min(right, x(nanos(r.end)!)); return { x: a, width: Math.max(0, b - a) }; };
  const coverageStart = Math.min(right, Math.max(left, x(nanos(model.coverage.start)!))), coverageEnd = Math.max(left, Math.min(right, x(nanos(model.coverage.end)!)));
  const unheld = [{ x: left, width: coverageStart - left }, { x: coverageEnd, width: right - coverageEnd }].filter(r => r.width > 0);
  const readout = readoutText(model, cursor, nearestSamples);
  return <section className={`timeline-track${strip?" timeline-preview-strip":""}`} aria-label={model.title}>
    <header style={strip?{width:left}:undefined}><strong title={model.title}>{model.title}</strong><span className="timeline-units">{model.series.map(s => `${s.label}${s.unit ? ` · ${s.unit}` : ""}`).join(" · ")}</span>
      {model.omitted > 0 && <span className="status-warn">{model.omitted} omitted</span>}
      {control}
    </header>
    <svg ref={ref} className="timeline-svg" width="100%" height={height} viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" role="group" aria-label={`${model.title} timeline`} tabIndex={0}
      onPointerDown={e => { if (e.button !== 0 || span(viewport) === 0n) return; e.currentTarget.focus(); const at = atPointer(e); gesture.current = { at, pointer: e.pointerId }; e.currentTarget.setPointerCapture(e.pointerId); port.emit({ kind: "selection-preview", range: { start: at, end: at } }); }}
      onPointerMove={e => { const at = atPointer(e); port.emit({ kind: "cursor", at }); if (gesture.current) port.emit({ kind: "selection-preview", range: ordered(gesture.current.at, at) }); }}
      onPointerUp={e => { if (!gesture.current || gesture.current.pointer !== e.pointerId) return; const draft = ordered(gesture.current.at, atPointer(e)); gesture.current = undefined; e.currentTarget.releasePointerCapture(e.pointerId); port.emit({ kind: "selection", range: draft }); }}
      onPointerCancel={() => { gesture.current = undefined; port.emit({ kind: "selection-preview", range: null }); }}
      onLostPointerCapture={() => { if (gesture.current) { gesture.current = undefined; port.emit({ kind: "selection-preview", range: null }); } }}
      onPointerLeave={() => { if (!gesture.current) port.emit({ kind: "cursor", at: null }); }}
      onKeyDown={e => {
        if (span(viewport) === 0n && e.key !== "Escape") return;
        if (e.key === "Escape") { if (gesture.current) { gesture.current = undefined; port.emit({ kind: "selection-preview", range: null }); } else port.emit({ kind: "selection", range: null }); }
        else if (e.key === "+" || e.key === "-") port.emit({ kind: "viewport", range: zoom(viewport, e.key === "+" ? 1n : 2n, e.key === "+" ? 2n : 1n, model.range) });
        else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
          const current = cursor !== null && contains(viewport, cursor) ? cursor : viewport.start;
          const move = span(viewport) / 100n || 1n, t = nanos(current)! + (e.key === "ArrowRight" ? move : -move);
          const at = instant(t < nanos(viewport.start)! ? nanos(viewport.start)! : t > nanos(viewport.end)! ? nanos(viewport.end)! : t);
          port.emit({ kind: "cursor", at }); if (e.shiftKey) port.emit({ kind: "selection", range: ordered(port.state.selection?.start ?? current, at) });
        } else return;
        e.preventDefault(); e.stopPropagation();
      }}>
      <desc>{PLOT_HELP}</desc>
      <svg x={left} y={0} width={right - left} height={bodyHeight} viewBox={`${left} 0 ${right - left} ${bodyHeight}`} overflow="hidden">
        {plotHeight > 0 && <rect x={left} y={0} width={right - left} height={plotHeight} className="timeline-plot-bg" />}
        {time.ticks.map(t => <line key={String(t)} x1={x(t)} x2={x(t)} y1={0} y2={bodyHeight} className="timeline-grid" />)}
        {!strip && plotHeight > 0 && scale.ticks.map(v => <line key={v} x1={left} x2={right} y1={yAt(v, scale, plotHeight)} y2={yAt(v, scale, plotHeight)} className="timeline-grid" />)}
        {plotHeight > 0 && laneHeight > 0 && <line x1={left} x2={right} y1={plotHeight + .5} y2={plotHeight + .5} className="timeline-lane-rule" />}
        {!model.sourceError && <SeriesPaths model={model} viewport={viewport} scale={scale} width={width} left={left} plotHeight={plotHeight} />}
        {unheld.map(r => <g key={r.x}><rect x={r.x} y={0} width={r.width} height={bodyHeight} className="timeline-unheld" />{r.width >= 64 && <text x={r.x + 6} y={14}>not held</text>}</g>)}
        {[port.state.selection, port.state.draft].map((sel, i) => sel && <rect key={i} {...band(sel)} y={0} height={bodyHeight} className={i === 0 ? "timeline-selection" : "timeline-draft"} />)}
        {selected && contains(viewport, selected.at) && <line x1={x(nanos(selected.at)!)} x2={x(nanos(selected.at)!)} y1={0} y2={bodyHeight} className="timeline-guide" />}
        {cursor && contains(viewport, cursor) && <line x1={x(nanos(cursor)!)} x2={x(nanos(cursor)!)} y1={0} y2={bodyHeight} className="timeline-cursor" />}
        {nearestSamples.map(({ series, p }) => p.value !== null && <circle key={series.id} cx={x(p.t)} cy={yAt(p.value, scale, plotHeight)} r={3} className="timeline-nearest" />)}
        {model.series.map(series => {
          const p = selected?.source === model.id && selected.series === series.id ? series.samples.find(p => sameItem(selected, sampleRef(model, series, p))) : undefined;
          return p && p.value !== null && contains(viewport, p.at) ? <circle key={series.id} cx={x(p.t)} cy={yAt(p.value, scale, plotHeight)} r={5} className="timeline-picked" aria-label="Selected sample" /> : null;
        })}
        {model.sourceError && plotHeight > 0 && <text x={left + 6} y={plotHeight / 2 + 4}>Source error · samples not drawn</text>}
        {!model.series.length && !model.events.length && <text x={left + 6} y={laneMiddle + 4}>No samples or events in this snapshot</text>}
        {clusters.map((cluster, index) => {
          const first = cluster.items[0]!, last = cluster.items[cluster.items.length - 1]!, many = cluster.items.length > 1;
          const chosen = cluster.items.some(e => sameItem(selected, eventRef(model, e))), labels = labelCounts(cluster.items);
          const capsule = Math.max(cluster.x1 - cluster.x0 + 12, String(cluster.items.length).length * LABEL_CHAR + 12);
          const end = many ? cluster.x0 - 6 + capsule : cluster.x0 + 6, next = clusters[index + 1]?.x0 ?? right + 6;
          const chars = Math.floor((next - end - 16) / LABEL_CHAR), label = labels.length === 1 ? first.label : "";
          return <g key={first.id} ref={node => { markers.current[index] = node; }} className="timeline-marker" role="button" onFocus={()=>{focusedMarker.current=true;setRove(first.id);}} onBlur={event=>{if(!event.currentTarget.ownerSVGElement?.contains(event.relatedTarget as Node|null))focusedMarker.current=false;}} tabIndex={index === roveIndex ? 0 : -1}
            aria-label={many ? `${clusterSummary(cluster.items)} · ${first.at} to ${last.at}` : `${first.at} ${first.label}`} aria-pressed={chosen} aria-expanded={many ? openCluster === cluster : undefined}
            onPointerDown={e => e.stopPropagation()} onClick={() => activate(cluster)} onKeyDown={e => markerKey(e, index)}>
            <rect x={cluster.x0 - 8} y={strip?0:plotHeight + 4} width={many ? capsule + 4 : 16} height={strip?bodyHeight:LANE - 8} className="timeline-hit" />
            {many
              ? <><rect x={cluster.x0 - 6} y={laneMiddle - 9} width={capsule} height={18} rx={4} className={`timeline-cluster${labels.length > 1 ? " mixed" : ""}${chosen ? " selected" : ""}${openCluster === cluster ? " open" : ""}`} />
                <text x={cluster.x0 - 6 + capsule / 2} y={laneMiddle + 4} textAnchor="middle" className="timeline-count">{cluster.items.length}</text></>
              : <path d={`M${cluster.x0},${laneMiddle - 6} l6,6 l-6,6 l-6,-6 Z`} className={chosen ? "timeline-event selected" : "timeline-event"} />}
            {label && chars >= 4 && <text x={end + 6} y={laneMiddle + 4} className="timeline-event-label">{label.length <= chars ? label : `${label.slice(0, chars - 1)}…`}</text>}
          </g>;
        })}
      </svg>
      {!strip && plotHeight > 0 && scale.ticks.map(v => <text key={v} x={left - 8} y={yAt(v, scale, plotHeight) + 4} textAnchor="end">{valueLabel(v, left === NARROW_GUTTER)}</text>)}
      {laneHeight > 0 && model.series.length > 0 && <text x={left - 8} y={laneMiddle + 4} textAnchor="end">events</text>}
      {!coordinated && <g className="timeline-time-axis">
        <line x1={left} x2={right} y1={bodyHeight + .5} y2={bodyHeight + .5} className="timeline-grid" />
        {time.ticks.map(t => <g key={String(t)}><line x1={x(t)} x2={x(t)} y1={bodyHeight} y2={bodyHeight + 4} className="timeline-grid" /><text x={x(t)} y={bodyHeight + 17} textAnchor="middle">{tickLabel(t, time.step)}</text></g>)}
      </g>}
    </svg>
    {openCluster && !onInspect && <ClusterList model={model} port={port} items={openCluster.items} close={() => { setOpen(null); markers.current[openIndex]?.focus(); }} />}
    {(!strip || model.sourceError) && <div className="timeline-readout" title={model.sourceError ? undefined : readout}>{model.sourceError ? <><span className="status-bad">source error</span> {model.sourceError}</> : readout}</div>}
  </section>;
}
/** The real members of one collision cluster, bounded like the record inspector. */
function ClusterList({ model, port, items, close }: { model: TimelineModel; port: TimePort; items: readonly EventRecord[]; close: () => void }) {
  const shown = items.slice(0, TIMELINE_LIMITS.detailRows);
  return <div className="timeline-cluster-list" role="region" aria-label={clusterSummary(items)} onKeyDown={e => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(); } }}>
    <header><span title={clusterSummary(items)}>{clusterSummary(items)}</span><button onClick={close} title="Close the cluster list (Escape)">Close</button></header>
    <ol>{shown.map(e => { const ref = eventRef(model, e); return <li key={e.id}><button className="timeline-cluster-item" aria-pressed={sameItem(port.state.selectedItem, ref)} title={e.at} onClick={() => port.emit({ kind: "item", item: ref })}><time>{clock(e.at)}</time><span>{e.label}</span></button></li>; })}</ol>
    {items.length > shown.length && <p className="timeline-note"><span className="status-warn">{items.length - shown.length} more</span> zoom to the selection to split this cluster</p>}
  </div>;
}
interface InspectionRow { readonly ref: ItemRef; readonly label: string; readonly detail: string }
export function TimelineInspection({ model, port }: { model: TimelineModel; port: TimePort }) {
  const selection = port.state.selection;
  const list = useMemo(() => {
    const range = selection ?? model.range;
    const rows: InspectionRow[] = []; let total = 0;
    const eventStart = lowerBound(model.events, nanos(range.start)!), eventEnd = lowerBound(model.events, nanos(range.end)!);
    const sampleCap = TIMELINE_LIMITS.detailRows - Math.min(100, eventEnd - eventStart);
    for (const series of model.series) {
      const a = lowerBound(series.samples, nanos(range.start)!), b = lowerBound(series.samples, nanos(range.end)!); total += b - a;
      for (let i = a; i < b && rows.length < sampleCap; i++) { const p = series.samples[i]!; rows.push({ ref: sampleRef(model, series, p), label: `${series.label}: ${p.value === null ? "gap" : p.value}${p.value === null || !series.unit ? "" : ` ${series.unit}`}`, detail: "" }); }
    }
    total += eventEnd - eventStart;
    for (let i = eventStart; i < eventEnd && rows.length < TIMELINE_LIMITS.detailRows; i++) { const e = model.events[i]!; rows.push({ ref: eventRef(model, e), label: e.label, detail: e.detail }); }
    return { rows, total };
  }, [model, selection]);
  const selected = useMemo(() => {
    const ref = port.state.selectedItem; if (!ref || ref.source !== model.id) return undefined;
    if (ref.series === "") { const e = model.events.find(e => sameItem(ref, eventRef(model, e))); return e ? { ref, label: e.label, detail: e.detail } : null; }
    const series = model.series.find(s => s.id === ref.series), sample = series?.samples.find(p => sameItem(ref, sampleRef(model, series, p)));
    return series && sample ? { ref, label: `${series.label}: ${sample.value ?? "gap"}${sample.value === null || !series.unit ? "" : ` ${series.unit}`}`, detail: "" } : null;
  }, [model, port.state.selectedItem]);
  const viewport = displayViewport(port.state);
  return <div className="timeline-inspection">
    <div className="timeline-records"><strong>{list.total} records{selection ? " in selection" : ""}</strong>
      {list.rows.map(row => <button key={JSON.stringify([row.ref.series, row.ref.id])} className="timeline-record" title={row.ref.at} aria-pressed={sameItem(port.state.selectedItem, row.ref)} onClick={() => port.emit({ kind: "item", item: row.ref })}><time>{clock(row.ref.at)}</time><span>{row.label}</span></button>)}
      {list.total > list.rows.length && <p className="timeline-note"><span className="status-warn">{list.total - list.rows.length} more</span> narrow the selection to inspect</p>}
    </div>
    <div className="timeline-record-detail">{selected === null ? <p>Selected record is no longer in this snapshot.</p> : selected ? <>
      <strong>Selected {selected.ref.series ? "sample" : "event"}</strong><p className="timeline-instant">{selected.ref.at}</p><p>{selected.label}</p>{selected.detail && <p>{selected.detail}</p>}
      <div className="timeline-detail-actions">
        {!contains(viewport, selected.ref.at) && <button onClick={() => {
          const size = span(viewport) || 1n, start = nanos(selected.ref.at)! - size / 2n;
          port.emit({ kind: "viewport", range: fitNanos(start, start + size, model.range) });
        }}>Go to selected</button>}
        <button onClick={() => port.emit({ kind: "item", item: null })}>Clear record</button>
      </div>
    </> : <p>Select a sample or event to inspect it. Hover moves the cursor without changing the selection.</p>}</div>
  </div>;
}
export function TimelineView({ model, interaction, coordinated=false, onInspect, inspectionActive=false }: {model:TimelineModel;interaction:TimePort;coordinated?:boolean;onInspect?:()=>void;inspectionActive?:boolean}) {
  const [inspect,setInspect]=useState(false);
  const inspection = !model.preview && (!coordinated || inspect && !onInspect);
  return <div className={`timeline-view${coordinated?" timeline-member":""}`}>
    {!coordinated && <TimeToolbar range={model.range} port={interaction} />}
    <div className={`timeline-body${inspection && !coordinated ? " timeline-split" : ""}`}>
      <TimelinePlot model={model} port={interaction} coordinated={coordinated} onInspect={onInspect} control={coordinated && !model.preview ? <button aria-label={`Inspect ${model.title}`} title="Show this source's records" aria-expanded={onInspect ? inspectionActive : inspect} onClick={()=>onInspect ? onInspect() : setInspect(value=>!value)}>Inspect</button> : undefined}/>
      {inspection && <TimelineInspection model={model} port={interaction} />}
    </div>
  </div>;
}
