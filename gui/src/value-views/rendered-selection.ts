import type { ContextType } from "react";
import { InstanceInteractionHost } from "./interactive";
import { parseExactJson, stringifyExactJson } from "../exact-json";
import type { SelectionActivations } from "../selection-activations";

/** A nominal result's drawing can activate only its declared output and captured source version. */
export function renderedSelectionHost(selections:SelectionActivations, source:{node:string;run:string}):NonNullable<ContextType<typeof InstanceInteractionHost>> {
  return (path,module,controller)=>{
    const definition=module.definition;
    const port=["selected","selection"].find(name=>definition?.outputs[name]?.shared && definition.outputs[name]?.mode==="state");
    if(path!=="" || !definition?.artifact || !port || !module.outputSnapshot)return ()=>{};
    let active=true;
    const stop=controller.intercept((_previous,next,_event,activation)=>{
      if(!activation)return true;
      const value=(module.outputSnapshot!(next) as Record<string,unknown>)[port];
      if(value===null || value===undefined)return true;
      const encoded=stringifyExactJson(value);
      if(new TextEncoder().encode(encoded).length>128*1024){controller.problem("Selection exceeds its input budget.");return false;}
      const selection={kind:"rendered" as const,source:source.node,run:source.run,view:definition.id,artifact:definition.artifact!,value:parseExactJson(encoded)};
      queueMicrotask(()=>{if(active)selections.activate(selection);});
      return true;
    });
    return ()=>{active=false;stop();};
  };
}
