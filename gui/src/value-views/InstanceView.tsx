import { InstanceInteractionHost } from "./interactive";
import { attachSharedState } from "./shared-state";
import { LocalCoordinators } from "./coordinated";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";
import { prepareSync } from "../presentation/prepare";
import { present } from "../presentation/present";
import { registryStore } from "../presentation/registry-store";
import type { Context, Mode, PresentationNode } from "../presentation/types";
import { Presented } from "../surface/render/Presentation";
import { useColumns } from "../surface/render/measure";
import { valueViewModules } from "./registry";
import type { FrameSample, ViewFrame, ViewInstance as Entry, InputPatches } from "./instances";
import { max_view_frame_instances } from "./limits";
import { InstancePlacement,type InstanceDisplay } from "./InstancePlacement";
import { displayIdentity } from "./displays";
import { INPUT_RUN_DESCRIPTION, pinRefusal, referenceLabel } from "./pin";
import { documentRenderStatus, frameRenderObservation, RenderObservationContext } from "../view-render-status";
import { frameBindings, ViewDatasetContext } from "./view-datasets";

export const isViewInstance = (value:StoredValue) => value.type?.kind==="meta" && value.type.name==="ViewInstance";

type Cached = {entry:Entry;coordinated:boolean;patch:unknown;waiting:string|undefined;children:readonly PresentationNode[];node:PresentationNode;spent:number};
export class FramePresentationCache {
  context="";
  readonly nodes=new Map<string,Cached>();
  readonly inputs=new WeakMap<StoredValue,ReturnType<typeof prepareSync>>();
}
/** Prepare referenced members once. Container renderers receive views, not copied source records. */
export function presentFrame(frame:ViewFrame, context:Context, patches?:InputPatches, cache=new FramePresentationCache()):PresentationNode {
  if(!Array.isArray(frame.instances)||frame.instances.length>max_view_frame_instances())throw new Error("Invalid or oversized view frame");
  const contextKey=JSON.stringify(context);
  if(cache.context!==contextKey){cache.nodes.clear();cache.context=contextKey;}
  const entries=new Map(frame.instances.map(i=>[i.id,i]));
  const coordinated=new Set(frame.instances.flatMap(entry=>Object.entries(entry.members).filter(([name])=>valueViewModules.named(entry.definition,entry.artifact)?.definition?.slots[name]?.coordinates).flatMap(([,ids])=>ids)));
  if(entries.size!==frame.instances.length)throw new Error("Duplicate view identity");
  for(const id of cache.nodes.keys())if(!entries.has(id))cache.nodes.delete(id);
  const prepared=new Map<string,PresentationNode>(), visiting=new Set<string>();
  let remaining=context.lines, edges=0;
  const build=(id:string):PresentationNode=>{
    const built=prepared.get(id);if(built)return built;
    if(visiting.has(id))throw new Error("View membership cycle");
    const entry=entries.get(id);if(!entry)throw new Error("View member is unavailable");
    const module=valueViewModules.named(entry.definition,entry.artifact);
    if(!module?.definition||module.definition.digest!==entry.digest)throw new Error("View package does not match the backend. Rebuild the application.");
    if(!entry.input){
      const node:PresentationNode={kind:"view",path:`view/${id}`,view:module.id,model:undefined,children:[],waiting:entry.inputProblem??(entry.query?"Apply the query to load this view.":`Bind input to ${module.definition.name} before viewing it.`),fallback:{type:{kind:"unknown"},data:null,context},ownLines:1,summary:[{text:module.definition.name,tone:"dim"}]};
      prepared.set(id,node);return node;
    }
    visiting.add(id);
    const slots:Record<string,readonly PresentationNode[]>={};
    for(const [name,ids] of Object.entries(entry.members)){
      const slot=module.definition.slots[name];
      if(!slot||!Array.isArray(ids)||ids.length>slot.max||(edges+=ids.length)>512)throw new Error("Invalid view slot");
      slots[name]=ids.map(child=>{
        const member=entries.get(child), target=member&&valueViewModules.named(member.definition,member.artifact)?.definition;
        if(!target || (slot.accepts.length&&!slot.accepts.includes(target.name)) || (slot.protocol&&slot.protocol!==target.interaction?.protocol))throw new Error("Incompatible view member");
        return build(child);
      });
    }
    if(!Array.isArray(entry.linkedInputs))throw new Error("Invalid view input bindings");
    const path=`view/${id}`, patch=patches?.values[id];
    const waiting=entry.linkedInputs.length ? patches?.problems[id] ?? (!patch || entry.linkedInputs.some((f:string)=>!Object.hasOwn(patch,f)) ? "Waiting for committed input bindings…":undefined):undefined;
    const children=Object.values(slots).flat(), old=cache.nodes.get(id), isCoordinated=coordinated.has(id);
    if(old && old.entry===entry && old.coordinated===isCoordinated && old.patch===patch && old.waiting===waiting && old.children.length===children.length && children.every((c,i)=>c===old.children[i])){
      remaining=Math.max(0,remaining-old.spent);visiting.delete(id);prepared.set(id,old.node);return old.node;
    }
    let value=cache.inputs.get(entry.input);
    if(!value){value=prepareSync(entry.input);cache.inputs.set(entry.input,value);}
    if(patch && !waiting){
      if(value.type.kind!=="record" || typeof value.data!=="object" || !value.data)throw new Error("Expected record input");
      const data:Record<string,unknown>={...value.data}, fields=[...value.type.fields];
      for(const [name,input] of Object.entries(patch)){
        if(!entry.linkedInputs.includes(name))throw new Error("Undeclared input binding");
        const prepared=prepareSync(input);data[name]=prepared.data;
        const at=fields.findIndex(f=>f.name===name);
        if(at<0)fields.push({name,type:prepared.type});else fields[at]={name,type:prepared.type};
      }
      value={...value,type:{...value.type,fields},data};
    }
    const before=remaining;
    const shown=module.present({path,type:value.type,data:value.data,context,slots,instanceKey:entry.instance,inputRevision:entry.inputRevision,coordinated:isCoordinated},{
      remaining:()=>remaining,spend:n=>{remaining=Math.max(0,remaining-Math.max(0,n));},
      child:(name,type,data,options)=>{
        const child=present({prepared:prepareSync({type,data}),context:{...context,lines:remaining},registry:registryStore.get()}).root;
        if(options && (child.kind!=="view"||child.view!==options.view))throw new Error("Child view is unavailable");
        return {...child,path:`${path}/${encodeURIComponent(name)}`};
      },
    });
    const result:PresentationNode={kind:"view",path,view:module.id,...shown,...(waiting?{waiting}:{}),fallback:{type:value.type,data:value.data,context}};
    cache.nodes.set(id,{entry,coordinated:isCoordinated,patch,waiting,children,node:result,spent:before-remaining});
    visiting.delete(id);prepared.set(id,result);return result;
  };
  return build(frame.root);
}

export function InstanceView({value,engine,mode,display}:{value:StoredValue;engine?:Engine;mode:Mode;display?:InstanceDisplay}){
  const identity=displayIdentity(value,engine?.viewGeneration());
  if(engine&&display)return <InstancePlacement key={identity} value={value} engine={engine} display={display}>
    {report=><ActiveInstanceView value={value} engine={engine} mode={mode} onFrame={report}/>}
  </InstancePlacement>;
  return <div className="view-instance-viewport">
    <ActiveInstanceView key={identity} value={value} engine={engine} mode={mode}/>
  </div>;
}

// `observing` is the engine's effective state for a Current reference: it reads committed results while
// a panel is visible and suspends when the last one closes. Stop revokes reading; restored work waits
// for Start. Pinned and literal inputs never observe.
const OBSERVATION={
  start:"Read committed results of the referenced output into this view. Source commands are not run.",
  stop:"Stop reading results into this view. Source commands keep running, and reopening the view does not resume reading.",
};
const PIN_DESCRIPTION="Keep exactly the displayed input as a new protected result, then switch this view to it once retention is acknowledged. The source is not run again. Keep is a separate action for whole results.";
function ActiveInstanceView({value,engine,mode,onFrame}:{value:StoredValue;engine?:Engine;mode:Mode;onFrame?:(frame:ViewFrame)=>void}){
  const box=useRef<HTMLDivElement>(null),columns=useColumns(box);
  const id=typeof value.data==="object"&&value.data!==null&&"id" in value.data ? String(value.data.id):undefined;
  const instance=typeof value.data==="object"&&value.data!==null&&"instance" in value.data ? String(value.data.instance):undefined;
  const generation=engine?.viewGeneration();
  const subscribeWorkspace=useCallback((changed:()=>void)=>engine?engine.onViewWorkspace(changed):()=>{},[engine]);
  const workspace=useSyncExternalStore(subscribeWorkspace,()=>engine?.viewWorkspaceName());
  const [sample,setSample]=useState<FrameSample>({});
  const [attempt,setAttempt]=useState(0);
  const [controlProblem,setControlProblem]=useState<string>();
  const [controlling,setControlling]=useState(false);
  const [patches,setPatches]=useState<FrameSample<InputPatches>>({});
  const cache=useRef(new FramePresentationCache());
  const [settled,setSettled]=useState<Record<string,boolean>>({});
  useEffect(()=>{
    setSample({});
    cache.current=new FramePresentationCache();
    if(!engine||!id||!instance){setSample({problem:"Open this view in its workspace to resolve its live instance."});return;}
    return engine.watchViewFrame(id,instance,setSample);
  },[engine,id,instance,generation,attempt]);
  const linked=sample.frame?.instances.some(i=>i.linkedInputs.length)??false;
  useEffect(()=>{
    setPatches({});
    if(linked && engine && id && instance && generation)return engine.watchViewInputs(id,instance,generation,setPatches);
  },[linked,engine,id,instance,generation]);
  const shown=useMemo(()=>{
    if(!sample.frame)return {};
    try{return {node:presentFrame(sample.frame,{mode,columns,lines:mode==="preview"?200:4000,density:"normal",locale:"en-GB",timeZone:"UTC"},patches.frame,cache.current)};}
    catch(error){return {problem:error instanceof Error?error.message:"View unavailable"};}
  },[sample.frame,patches.frame,mode,columns]);
  useEffect(()=>{if(sample.frame&&shown.node)onFrame?.(sample.frame);},[sample.frame,shown.node,onFrame]);
  // Receipts are scoped by the actual workspace, generation and live frame entries. While the
  // workspace identity is unknown nothing is observed: no receipt beats a wrongly scoped one.
  const observation=useMemo(()=>workspace&&generation&&sample.frame?frameRenderObservation(documentRenderStatus,workspace,generation,sample.frame):undefined,[workspace,generation,sample.frame]);
  const frameRef=useRef(sample.frame);frameRef.current=sample.frame;
  const interactionKey=sample.frame?.instances.map(i=>`${i.id}:${i.instance}:${i.digest}:${JSON.stringify(i.members)}`).sort().join("|");
  const interactionHost=useMemo<React.ContextType<typeof InstanceInteractionHost>>(()=>{
    if(!engine||!generation||!interactionKey)return undefined;
    const local=new LocalCoordinators();
    const parents=new Map<string,string>();
    for(const entry of frameRef.current!.instances)for(const [slot,ids] of Object.entries(entry.members))if(valueViewModules.named(entry.definition,entry.artifact)?.definition?.slots[slot]?.coordinates)for(const id of ids)parents.set(id,entry.id);
    const owner=(id:string)=>{for(let depth=0;depth<32&&parents.has(id);depth++)id=parents.get(id)!;return id;};
    return (path,module,controller)=>{
      const entry=frameRef.current?.instances.find(entry=>`view/${entry.id}`===path);
      if(!entry?.instance||!module.definition?.interaction?.sharedFields.length)return ()=>{};
      const closeLocal=local.attach(owner(entry.id),module,controller);
      const close=attachSharedState(module,controller,{
        settled:ready=>setSettled(old=>old[entry.id]===ready?old:{...old,[entry.id]:ready}),
        watch:listener=>engine.watchViewState(entry.id,entry.instance,generation,listener),
        commit:(edit,signal)=>engine.commitViewState(entry.id,entry.instance,generation,edit,signal),
      });
      return ()=>{close();closeLocal();setSettled(old=>{const next={...old};delete next[entry.id];return next;});};
    };
  },[interactionKey,engine,generation]);
  // Dataset reads of a member are bound to exactly the frame drawn here: its root, instance and revisions.
  const datasetBindings=useMemo(()=>sample.frame&&engine&&instance&&generation?frameBindings(sample.frame,instance,engine,generation):undefined,[sample.frame,engine,instance,generation]);
  const following=sample.frame?.instances.filter(i=>i.inputReference.kind==="current")??[];
  const queries=sample.frame?.instances.filter(i=>!!i.query)??[];
  const active=following.some(i=>i.observing);
  const root=sample.frame?.instances.find(i=>i.id===sample.frame!.root);
  // Accepting the command is not the outcome; the Pin's own cell reports retention and binding.
  const [pinRequest,setPinRequest]=useState<{name:string;revision:string}>();
  const pin=async(entry:Entry)=>{
    if(!engine||!generation)return;
    setControlling(true);setControlProblem(undefined);
    try{setPinRequest({name:await engine.pinViewInput(entry,generation),revision:entry.revision});}
    catch(error){setControlProblem(error instanceof Error?error.message:"Pin request failed");}finally{setControlling(false);}
  };
  const unsettled=Object.values(settled).some(ready=>!ready);
  const observe=async()=>{
    if(!engine||!id||!instance||!generation)return;
    setControlling(true);setControlProblem(undefined);
    try{await engine.viewObservation(id,instance,generation,!active);}catch(error){setControlProblem(error instanceof Error?error.message:"Observation failed");}finally{setControlling(false);}
  };
  const applyQuery=async(entry:Entry)=>{
    if(!engine||!generation)return;
    setControlling(true);setControlProblem(undefined);
    try{await engine.applyViewQuery(entry.id,generation);}catch(error){setControlProblem(error instanceof Error?error.message:"Query request failed");}finally{setControlling(false);}
  };
  const stopQuery=async(entry:Entry)=>{
    if(!engine||!generation)return;
    setControlling(true);setControlProblem(undefined);
    try{await engine.viewObservation(entry.id,entry.instance,generation,false);}catch(error){setControlProblem(error instanceof Error?error.message:"Query stop failed");}finally{setControlling(false);}
  };
  const entries=sample.frame?.instances??[];
  const coordinated=new Set(entries.flatMap(entry=>Object.entries(entry.members).filter(([slot])=>valueViewModules.named(entry.definition,entry.artifact)?.definition?.slots[slot]?.coordinates).flatMap(([,ids])=>ids)));
  const outputs=entries.filter(entry=>!coordinated.has(entry.id)).flatMap(entry=>Object.entries(valueViewModules.named(entry.definition,entry.artifact)?.definition?.outputs??{}).filter(([,port])=>port.shared).map(([port])=>({entry,port})));
  const capture=async(node:string,port?:string)=>{
    if(!engine||!generation)return;
    setControlling(true);setControlProblem(undefined);
    try{await engine.captureViewResult(node,generation,port);}catch(error){setControlProblem(error instanceof Error?error.message:"Result capture failed");}finally{setControlling(false);}
  };
  // Says only what the backend snapshot actually freezes for this frame: connected members,
  // interaction state and shared outputs are named when present, never implied for a static view.
  const connected=entries.length-1;
  const interactive=entries.flatMap(entry=>{const definition=valueViewModules.named(entry.definition,entry.artifact)?.definition;return definition?.interaction?[definition]:[];});
  const stateful=interactive.length>0, shared=interactive.some(definition=>Object.values(definition.outputs??{}).some(port=>port.shared));
  const frozen=`${connected>0?`the inputs of this view and its ${connected} connected ${connected===1?"view":"views"}`:"this view’s inputs"}, where they came from${stateful?`, confirmed view state${shared?" and outputs":""}`:""}`;
  return <div className="view-instance" ref={box}>
    {engine && sample.frame && <details className="view-result-actions"><summary>{outputs.length?"Outputs & snapshot":"Snapshot"}</summary><div className="view-observation-controls">
      {outputs.map(({entry,port})=><button key={`${entry.id}/${port}`} disabled={controlling||!settled[entry.id]} aria-description={`Save the confirmed ${port} output of ${entry.id} as a new result`} onClick={()=>void capture(entry.id,port)}>Save ${entry.id}.{port} as result</button>)}
      <button disabled={controlling||unsettled} aria-description={`Create a separate result holding ${frozen}`} onClick={()=>void capture(sample.frame!.root)}>Save snapshot as result</button>
      <span className="mono-faint">{`Creates a separate result holding ${frozen}. Source commands are not rerun; this view keeps showing.${stateful?" Pending edits must be confirmed first.":""} The result uses your normal retention settings; check its kept status.`}</span>
    </div></details>}
    {queries.map(entry=><div className="view-observation-controls" key={entry.id}>
      <span>{entry.query!.environment && `${entry.query!.environment} · `}{entry.query!.template}{entry.query!.adapter?` → ${entry.query!.adapter}`:""}</span><button disabled={controlling||unsettled} aria-description="Run this view's query now. Only SAFE operations are admitted. Opening, observing or selecting never applies it." onClick={()=>void applyQuery(entry)}>Apply</button>
      {(entry.observing||entry.query!.running) && <button disabled={controlling} aria-description="Stop this view's query and cancel its request. Source commands are not affected." onClick={()=>void stopQuery(entry)}>Stop query</button>}
      <span className="mono-faint">{entry.query!.running?(entry.query!.mode==="live"?"Live query · bounded stream window":"Query in progress · previous result remains visible"):entry.observing?"Following committed selections":"Query not running · Apply to run it"}</span>
      {entry.inputProblem && <span className="mono-warn" role="status">{entry.inputProblem}</span>}
    </div>)}
    {sample.frame?.instances.map(i=>i.inputCautions.length?<p className="mono-warn" role="status" key={`cautions-${i.id}`}>{i.inputCautions.join(" · ")}</p>:null)}
    {patches.frame && Object.entries(patches.frame.cautions).map(([id,items])=>items.length?<p className="mono-warn" role="status" key={`linked-cautions-${id}`}>{items.join(" · ")}</p>:null)}
    {controlProblem && <p className="mono-warn" role="status">{controlProblem}</p>}
    {entries.filter(i=>!i.query).map(i=>i.inputProblem?<p className="mono-warn" key={i.id} role="status">{i.inputProblem}</p>:null)}{patches.problem && <p className="mono-warn" role="status">{patches.problem}</p>}{sample.problem||shown.problem
    ? <div><p className="mono-warn" role="status">{sample.problem??shown.problem}</p>{engine && sample.problem && <button className="cell-action" onClick={()=>setAttempt(old=>old+1)}>Reopen view</button>}</div>
    : shown.node ? <ViewDatasetContext.Provider value={datasetBindings}><InstanceInteractionHost.Provider value={interactionHost}><RenderObservationContext.Provider value={observation}><Presented node={shown.node}/></RenderObservationContext.Provider></InstanceInteractionHost.Provider></ViewDatasetContext.Provider> : <p className="mono-faint" role="status">{sample.paused?"View paused while other visible views are active. It will open when space is available.":"Reading view…"}</p>}
    {engine && root && <ReferenceFooter entry={root} following={following.length>0} active={active} controlling={controlling||unsettled}
      request={pinRequest?.revision===root.revision ? pinRequest.name : undefined} onObserve={()=>void observe()} onPin={()=>void pin(root)}/>}</div>;
}

/** What the view's input refers to, and the actions on that reference, below the view it describes. */
function ReferenceFooter({entry,following,active,controlling,request,onObserve,onPin}:{entry:Entry;following:boolean;active:boolean;controlling:boolean;request?:string;onObserve:()=>void;onPin:()=>void}){
  const label=referenceLabel(entry);
  const refusal=pinRefusal(entry);
  const rootCurrent=entry.inputReference.kind==="current";
  return <footer className="view-observation-controls view-reference" aria-label="View input">
    <span className="view-reference-label"><strong>{label.kind}</strong> <span className="mono-dim">{label.detail}</span></span>
    {label.inputRun && <details><summary>Input details</summary>
      <span className="mono-faint">{INPUT_RUN_DESCRIPTION}</span> <span className="mono-dim">{`Input run ${label.inputRun}`}</span>
    </details>}
    {following && <><button aria-description={`${active?OBSERVATION.stop:OBSERVATION.start}${rootCurrent?"":" Applies to connected views with Current inputs; this view's own input does not change."}`} disabled={controlling} onClick={onObserve}>{active?"Stop observing":"Start observing"}</button>
      {/* One panel-wide control. When only members follow results, say so: the root's own data does not update. */}
      <span className="mono-faint">{rootCurrent
        ? active?"Reading committed results":"Not reading · Start to read committed results"
        : active?"Members: reading committed results":"Members: not reading · Start to read their committed results"}</span></>}
    <button aria-description={refusal??PIN_DESCRIPTION} disabled={controlling||refusal!==undefined} onClick={onPin}>Pin input</button>
    {refusal && <span className="mono-faint">{refusal}</span>}
    {request && <span className="mono-dim" role="status">{`Pin requested as $${request}. Its cell reports retention and binding; this view changes only after both succeed.`}</span>}
  </footer>;
}
