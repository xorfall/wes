/** Render the real SDK component in host unit tests. Iframe transport is tested separately. */
import {vi} from "vitest";
import type {ViewRenderer} from "@wes/view-sdk";
import type {PresentationNode} from "../presentation/types";
import {valueViewModules} from "./registry";
import {decodeContract} from "./definition";
import {InteractiveModule} from "./interactive";
import type {ValueViewModule} from "./contract";

export function localPackage<I,O,S,E,EO>(name:string, source:ViewRenderer<I,O,S,E,EO>) {
  const renderer=source as unknown as ViewRenderer<any,any,any,any,any>;
  const shipped=valueViewModules.named(name)!;
  const definition=shipped.definition!, protocol=definition.interaction;
  const valid=(name:string,value:unknown)=>{try{decodeContract(definition,name,value);return true;}catch{return false;}};
  const module:ValueViewModule={...shipped,
    ...(protocol?{interaction:{protocol:{id:protocol.protocol,state:(v):v is unknown=>valid(protocol.state,v),event:(v):v is unknown=>valid(protocol.event,v)},
      initial:model=>renderer.initial!((model as {input:unknown}).input),reduce:renderer.reduce!},
      outputSnapshot:renderer.outputs!,outputEvents:renderer.eventOutputs?(previous,event,next)=>renderer.eventOutputs!(previous,next,event):undefined}:{}),
  };
  return vi.spyOn(shipped,"Component").mockImplementation(props=>{
    const model=props.model as {input:unknown;identity?:string;path:string;mode:"window";coordinated?:boolean;slots:Record<string,PresentationNode[]>};
    const Component=renderer.Component;
    const draw=(state:unknown,revision:number,emit:(event:unknown)=>void)=><Component input={model.input} state={state} revision={revision} emit={emit}
      context={{mode:model.mode,instance:model.identity??null,coordinated:model.coordinated}} slots={Object.fromEntries(Object.entries(model.slots).map(([name,nodes])=>[name,nodes.map(node=>props.renderChild(node))]))}/>;
    return protocol?<InteractiveModule module={module} model={model} path={model.path} render={port=>draw(port.state,port.revision,port.emit)}/>:draw(undefined,0,()=>{});
  });
}
