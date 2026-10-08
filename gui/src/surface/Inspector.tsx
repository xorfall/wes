import { stringifyExactJson } from "../exact-json";
import { useEffect, useMemo, useRef, useState } from "react";
import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";
import { stoppedStream, updatePendingStatus, type Workspace, type WorkspaceNode } from "../workspace";
import { RecordProgress } from "./RecordProgress";
import { ScanReceiptDetails } from "./ScanReceiptDetails";
import { evidenceLabel } from "./record-progress";
import type { SessionCell } from "./session-model";
import { liveReader, type LiveSample } from "../live-view-reader";
import { ValueBlock } from "./render/ValueBlock";
import { ReadableJson } from "./ResultInspection";
import { RunHistory } from "./RunHistory";
import { typeLine } from "../presentation/type-shape";
import { presentationType } from "../presentation/prepare";
import { useScrollMemory } from "./scroll-memory";
import { resultAccess } from "./result-access";
import { openRoute } from "./open-route";
import { creationInputOf } from "./graph-model";
import "./inspector.css";

const sentence=(text:string)=>`${text.charAt(0).toUpperCase()}${text.slice(1)}.`;

export type InspectorTab="inspect"|"json"|"history";
export interface InspectorSelection { cell:string; node?:string; tab:InspectorTab }
export function snapshotLabel(snapshot:{at:string;revision:number;serverRevision?:string},stopped:boolean,newer:boolean):string {
  return `snapshot · ${snapshot.at} · ${snapshot.serverRevision ? `value revision ${snapshot.serverRevision}` : `client sample ${snapshot.revision}`}${stopped ? " · stream stopped" : ""}${newer ? stopped ? " · the stream's last value is newer" : " · the cell has newer values" : ""}`;
}

/** A passive, workspace-scoped host. Freezing affects only the display. */
export function Inspector({engine,workspace,generation,cell,selection,active,onTab,onClose,onWindow,overlay}:{
  engine:Engine;workspace:Workspace;generation:string|undefined;cell:SessionCell;selection:InspectorSelection;active:boolean;
  onTab:(tab:InspectorTab)=>void;onClose:()=>void;onWindow:()=>void;overlay:boolean;
}) {
  const rememberScroll=useScrollMemory(active,selection.tab);
  const node=workspace.nodes.find(it=>it.id===selection.node);
  const [read,setRead]=useState<{handle:string;value:StoredValue;generation:string;signature:string}>();
  const [problem,setProblem]=useState<string>();
  const [retry,setRetry]=useState(0);
  const [historyVisited,setHistoryVisited]=useState(selection.tab==="history");
  useEffect(()=>{if(selection.tab==="history")setHistoryVisited(true);},[selection.tab]);
  const [sample,setSample]=useState<LiveSample & {signature?:string}>({revision:0});
  const sampleRef=useRef(sample);sampleRef.current=sample;
  const [snapshot,setSnapshot]=useState<{value:StoredValue;at:string;revision:number;signature:string;epoch?:string;serverRevision?:string;fromCell?:boolean}>();
  const latest=useRef(node);latest.current=node;
  const heading=useRef<HTMLButtonElement>(null);
  const permitted=resultAccess(node).current || resultAccess(node).live;
  const live=resultAccess(node).live;
  const owner=useMemo(()=>liveReader(engine),[engine]);
  const signature=JSON.stringify([node?.id,node?.command,node?.environment,node?.private,node?.doubt,generation]);
  const safeSnapshot=snapshot?.signature===signature ? snapshot : undefined;
  useEffect(()=>{
    const held=node && generation ? owner.cellSnapshot(node.id,generation) : undefined;
    setSnapshot(held?.sample.value ? {value:held.sample.value,at:held.at,revision:held.sample.revision,epoch:JSON.stringify(held.sample.metadata?.epochs),serverRevision:held.sample.metadata?.revision,fromCell:true,signature} : undefined);
    setSample({revision:0});setRead(undefined);setProblem(undefined);
  },[signature]);
  useEffect(()=>{if(active)heading.current?.focus({preventScroll:true});},[active]);
  useEffect(()=>{
    if(!active || !permitted || !node?.handle || !generation)return;
    let current=true;const handle=node.handle;setProblem(undefined);
    void engine.fetch(handle).then(value=>{if(current && latest.current?.handle===handle)setRead({handle,value,generation,signature});},error=>{if(current)setProblem(error instanceof Error ? error.message : String(error));});
    return()=>{current=false;};
  },[engine,node?.handle,node?.evidence?.kind,node?.evidence?.run,signature,generation,active,permitted,retry]);
  useEffect(()=>{
    if(!active || selection.tab==="history" || !live || !node || !generation)return;
    return owner.watch(node.id,generation,next=>{
      if(next.withdrawn){setSnapshot(undefined);setRead(undefined);}
      else setSnapshot(was=>was?.epoch && was.epoch!==JSON.stringify(next.metadata?.epochs) ? undefined : was);
      setSample({...next,signature});
    },sampleRef.current);
  },[owner,node?.id,generation,signature,active,live,retry,selection.tab]);
  const currentValue=live ? (sample.signature===signature ? sample.value : undefined) : read && read.handle===node?.handle && read.generation===generation && read.signature===signature ? read.value : undefined;
  const value=permitted && !sample.withdrawn ? safeSnapshot?.value ?? currentValue : undefined;
  const stopped=Boolean(stoppedStream(node));
  const partial=resultAccess(node).partial;
  const newer=useMemo(()=>{
    if(!safeSnapshot)return false;
    if(!stopped)return safeSnapshot.serverRevision && sample.metadata ? BigInt(sample.metadata.revision)>BigInt(safeSnapshot.serverRevision) : sample.revision>safeSnapshot.revision;
    if(!currentValue)return false;
    try{return stringifyExactJson(currentValue.data)!==stringifyExactJson(safeSnapshot.value.data);}catch{return undefined;}
  },[safeSnapshot,stopped,currentValue,sample.revision]);
  const tab=selection.tab;
  const label=node?.name ? `$${node.name}` : selection.node ? `$${selection.node}` : "submission";
  return <aside className="inspector-host" aria-label={`Inspector of ${label}`} hidden={!active} onScrollCapture={rememberScroll} onKeyDown={event=>{
    if(event.key==="Escape" && !event.defaultPrevented){event.preventDefault();event.stopPropagation();onClose();}
  }}>
    <header className="inspector-header">
      {overlay && <button className="cell-action" onClick={onClose}>← session</button>}
      <div className="inspector-title"><strong>{label}</strong><span>{value ? typeLine(presentationType(value)) : node?.state ?? "not run"}</span></div>
      <div className="inspector-live" role="status">
        {safeSnapshot ? <><span>{safeSnapshot.fromCell ? `snapshot from the cell’s hold at ${safeSnapshot.at}` : snapshotLabel(safeSnapshot,stopped,Boolean(newer))}{newer===undefined && " · last-value comparison exceeds the display budget"}</span><button className="cell-action" onClick={()=>setSnapshot(undefined)}>{stopped ? "show last value" : "follow live"}</button></>
          : live ? <><span>live · following</span><button className="cell-action" disabled={!currentValue} onClick={()=>currentValue && setSnapshot({value:currentValue,at:new Date().toLocaleTimeString(),revision:sample.revision,epoch:JSON.stringify(sample.metadata?.epochs),serverRevision:sample.metadata?.revision,signature})}>freeze</button></>
            : stopped ? <span>stopped · last value</span> : partial ? <span className="mono-warn">{evidenceLabel(node)}</span> : node?.state==="stale" ? <span>stale · previous result</span> : node?.state==="pending" || node?.state==="running" ? <span>updating · previous result</span> : null}
        {/* Holding or following the display never pauses computation, so this stands beside every observation label. */}
        {node && updatePendingStatus(node) && <span className="inspector-update-pending">{updatePendingStatus(node)}</span>}
      </div>
      <div className="inspector-tabs" role="tablist" aria-label="Inspector views">{(["inspect","json","history"] as const).map(name=><button ref={name===tab ? heading : undefined} key={name} className="cell-action" role="tab" aria-selected={name===tab} onClick={()=>onTab(name)}>{name}</button>)}<button className="cell-action" aria-label="Open result in a separate window" onClick={()=>openInspectorWindow(node,tab,workspace.identity?.name)}>↗</button><button className="cell-action inspector-window" onClick={onWindow} aria-label="Toggle inspector window">⧉</button><button className="cell-action" onClick={onClose} aria-label="Close inspector">×</button></div>
    </header>
    <div className="inspector-content">
      <div role="tabpanel" aria-label="inspect" hidden={tab!=="inspect"}>
        {node?.private && <p className="mono-warn">Private · memory only · readable in this workspace</p>}
        <RecordProgress node={node} />
        {partial && node?.failure && <p className="mono-bad">{node.failureRecord?.code ? `${node.failureRecord.code} · ` : ""}{node.failure}</p>}
        {!permitted && node && <p className="mono-warn">{node.doubt ? "Outcome uncertain · result not read" : `${node.state} · no current value`}</p>}
        {(problem || sample.problem) && <p className="mono-warn" role="alert">Read failed · {problem ?? sample.problem} <button className="cell-action" onClick={()=>setRetry(n=>n+1)}>retry reading</button></p>}
        {value ? <ValueBlock engine={engine} value={value} cacheKey={`${generation}:${node?.id}:inspector:${safeSnapshot ? `snapshot:${safeSnapshot.revision}` : live ? `${JSON.stringify(sample.metadata?.epochs)}:${sample.metadata?.revision??sample.revision}` : node?.handle}`} bindingKey={`${generation}:${node?.id}:inspector`} mode="window" inCell={false} facts={{whole:true,stopped}}/>
          : permitted && !problem && <p className="mono-dim">{node?.handle || live ? "Reading value…" : "no value · inspect run history for the recorded outcome"}</p>}
        {/* Beside the generic value view, not instead of it; only a typed ScanResult has one. */}
        <ScanReceiptDetails value={value} />
        {node?.dependencyLifetime==="creation" && <p className="mono-dim inspector-creation" role="note">{sentence(creationInputOf(workspace,node))}</p>}
        <details className="inspector-details"><summary>run details</summary><p>{node?.state ?? "not run"}</p><pre>{node?.command ?? cell.source}</pre>{node?.failure && <p className="mono-bad">{node.failure}</p>}<RecordProgress node={node} full /></details>
      </div>
      <div role="tabpanel" aria-label="json" hidden={tab!=="json"}>{value ? <ReadableJson value={value} active={tab==="json"}/> : <p className="mono-dim">No stored value available.</p>}</div>
      <div role="tabpanel" aria-label="history" hidden={tab!=="history"}>{generation && historyVisited && <RunHistory engine={engine} workspace={workspace} generation={generation} cell={cell.id} nodes={cell.nodes} initialNode={selection.node} onClose={onClose}/>}</div>
    </div>
  </aside>;
}
export function openInspectorWindow(node:WorkspaceNode|undefined,tab:InspectorTab,workspace?:string):boolean {
  if(!node)return false;
  const windowTab=tab==="json" ? "json" : "result";
  const result=window.open(openRoute(node.id,windowTab,workspace),"_blank");
  return Boolean(result || window.__WES_DESKTOP__);
}
