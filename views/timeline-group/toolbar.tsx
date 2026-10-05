import {clock,contains,fitNanos,fitRange,instant,nanos,ratio,span,type TimeRange} from "@wes/view-sdk";
import type {State,Event} from "./contract";
type TimePort={state:State;emit:(event:Event)=>void};
const displayViewport=(state:State)=>state.viewport;

/** Mirrors the Timeline member geometry: equal gutters and the same tick rule keep the shared axis aligned. */
const GUTTER=100, NARROW_GUTTER=64, NARROW_WIDTH=520, RIGHT=24, AXIS=24;
const US=1_000n, MS=1_000_000n, S=1_000_000_000n, MIN=60n*S, H=60n*MIN, DAY=24n*H;
const TIME_STEPS=[
  ...[1n,US,MS].flatMap(unit=>[1n,2n,5n,10n,20n,50n,100n,200n,500n].map(k=>k*unit)),
  ...[1n,2n,5n,10n,15n,30n].map(k=>k*S),...[1n,2n,5n,10n,15n,30n].map(k=>k*MIN),
  ...[1n,2n,3n,6n,12n].map(k=>k*H),...[1n,2n,7n,14n,30n,90n,180n,365n].map(k=>k*DAY),
];
export const gutterFor=(width:number)=>width<NARROW_WIDTH?NARROW_GUTTER:GUTTER;
const floorDiv=(a:bigint,b:bigint)=>a/b-(a%b<0n?1n:0n);
const labelChars=(step:bigint)=>step>=DAY?10:step>=MIN?5:step>=S?8:step>=MS?12:step>=US?15:18;
function timeTicks(viewport:TimeRange,plotWidth:number){
  const start=nanos(viewport.start)!,end=nanos(viewport.end)!,size=end-start;
  if(size===0n)return {step:1n,ticks:[start]};
  const fits=(step:bigint)=>size/step<=BigInt(Math.max(1,Math.floor(plotWidth/(labelChars(step)*6.8+24))));
  const step=TIME_STEPS.find(fits)??(size/DAY/4n+1n)*DAY,ticks:bigint[]=[];
  for(let t=-floorDiv(-start,step)*step;t<=end&&ticks.length<100;t+=step)ticks.push(t);
  return {step,ticks};
}
function tickLabel(t:bigint,step:bigint){
  const at=instant(t);
  if(step>=DAY)return at.slice(0,at.indexOf("T"));
  const [time,fraction=""]=clock(at).split(".");
  if(step>=MIN)return time!.slice(0,5);
  if(step>=S)return time!;
  return `${time}.${fraction.padEnd(9,"0").slice(0,step>=MS?3:step>=US?6:9)}`;
}
export function duration(ns:bigint){
  const a=ns<0n?-ns:ns;
  const [unit,size]=a<US?["ns",1n]:a<MS?["µs",US]:a<S?["ms",MS]:a<MIN?["s",S]:a<H?["min",MIN]:a<DAY?["h",H]:["d",DAY];
  return `${new Intl.NumberFormat("en-US",{maximumSignificantDigits:3}).format(Number(a)/Number(size))} ${unit}`;
}
export function rangeLabel(r:TimeRange){
  const sameDay=r.start.slice(0,r.start.indexOf("T"))===r.end.slice(0,r.end.indexOf("T"));
  return `${sameDay?`${clock(r.start)}–${clock(r.end)}`:`${r.start} – ${r.end}`} UTC · ${duration(span(r))}`;
}
function zoom(viewport: TimeRange, factor: bigint, divide: bigint, limit: TimeRange) {
  const size = span(viewport) * factor / divide, middle = nanos(viewport.start)! + span(viewport) / 2n;
  return fitNanos(middle - size / 2n, middle + (size + 1n) / 2n, limit);
}
export function TimeToolbar({ range, port }: { range: TimeRange; port: TimePort }) {
  const viewport = displayViewport(port.state), empty = span(range) === 0n;
  const pan = (direction: bigint) => {
    const delta = span(viewport) / 4n * direction;
    port.emit({ kind: "viewport", range: fitNanos(nanos(viewport.start)! + delta, nanos(viewport.end)! + delta, range) });
  };
  return <div className="timeline-toolbar" role="group" aria-label="Shared time navigation">
    <button title="Fit the query range in every track" onClick={() => port.emit({ kind: "viewport", range })} disabled={empty}>Fit</button>
    <button className="timeline-glyph" aria-label="Zoom out" title="Zoom out every track (−)" onClick={() => port.emit({ kind: "viewport", range: zoom(viewport, 2n, 1n, range) })} disabled={empty}>−</button>
    <button className="timeline-glyph" aria-label="Zoom in" title="Zoom in every track (+)" onClick={() => port.emit({ kind: "viewport", range: zoom(viewport, 1n, 2n, range) })} disabled={span(viewport) < 2n}>+</button>
    <button className="timeline-glyph" aria-label="Earlier" title="Move every track earlier" onClick={() => pan(-1n)} disabled={empty}>←</button>
    <button className="timeline-glyph" aria-label="Later" title="Move every track later" onClick={() => pan(1n)} disabled={empty}>→</button>
    <button title="Zoom every track to the shared selection" disabled={!port.state.selection || span(port.state.selection) === 0n} onClick={() => port.state.selection && port.emit({ kind: "viewport", range: fitRange(port.state.selection, range) })}>Zoom to selection</button>
    <button title="Clear the shared selection (Escape in a plot)" disabled={!port.state.selection} onClick={() => port.emit({ kind: "selection", range: null })}>Clear selection</button>
  </div>;
}
/** The common time axis drawn from shared state only; member data stays inside each slot. */
export function SharedAxis({ state, width }: { state: State; width: number }) {
  const viewport=displayViewport(state),left=gutterFor(width),right=width-RIGHT,time=timeTicks(viewport,right-left);
  const x=(t:bigint)=>left+ratio(t,viewport)*(right-left);
  const band=(r:TimeRange)=>{const a=Math.max(left,x(nanos(r.start)!)),b=Math.min(right,x(nanos(r.end)!));return {x:a,width:Math.max(0,b-a)};};
  const mark=(at:string|null|undefined)=>at&&contains(viewport,at)?x(nanos(at)!):null;
  const cursor=mark(state.cursor),picked=mark(state.selectedItem?.at);
  return <svg className="timeline-axis" width="100%" height={AXIS} viewBox={`0 0 ${width} ${AXIS}`} preserveAspectRatio="none" aria-label={`Shared UTC axis · ${rangeLabel(viewport)}`} role="img">
    {state.selection&&<rect {...band(state.selection)} y={0} height={AXIS} className="timeline-axis-selection"/>}
    <line x1={left} x2={right} y1={.5} y2={.5} className="timeline-axis-rule"/>
    {time.ticks.map(t=><g key={String(t)}><line x1={x(t)} x2={x(t)} y1={0} y2={4} className="timeline-axis-rule"/><text x={x(t)} y={17} textAnchor="middle">{tickLabel(t,time.step)}</text></g>)}
    {picked!==null&&<line x1={picked} x2={picked} y1={0} y2={AXIS} className="timeline-axis-guide"/>}
    {cursor!==null&&<line x1={cursor} x2={cursor} y1={0} y2={AXIS} className="timeline-axis-cursor"/>}
  </svg>;
}
