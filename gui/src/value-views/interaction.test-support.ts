/** Pure SDK callbacks exercise host authority without simulating an iframe in React Test Renderer. */
import timeline from "../../../views/timeline/View";
import {valueViewModules} from "./registry";
import {decodeContract} from "./definition";
import type {ValueViewModule} from "./contract";
import type {Input,State,Event} from "../../../views/timeline/contract";
const shipped=valueViewModules.named("timeline")!;
const valid=(name:string,value:unknown)=>{try{decodeContract(shipped.definition!,name,value);return true;}catch{return false;}};
export const localTimeline:ValueViewModule={...shipped,
  interaction:{protocol:{id:"TimeNavigation",state:(v):v is State=>valid("TimeState",v),event:(v):v is Event=>valid("TimeEvent",v)},
    initial:model=>{const r=(model as {range:{start:string;end:string}}).range;return timeline.initial!({range:`${r.start}/${r.end}`} as Input);},
    reduce:(state,event)=>timeline.reduce!(state as State,event as Event)},
  outputSnapshot:state=>timeline.outputs!(state as State),
  outputEvents:(previous,event,next)=>timeline.eventOutputs!(previous as State,next as State,event as Event),
};
