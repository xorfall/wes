import {defineView} from "@wes/view-sdk";
import {useMemo} from "react";
import {definition} from "./contract";
import {initial,reduce,outputs,eventOutputs} from "./navigation";
import {prepareTimeline} from "./model";
import {TimelineView,TimelineInspection} from "./Timeline";
export default defineView(definition,{initial,reduce,outputs,eventOutputs,Inspection:({input,state,emit,context})=>{
  const model=useMemo(()=>prepareTimeline(input,false,context.instance),[input,context.instance]);
  return <div className="timeline-view timeline-inspection-outlet"><h3 className="timeline-inspection-title">{model.title}</h3><TimelineInspection model={model} port={{state,emit}}/></div>;
},Component:({input,state,emit,context})=>{
  const model=useMemo(()=>prepareTimeline(input,context.mode==="preview",context.instance),[input,context.mode,context.instance]);
  return <TimelineView model={model} interaction={{state,emit}} coordinated={context.coordinated} onInspect={context.inspect} inspectionActive={context.inspectionActive}/>;
}});
