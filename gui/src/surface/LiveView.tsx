import { StreamItemsContext, streamItemKeys } from "./render/stream-items";
import { createContext, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { Mode } from "../presentation/types";
import type { Engine } from "../engine";
import type { WorkspaceNode } from "../workspace";
import { liveReader, sourceScope, SOURCE_SCOPE_HELP, type DisplaySource, type HeldDisplay, type LiveSample } from "../live-view-reader";
import { ValueBlock } from "./render/ValueBlock";
import { isLogValue } from "./render/LogView";
import { useVisibility } from "./useVisibility";
import { resultAccess } from "./result-access";
import "./stream.css";

type Reason="manual"|"selection"|"reading";
type Display={shown:LiveSample;latest:LiveSample;held?:HeldDisplay;hold:(reason:Reason)=>void;clear:(reason:Reason)=>void;resume:()=>void;retry:()=>void;reasons:ReadonlySet<Reason>;generation:string;node:WorkspaceNode;engine:Engine;root:React.RefObject<HTMLDivElement>};
const Context=createContext<Display|undefined>(undefined);
export function StreamBoundary({stream,children}:{stream?:Omit<StreamDisplayProps,"children">;children:ReactNode}) {return stream ? <StreamDisplay {...stream}>{children}</StreamDisplay> : <>{children}</>;}
/** `cellHold` is false for a second presentation (a value pane): it reads and holds its own display
 * but never replaces or clears the hold its cell publishes for the inspector. */
export interface StreamDisplayProps {engine:Engine;generation:string;node:WorkspaceNode;children?:ReactNode;cellHold?:boolean}
const epoch=(sample:LiveSample)=>JSON.stringify(sample.metadata?.epochs);
/** A cell holds exactly the displayed immutable sample. Source execution is independent. */
export function StreamDisplay({engine,generation,node,children,cellHold=true}:StreamDisplayProps) {
  const [latest,setLatest]=useState<LiveSample>({revision:0});
  const latestRef=useRef(latest);latestRef.current=latest;
  const [held,setHeld]=useState<HeldDisplay>();
  const [reasons,setReasons]=useState<ReadonlySet<Reason>>(new Set());
  const [retry,setRetry]=useState(0);
  const root=useRef<HTMLDivElement>(null),drawn=useRef<LiveSample>({revision:0});
  const visible=useVisibility(root), failed=useRef(false);
  const owner=useMemo(()=>liveReader(engine),[engine]);
  const publish=(snapshot:HeldDisplay|undefined)=>{if(cellHold)owner.holdCell(node.id,generation,snapshot);};
  const permitted=resultAccess(node).live;
  const signature=JSON.stringify([generation,node.id,node.command,node.environment,node.private,node.doubt]);
  const signatureRef=useRef(signature);
  const valid=signatureRef.current===signature && permitted && !latest.withdrawn;
  const safeHeld=valid && held && epoch(held.sample)===epoch(latest) ? held : undefined;
  const shown=valid ? safeHeld?.sample ?? (latest.problemCode==="budget" ? {...latest,value:undefined} : latest) : {revision:0};
  useLayoutEffect(()=>{drawn.current=shown;});
  useEffect(()=>{
    signatureRef.current=signature;failed.current=false;setLatest({revision:0});setHeld(undefined);setReasons(new Set());
    publish(undefined);
    return()=>publish(undefined);
  },[signature,owner,node.id,generation,cellHold]);
  useEffect(()=>{
    if(!permitted){setLatest({revision:0});setHeld(undefined);setReasons(new Set());publish(undefined);return;}
    if(!visible || failed.current)return;
    return owner.watch(node.id,generation,next=>{
      if(next.problem)failed.current=true;
      if(next.withdrawn || drawn.current.metadata && epoch(drawn.current)!==epoch(next)) {setHeld(undefined);setReasons(new Set());publish(undefined);}
      setLatest(next);
    },latestRef.current);
  },[owner,node.id,generation,permitted,visible,retry,signature]);
  const hold=(reason:Reason)=>{
    if(!drawn.current.value || latest.withdrawn)return;
    if(!safeHeld){const snapshot={sample:drawn.current,at:new Date().toLocaleTimeString([], {hour12:false})};setHeld(snapshot);publish(snapshot);}
    setReasons(was=>new Set(was).add(reason));
  };
  const resume=()=>{root.current?.ownerDocument.getSelection()?.removeAllRanges();setHeld(undefined);setReasons(new Set());publish(undefined);};
  const clear=(reason:Reason)=>setReasons(was=>{const next=new Set(was);next.delete(reason);if(!next.size){setHeld(undefined);publish(undefined);}return next;});
  const value:Display={shown,latest,held:safeHeld,reasons,hold,clear,resume,retry:()=>{failed.current=false;setRetry(n=>n+1);},generation,node,engine,root};
  return <Context.Provider value={value}><div className="stream-result" ref={root}>{children}</div></Context.Provider>;
}
export function StreamControls() {
  const display=useContext(Context);if(!display)return null;
  const {held,latest,hold,resume,shown}=display;
  const sources=latest.metadata?.sources ?? [],phase=sources.find(source=>source.phase!=="open")?.phase ?? (sources.length?"open":"opening");
  return <div className="stream-controls">
    <span className={`stream-phase mono-${phase==="failed"?"bad":phase==="open"?"ok":"dim"}`} role="status">{held?`held${["ended","stopped","failed"].includes(phase)?` · ${phase}`:""}`:phase}</span>
    <button className="cell-action stream-hold" disabled={!shown.value} aria-label={held?"Resume live display":"Hold displayed snapshot"} title={held?"Resume live display":"Hold displayed snapshot"} aria-description={held?"Show the newest sample again":"Freeze this display locally. The source keeps running; cancelling it is a run action."} aria-pressed={Boolean(held)} onClick={()=>held?resume():hold("manual")}>
      <svg aria-hidden="true" viewBox="0 0 18 18" fill="none" stroke="currentColor" strokeWidth="1.5">{held?<path d="m6 3 8 6-8 6Z"/>:<path d="M6 3v12M12 3v12"/>}</svg>
    </button>
  </div>;
}
/** `sourceLabel` names a source node for per-source counts; the node id is used without it. */
export function LiveView(props:{mode?:Mode;collapsed?:boolean;engine:Engine;generation:string;node:WorkspaceNode;workspace?:string;sourceLabel?:(node:string)=>string;cellHold?:boolean}) {
  const display=useContext(Context);
  const body=<StreamBody mode={props.mode??"expanded"} collapsed={props.collapsed??false} sourceLabel={props.sourceLabel}/>;
  if(!display)return <StreamDisplay {...props}><StreamControls/>{body}</StreamDisplay>;
  return body;
}
/** Each source's counters in its own scope; several sources are labelled, never summed. */
function SourceCounts({sources,label}:{sources:readonly DisplaySource[];label:(node:string)=>string}) {
  if(!sources.some(source=>source.counts))return null;
  const several=sources.length>1;
  const entries=sources.map(source=>({key:`${source.node}:${source.run}`,name:several?`${label(source.node)}: `:"",scope:source.counts ? sourceScope(source.counts) : undefined}));
  const text=entries.map(entry=>`${entry.name}${entry.scope ? [entry.scope.text,entry.scope.rejected].filter(Boolean).join(" · ") : "no counts"}`).join(" | ");
  return <div className="stream-counts mono-dim" title={`${text}. ${SOURCE_SCOPE_HELP}`}>{entries.map(entry=><span key={entry.key}>{entry.name}{entry.scope?.text ?? "no counts"}{entry.scope?.rejected && <span className="mono-warn"> · {entry.scope.rejected}</span>}</span>)}</div>;
}
function StreamBody({mode,collapsed,sourceLabel=node=>node}:{mode:Mode;collapsed:boolean;sourceLabel?:(node:string)=>string}) {
  const display=useContext(Context)!;
  const {shown,latest,held,node,generation,engine,hold,clear}=display;
  const keys=useMemo(()=>shown.value ? streamItemKeys(shown.value) : undefined,[shown.value]);
  // Accepted deltas are meaningful only for one source; several sources never get an invented total.
  const counts=shown.metadata?.sources.length===1 ? shown.metadata.sources[0]?.counts : undefined;
  const incoming=held && counts && latest.metadata?.sources.length===1 && latest.metadata.sources[0]?.counts ? BigInt(latest.metadata.sources[0].counts.accepted)-BigInt(counts.accepted) : 0n;
  const value=shown.value;
  const terminal=latest.metadata?.sources.some(source=>source.phase==="stopped" || source.phase==="failed");
  const revisions=held && latest.metadata && held.sample.metadata ? BigInt(latest.metadata.revision)-BigInt(held.sample.metadata.revision) : 0n;
  const body=useRef<HTMLDivElement>(null);
  useEffect(()=>{
    const doc=body.current?.ownerDocument;if(!doc)return;
    const change=()=>{const selection=doc.getSelection();if(!selection || selection.isCollapsed){clear("selection");return;}if(body.current?.contains(selection.anchorNode) || body.current?.contains(selection.focusNode))hold("selection");};
    doc.addEventListener("selectionchange",change);return()=>doc.removeEventListener("selectionchange",change);
  });
  return <div className={`live-view${latest.problem && !held?" live-view-read-failed":terminal?" live-view-terminal":""}`} ref={body} onScrollCapture={event=>{
    const target=event.target as HTMLElement;
    if(value && Array.isArray(value.data) && !isLogValue(value) && !keys && target.scrollTop+target.clientHeight<target.scrollHeight-2)hold("reading");
  }}>
    {latest.problem && <div className="stream-notice mono-warn" role="alert" title={latest.problem}>{latest.problem}{!latest.withdrawn && <button className="cell-action" onClick={display.retry}>read again</button>}</div>}
    <div className="stream-value"><StreamItemsContext.Provider value={keys}>{value ? <ValueBlock engine={engine} value={value} cacheKey={`${generation}:${node.id}:display:${shown.metadata?.revision??shown.revision}:${epoch(shown)}`} bindingKey={`${generation}:${node.id}:${epoch(shown)}`} mode={mode} collapsed={collapsed} facts={{whole:true}}/>
      : <p className="mono-dim">{latest.withdrawn || !resultAccess(node).live ? "Display unavailable for this source." : latest.problemCode==="budget" ? "Newest value not displayed · display budget exceeded." : "Waiting for a public sample…"}</p>}</StreamItemsContext.Provider></div>
    {!collapsed && <div className="stream-footer"><div className="stream-status-line">
      {held && <span className="stream-snapshot mono-dim" title={[...display.reasons].join(", ")}>snapshot · {held.at}{revisions>0n && ` · ${revisions} newer value revisions`}</span>}
      {incoming>0n && <button className="cell-action stream-new" onClick={display.resume} title="Events this source accepted since the hold. Resume to show its latest window.">↓ {incoming.toString()} source events</button>}
    {value && Array.isArray(value.data) && !keys && !collapsed && <div className="stream-counts mono-faint" title="no item identity · reading holds the display">no item identity · reading holds the display</div>}
    {terminal && !collapsed && <div className="stream-counts mono-warn" title="last displayed window · not a current input">last displayed window · not a current input</div>}
    </div>{!collapsed && <SourceCounts sources={shown.metadata?.sources ?? []} label={sourceLabel}/>}</div>}
  </div>;
}
