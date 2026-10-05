import { useEffect, useRef, useState } from "react";
import type { EditorView } from "@codemirror/view";
import "./editor.css";

/** JSON editing only; validity is decided by the shared backend descriptor validator. */
export function SpecSourceEditor({source,onChange,onSave,onValidate,readOnly=false}:{readOnly?:boolean;source:string;onChange:(text:string)=>void;onSave:()=>void;onValidate:()=>void}) {
  const parent=useRef<HTMLDivElement>(null);const view=useRef<EditorView>();const callbacks=useRef({onChange,onSave,onValidate});callbacks.current={onChange,onSave,onValidate};
  const latest=useRef(source);latest.current=source;
  const [failed,setFailed]=useState(false);
  useEffect(()=>{let active=true;void import("./spec-source-wiring").then(({makeSpecEditor})=>{
    if(active&&parent.current)view.current=makeSpecEditor(parent.current,latest.current,text=>callbacks.current.onChange(text),()=>callbacks.current.onSave(),()=>callbacks.current.onValidate(),readOnly);
  }).catch(()=>{if(active)setFailed(true);});return()=>{active=false;view.current?.destroy();view.current=undefined;};},[readOnly]);
  useEffect(()=>{const current=view.current;if(current&&current.state.doc.toString()!==source)current.dispatch({changes:{from:0,to:current.state.doc.length,insert:source}});},[source]);
  return failed?<textarea aria-label="Spec source" className="spec-source-fallback" value={source} readOnly={readOnly} onChange={e=>onChange(e.target.value)}/>:<div className="spec-source-editor surface-sunk" aria-label="Spec source" ref={parent}/>;
}
