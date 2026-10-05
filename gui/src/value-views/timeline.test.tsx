import {expect,it} from "vitest";
import {renderToStaticMarkup} from "react-dom/server";
import {act,create} from "react-test-renderer";
import {useState} from "react";
import timeline from "../../../views/timeline/View";
import {prepareTimeline} from "../../../views/timeline/model";
import {xAt} from "../../../views/timeline/Timeline";
import {nanos,instant,atRatio,ratio,fitNanos} from "@wes/view-sdk";
import {timelineData,timelineType} from "../../../tools/view-dev/work/timeline";
import {decodeContract} from "./definition";
import type {Input} from "../../../views/timeline/contract";
const data=()=>({...timelineData(),view:"timeline" as const});
const context={mode:"window" as const,instance:"stable-instance"};
it("preserves Instant precision and uses true timestamp spacing instead of sample index",()=>{
  for(const v of ["1969-12-31T23:59:59.999999999Z","2000-02-29T12:34:56.000000001Z","-0001-01-01T00:00:00Z","+1000000-12-31T23:59:59.123456789Z"])expect(instant(nanos(v)!)).toBe(v);
  const extent={start:"2030-01-01T00:00:00.000000001Z",end:"2030-01-01T00:00:00.000000011Z"};
  expect(ratio(nanos(atRatio(extent,.5))!,extent)).toBe(.5);
  expect(xAt(nanos(atRatio(extent,.5))!,extent)).toBe(538);
  expect(xAt(nanos(atRatio(extent,.1))!,extent)).toBeCloseTo(187.6);
  expect(fitNanos(nanos(extent.start)!-1n,nanos(extent.end)!+1n,extent)).toEqual(extent);
});
it("validates shape, intervals, sorting, stable IDs, units and record limits",()=>{
  expect(()=>decodeContract(timeline.definition,timeline.definition.input,data(),true,timelineType)).not.toThrow();
  expect(()=>decodeContract(timeline.definition,timeline.definition.input,data(),true,{kind:"record",name:"Timeline",fields:[]})).toThrow();
  const wrong=data();wrong.series[0]!.samples.reverse();expect(()=>prepareTimeline(wrong)).toThrow(/sorted/);
  const duplicate=data();duplicate.events.push(duplicate.events[0]!);expect(()=>prepareTimeline(duplicate)).toThrow(/unique/);
  const units=data();units.series.push({...units.series[0]!,id:"second",unit:"ms"});expect(()=>prepareTimeline(units)).toThrow(/unit/);
  expect(()=>prepareTimeline({...data(),range:"bad"})).toThrow(/Interval/);
  expect(()=>prepareTimeline(timelineData("large",20001) as Input)).toThrow(/record limit/);
});
it("renders gaps, times and inspection using only SDK input and context; identity belongs to the instance",()=>{
  const input=data(),model=prepareTimeline(input,false,context.instance);
  expect(model.id).toBe("stable-instance");expect(input.id).not.toBe("stable-instance");
  const html=renderToStaticMarkup(<timeline.Component input={input} state={timeline.initial!(input)} revision={0} emit={()=>{}} slots={{}} context={context}/>);
  expect(html).toContain("timeline-svg");expect(html).toContain("gaps break the line");expect(html).toContain("08:00");
  expect(html).toContain("timeline-inspection");
});
it("keeps keyboard selection, selected record outputs and events separate from hover",()=>{
  const input=data(),initial=timeline.initial!(input),events:unknown[]=[];
  function Harness(){const [state,setState]=useState(initial);return <timeline.Component input={input} state={state} revision={0} emit={event=>{events.push(event);setState(previous=>timeline.reduce!(previous,event));}} slots={{}} context={context}/>;}
  let tree!:ReturnType<typeof create>;act(()=>{tree=create(<Harness/>);});
  act(()=>tree.root.findByProps({className:"timeline-svg"}).props.onKeyDown({key:"ArrowRight",shiftKey:true,preventDefault(){},stopPropagation(){}}));
  expect(tree.root.findAllByProps({className:"timeline-selection"})).toHaveLength(1);
  expect(events).toHaveLength(2);
  act(()=>tree.root.findAllByProps({className:"timeline-record"})[0]!.props.onClick());
  const item=(events.at(-1) as {item:unknown}).item;expect(item).toHaveProperty("source","stable-instance");
  const picked=timeline.reduce!(initial,events.at(-1) as Parameters<NonNullable<typeof timeline.reduce>>[1]);
  expect(timeline.outputs!(picked).selectedItem).toEqual(item);
  expect(timeline.eventOutputs!(initial,picked,events.at(-1) as Parameters<NonNullable<typeof timeline.reduce>>[1])).toEqual([{port:"picked",value:item}]);
  expect(timeline.eventOutputs!(initial,initial,{kind:"cursor",at:initial.viewport.start})).toEqual([]);
  act(()=>tree.unmount());
});
it("shows compact coordinated plots with explicit inspection instead of duplicate standalone controls",()=>{
  const input=data();
  let tree!:ReturnType<typeof create>;
  act(()=>{tree=create(<timeline.Component input={input} state={timeline.initial!(input)} revision={0} emit={()=>{}} slots={{}} context={{...context,coordinated:true}}/>);});
  expect(tree.root.findAllByProps({className:"timeline-toolbar"})).toHaveLength(0);
  expect(tree.root.findAllByProps({className:"timeline-inspection"})).toHaveLength(0);
  expect(tree.root.findAllByProps({className:"timeline-svg"})).toHaveLength(1);
  const toggle=()=>tree.root.findAllByType("button").find(button=>button.props["aria-expanded"]!==undefined)!;
  act(()=>toggle().props.onClick());
  expect(tree.root.findAllByProps({className:"timeline-inspection"})).toHaveLength(1);
  act(()=>toggle().props.onClick());
  expect(tree.root.findAllByProps({className:"timeline-inspection"})).toHaveLength(0);
  act(()=>tree.unmount());
});
it("clusters colliding events without inventing records and selects through the final nanosecond",()=>{
 const input=data(),start=nanos(input.events[0]!.at)!;
 input.events=Array.from({length:29},(_,index)=>({id:`event-${index}`,at:instant(start+BigInt(index)),label:index%2?"retry":"failure",detail:`synthetic event ${index}`}));
 const events:unknown[]=[];let tree!:ReturnType<typeof create>;
 act(()=>{tree=create(<timeline.Component input={input} state={timeline.initial!(input)} revision={0} emit={event=>events.push(event)} slots={{}} context={context}/>);});
 const markers=tree.root.findAllByProps({className:"timeline-marker"});expect(markers).toHaveLength(1);
 expect(markers[0]!.props["aria-label"]).toContain("29 events · failure: 15 · retry: 14");
 act(()=>markers[0]!.props.onKeyDown({key:"Enter",preventDefault(){},stopPropagation(){}}));
 expect(events[0]).toEqual({kind:"selection",range:{start:instant(start),end:instant(start+29n)}});
 expect(events[1]).toMatchObject({kind:"item",item:{source:"stable-instance",id:"event-0"}});
 expect(tree.root.findAllByProps({className:"timeline-cluster-list"})).toHaveLength(1);
 act(()=>tree.unmount());
});
it("keeps coordinated preview to a sparkline strip and cancels a drag without committing effects",()=>{
 const input=data(),events:unknown[]=[];let tree!:ReturnType<typeof create>;
 act(()=>{tree=create(<timeline.Component input={input} state={timeline.initial!(input)} revision={0} emit={event=>events.push(event)} slots={{}} context={{...context,mode:"preview",coordinated:true}}/>);});
 const plot=tree.root.findByProps({className:"timeline-svg"});expect(plot.props.height).toBe(24);
 const target={getBoundingClientRect:()=>({left:0,width:1000}),focus(){},setPointerCapture(){}};
 act(()=>plot.props.onPointerDown({currentTarget:target,clientX:400,button:0,pointerId:3}));
 act(()=>plot.props.onPointerCancel());
 expect(events.at(-1)).toEqual({kind:"selection-preview",range:null});expect(events.some(event=>(event as {kind:string}).kind==="selection")).toBe(false);
 act(()=>tree.unmount());
});
it("opens the host outlet for coordinated inspection without inserting a member detail row",()=>{
 const input=data(),inspect=()=>{calls++;};let calls=0;let tree!:ReturnType<typeof create>;
 act(()=>{tree=create(<timeline.Component input={input} state={timeline.initial!(input)} revision={0} emit={()=>{}} slots={{}} context={{...context,coordinated:true,inspect,inspectionActive:true}}/>);});
 const button=tree.root.findAllByType("button").find(b=>b.props["aria-label"]?.startsWith("Inspect "))!;
 expect(button.props["aria-expanded"]).toBe(true);act(()=>button.props.onClick());expect(calls).toBe(1);
 expect(tree.root.findAllByProps({className:"timeline-inspection"})).toHaveLength(0);
 act(()=>tree.root.findAllByProps({className:"timeline-marker"})[0]!.props.onKeyDown({key:"Enter",preventDefault(){},stopPropagation(){}}));
 expect(calls).toBe(2);expect(tree.root.findAllByProps({className:"timeline-cluster-list"})).toHaveLength(0);act(()=>tree.unmount());
});
