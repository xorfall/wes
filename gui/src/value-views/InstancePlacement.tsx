import {useCallback,useLayoutEffect,useMemo,useRef,useState,type ReactNode} from "react";
import type {Engine} from "../engine";
import type {StoredValue} from "../protocol";
import type {ViewFrame} from "./instances";
import {valueViewModules} from "./registry";
import {viewDisplays,displayIdentity,type DisplaySnapshot} from "./displays";

export interface InstanceDisplay { readonly label:string; readonly referenceOnly?:boolean }

// Placement only: neither creates a result, applies a query nor starts observation.
const OPEN="Show this existing view here. It reads the view's current frame and runs nothing.";
const NAVIGATE="Scroll to where this view is already shown. Runs nothing.";
export function InstancePlacement({value,engine,display,children}:{value:StoredValue;engine:Engine;display:InstanceDisplay;children:(report:(frame:ViewFrame)=>void)=>ReactNode}){
  const store=viewDisplays(engine),slot=useMemo(()=>Symbol(),[]),box=useRef<HTMLDivElement>(null);
  const key=displayIdentity(value,engine.viewGeneration());
  const [received,setReceived]=useState<{key:string;snapshot:DisplaySnapshot}>();
  const [requested,setRequested]=useState<string>();
  const [reserved,setReserved]=useState<number>();
  const wasOwner=useRef(false);
  const eligible=!display.referenceOnly||requested===key;
  const snapshot=received?.key===key?received.snapshot:undefined,owner=snapshot?.owner===slot;
  useLayoutEffect(()=>store.join(key,slot,{eligible,label:display.label,target:()=>box.current,changed:snapshot=>{
    if(wasOwner.current&&snapshot.owner!==slot){const height=box.current?.getBoundingClientRect().height;if(height)setReserved(height);}
    wasOwner.current=snapshot.owner===slot;setReceived({key,snapshot});
  }}),[store,key,slot]);
  useLayoutEffect(()=>store.configure(key,slot,eligible,display.label),[store,key,slot,eligible,display.label]);
  const report=useCallback((frame:ViewFrame)=>{
    const root=frame.instances.find(entry=>entry.id===frame.root);if(!root)return;
    store.describe(key,slot,{definition:valueViewModules.named(root.definition,root.artifact)?.definition?.name??root.definition,
      members:new Set(Object.values(root.members).flat()).size});
  },[store,key,slot]);
  if(owner)return <div ref={box} className="view-instance-anchor" role="region" aria-label={`${snapshot?.label??display.label} view`} tabIndex={-1}>
    {children(report)}
  </div>;
  const definition=snapshot?.definition??(typeof value.data==="object"&&value.data!==null&&"definition" in value.data?String(value.data.definition):undefined);
  return <div ref={box} className="view-instance-reference" style={reserved?{minHeight:reserved}:undefined}>
    <span className="mono-ref">→ <span data-variable-name={snapshot?.label??display.label}>{snapshot?.label??display.label}</span></span>
    {definition&&<span className="mono-dim"> · {definition}</span>}
    {snapshot?.members!==undefined&&snapshot.members>0&&<span className="mono-dim"> · {snapshot.members} members</span>}
    <button type="button" className="cell-action" aria-description={snapshot?.owner?NAVIGATE:OPEN} title={snapshot?.owner?NAVIGATE:OPEN} onClick={()=>{if(snapshot?.owner)store.navigate(key);else setRequested(key);}}>{snapshot?.owner?"Go to view":"Open view"}</button>
  </div>;
}
