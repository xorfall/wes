import {defineView,range,clock,span} from "@wes/view-sdk";
import {useEffect,useRef,useState,type ReactNode} from "react";
import {definition} from "./contract";
import {initial,reduce,outputs,eventOutputs} from "./navigation";
import {TimeToolbar,SharedAxis,duration,rangeLabel} from "./toolbar";
import type {State} from "./contract";
import "./view.css";

/** Members render in connection order inside one bounded scroller. The axis sits outside it and
 * subtracts the scrollbar so it spans exactly the width each member slot receives. */
function Tracks({members,state,hidden,focused}:{members:readonly ReactNode[];state:State;hidden:ReadonlySet<number>;focused:number|undefined}){
  const ref=useRef<HTMLDivElement>(null),[box,setBox]=useState({width:1000,scrollbar:0});
  useEffect(()=>{
    const node=ref.current;if(!node||typeof ResizeObserver==="undefined")return;
    const measure=()=>{const next={width:Math.max(160,node.clientWidth),scrollbar:node.offsetWidth-node.clientWidth};setBox(old=>old.width===next.width&&old.scrollbar===next.scrollbar?old:next);};
    const observer=new ResizeObserver(measure);observer.observe(node);measure();return ()=>observer.disconnect();
  },[]);
  return <>
    <div ref={ref} className="timeline-tracks" aria-label="Connected timelines">{members.map((member,index)=><div key={index} hidden={focused===undefined?hidden.has(index):focused!==index}><span className="timeline-source-number" aria-label={`Source ${index+1}`}>{index+1}</span>{member}</div>)}</div>
    <div className="timeline-group-axis" style={{paddingRight:box.scrollbar}}><SharedAxis state={state} width={Math.max(160,box.width-14)}/></div>
    <Readout state={state}/>
  </>;
}
/** Shared cursor, selection and picked item below the plots, so selecting never moves the tracks.
 * Two lines are reserved; anything longer scrolls inside this focusable box instead of being clipped. */
function Readout({state}:{state:State}){
  const selection=state.selection,picked=state.selectedItem;
  return <div className="timeline-group-readout" role="group" aria-label="Shared cursor, selection and picked item" tabIndex={0}>
    <span>{state.cursor?`cursor ${clock(state.cursor)} UTC · each track reads its own nearest sample`:"Times are UTC · intervals include the start and exclude the end"}</span>
    {selection&&<> · <span title={`${selection.start} to ${selection.end}`}>selection {clock(selection.start)}–{clock(selection.end)} · {duration(span(selection))}</span></>}
    {picked&&<> · <span title={picked.at}>picked {clock(picked.at)}</span></>}
  </div>;
}
export default defineView(definition,{initial,reduce,outputs,eventOutputs,Component:({input,state,emit,slots,context})=>{
  const members=slots.members??[],preview=context.mode==="preview";
  const [hidden,setHidden]=useState<ReadonlySet<number>>(new Set()),[focused,setFocused]=useState<number>(),[sourcesOpen,setSourcesOpen]=useState(false);
  useEffect(()=>{setHidden(new Set());setFocused(undefined);},[members.length]);
  const visible=members.length-[...hidden].filter(index=>index<members.length).length;
  const visibleCount=focused===undefined ? visible : 1;
  const overview=<p className="timeline-group-overview">
    <span>{members.length} connected {members.length===1?"timeline":"timelines"}{visibleCount!==members.length && ` · ${visibleCount} shown`}</span>
    <span title={`${state.viewport.start} to ${state.viewport.end}`}>{rangeLabel(state.viewport)}</span>
  </p>;
  const controls=members.length>1 && (!preview || sourcesOpen) && <div className="timeline-source-controls" role="group" aria-label="Local source visibility">
    <span>Sources</span>{members.map((_member,index)=><span key={index} className="timeline-source-control"><button aria-label={`Show source ${index+1}`} aria-pressed={!hidden.has(index)} title={`Toggle source ${index+1} in connection order; does not change the query`} onClick={()=>{setFocused(undefined);setHidden(was=>{const next=new Set(was);if(next.has(index))next.delete(index);else next.add(index);return next;});}}>{index+1}</button><button className="timeline-glyph" aria-label={`Focus source ${index+1}`} aria-pressed={focused===index} title="Focus this source locally" onClick={()=>setFocused(was=>was===index?undefined:index)}>⊙</button></span>)}
    {(hidden.size>0 || focused!==undefined) && <button onClick={()=>{setHidden(new Set());setFocused(undefined);}}>Show all</button>}
  </div>;
  return <section className={`timeline-view timeline-group timeline-group-${context.mode}`} aria-label={input.title}>
    <header className="timeline-group-head"><h2 className="screen-title" title={input.title}>{input.title}</h2>{preview?<><span className="timeline-preview-count">{visibleCount}/{members.length} sources{visibleCount>4 && ` · scroll ${visibleCount-4} more`}</span>{members.length>1 && <button aria-expanded={sourcesOpen} aria-label="Source visibility" onClick={()=>setSourcesOpen(was=>!was)}>Sources</button>}</>:<TimeToolbar range={range(input.range)!} port={{state,emit}}/>}</header>
    {/* Expanded and window: source controls, count and viewport share one wrapping row. Preview keeps its own layout. */}
    {preview?<>{overview}{controls}</>:<div className="timeline-group-bar">{controls}{overview}</div>}
    {members.length && !visibleCount?<p className="screen-label timeline-group-empty">All sources are hidden locally. Use Show all to return them.</p>:null}
    {members.length?<Tracks members={members} state={state} hidden={hidden} focused={focused}/>:<p className="screen-label timeline-group-empty">No Timeline instances are connected. Connect Timeline Views with :view connect.</p>}
  </section>;
}});
