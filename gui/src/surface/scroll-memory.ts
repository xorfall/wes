import { useLayoutEffect, useRef, type UIEvent } from "react";

/** Retained hosts restore reader positions after a tab or pane was hidden. */
export function useScrollMemory(active:boolean,tab:string) {
  const positions=useRef(new Map<HTMLElement,{top:number;left:number}>());
  const remember=(event:UIEvent<HTMLElement>)=>{
    const element=event.target as HTMLElement;
    if(!element || typeof element.scrollTop!=="number")return;
    positions.current.set(element,{top:element.scrollTop,left:element.scrollLeft});
    for(const saved of positions.current.keys())if(!saved.isConnected)positions.current.delete(saved);
    while(positions.current.size>64)positions.current.delete(positions.current.keys().next().value!);
  };
  useLayoutEffect(()=>{
    if(!active || typeof requestAnimationFrame==="undefined")return;
    const frame=requestAnimationFrame(()=>{
      for(const [element,at] of positions.current) {
        if(!element.isConnected){positions.current.delete(element);continue;}
        if(element.getClientRects().length){element.scrollTop=at.top;element.scrollLeft=at.left;}
      }
    });
    return()=>cancelAnimationFrame(frame);
  },[active,tab]);
  return remember;
}
