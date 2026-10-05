import {localPackage} from "./package.test-support";
import metric from "../../../views/metric/View";
import dashboard from "../../../views/dashboard/View";
import choiceRenderer from "../../../views/choice/View";
import timelineRenderer from "../../../views/timeline/View";
import groupRenderer from "../../../views/timeline-group/View";
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { act, create } from "react-test-renderer";
import { ViewFrameReader, mergeViewFrame, type FrameSample, type ViewFrame } from "./instances";
import { InstanceView, presentFrame, FramePresentationCache } from "./InstanceView";
import { valueViewModules } from "./registry";
import type { Context } from "../presentation/types";
import {definition as rangeSummary} from "../../../examples/view-packages/source/range-summary/contract";
import {externalModule} from "./external/module";
import { groupValue, timelineValue } from "../../../tools/view-dev/work/timeline";
import { max_active_view_roots, max_view_frame_instances } from "./limits";

const context:Context={mode:"window",columns:100,lines:4000,density:"normal",locale:"en-GB",timeZone:"UTC"};
const definition=(name:string)=>valueViewModules.named(name)!.definition!;
function frame():ViewFrame{return {root:"board",instances:[
  {id:"board",instance:"board-identity",query:null,inputReference:{kind:"unlinked"},inputDelivery:"finite",observing:false,inputRevision:"0",inputProblem:null,inputCautions:[],linkedInputs:[],revision:"1",definition:"dashboard",digest:definition("dashboard").digest,
    input:{type:{kind:"record",name:"Settings",fields:[{name:"view",type:{kind:"primitive",name:"TEXT"}},{name:"title",type:{kind:"primitive",name:"TEXT"}}]},data:{view:"dashboard",title:"Overview"},provenance:{}},members:{members:["card"]}},
  {id:"card",instance:"card-identity",query:null,inputReference:{kind:"unlinked"},inputDelivery:"finite",observing:false,inputRevision:"0",inputProblem:null,inputCautions:[],linkedInputs:[],revision:"0",definition:"metric",digest:definition("metric").digest,
    input:{type:{kind:"record",name:"Sample",fields:[{name:"view",type:{kind:"primitive",name:"TEXT"}},{name:"value",type:{kind:"primitive",name:"INT"}}]},data:{view:"metric",value:1250},provenance:{}},members:{}},
]};}
beforeEach(()=>{localPackage("metric",metric);localPackage("dashboard",dashboard);localPackage("choice",choiceRenderer);});
afterEach(()=>{vi.restoreAllMocks();vi.useRealTimers();});

it("releases the old mount when the workspace generation changes even if a node ID repeats",()=>{
  let generation="first";
  const close=vi.fn(),watchViewFrame=vi.fn(()=>close);
  const engine={viewGeneration:()=>generation,watchViewFrame} as unknown as import("../engine").Engine;
  const value:import("../protocol").StoredValue={type:{kind:"meta",name:"ViewInstance"},data:{id:"id1",instance:"identity"},provenance:{}};
  let tree!:ReturnType<typeof create>;
  act(()=>{tree=create(<InstanceView value={value} engine={engine} mode="window"/>);});
  expect(watchViewFrame).toHaveBeenCalledWith("id1","identity",expect.any(Function));
  generation="second";
  act(()=>tree.update(<InstanceView value={value} engine={engine} mode="window"/>));
  expect(close).toHaveBeenCalledTimes(1);
  expect(watchViewFrame).toHaveBeenCalledTimes(2);
  act(()=>tree.unmount());
});

it("renders independent bound members through the same packaged renderer",()=>{
  const root=presentFrame(frame(),context);
  expect(root.kind).toBe("view");
  if(root.kind!=="view")throw Error("view");
  expect(root.model).toHaveProperty("input.title","Overview");
  expect(root.children[0]).toHaveProperty("model.input.value",1250);
});
it("keeps independent timeline identities and waiting tracks in one group",()=>{
  const group=groupValue(0), data=timelineValue(), defaults=frame().instances[1]!;
  const f:ViewFrame={root:"group",instances:[
    {...defaults,id:"group",instance:"group-instance",definition:"timeline-group",digest:definition("timeline-group").digest,input:group,members:{members:["first","second","waiting"]}},
    ...["first","second","waiting"].map(id=>({...defaults,id,instance:`${id}-instance`,definition:"timeline",digest:definition("timeline").digest,input:id==="waiting"?null:data})),
  ]};
  const shown=presentFrame(f,context);
  expect(shown.kind).toBe("view");if(shown.kind!=="view")return;
  expect(shown.children.slice(0,2).map(child=>(child as {model:{identity:string}}).model.identity)).toEqual(["first-instance","second-instance"]);
  expect(shown.children).toHaveLength(3);
  expect(shown.children[2]).toHaveProperty("waiting");
  expect((data.data as {id:string}).id).not.toBe("first-instance");
});
it("isolates identical timeline inputs in separate roots from a coordinator and its separately displayed member",async()=>{
  vi.useFakeTimers();localPackage("timeline",timelineRenderer);localPackage("timeline-group",groupRenderer);
  const defaults=frame().instances[1]!,line=definition("timeline"),group=definition("timeline-group"),input=timelineValue();
  const chart=(id:string)=>({...defaults,id,instance:`${id}-instance`,definition:line.id,digest:line.digest,input,members:{}});
  const member=chart("member"),standalone=chart("standalone");
  const coordinator={...defaults,id:"group",instance:"group-instance",definition:group.id,digest:group.digest,input:groupValue(),members:{members:["member"]}};
  const frames:Record<string,ViewFrame>={group:{root:"group",instances:[coordinator,member]},member:{root:"member",instances:[member]},standalone:{root:"standalone",instances:[standalone]}};
  const viewport={start:"2030-06-15T08:00:00Z",end:"2030-06-15T08:10:00Z"};
  const states=new Map([coordinator,standalone].map(entry=>[entry.id,{owner:entry.id,identity:entry.instance,definitionRevision:"0",revision:"1",definition:entry.definition,digest:entry.digest,artifact:definition(entry.definition).artifact,fields:{viewport,selection:null,selectedItem:null},outputs:{selection:null,selectedItem:null}} as import("./shared-state").SharedState]));
  const listeners=new Map<string,Set<(sample:FrameSample<import("./shared-state").SharedState>)=>void>>();
  const commitViewState=vi.fn(async(_id:string,_identity:string,_generation:string,edit:import("./shared-state").SharedCommit)=>{
    const previous=states.get(edit.owner)!,state={...previous,fields:edit.fields,outputs:edit.outputs,revision:String(BigInt(previous.revision)+1n)};
    states.set(edit.owner,state);listeners.get(edit.owner)?.forEach(listener=>listener({frame:state}));return {state,conflict:false};
  });
  const engine={viewGeneration:()=>"session",watchViewFrame:(id:string,_identity:string,listener:(sample:FrameSample)=>void)=>{listener({frame:frames[id]!});return ()=>{};},
    watchViewState:(id:string,_identity:string,_generation:string,listener:(sample:FrameSample<import("./shared-state").SharedState>)=>void)=>{const owner=id==="member"?"group":id,set=listeners.get(owner)??new Set();listeners.set(owner,set);set.add(listener);listener({frame:states.get(owner)!});return ()=>{set.delete(listener);};},commitViewState} as unknown as import("../engine").Engine;
  let tree!:ReturnType<typeof create>;act(()=>{tree=create(<>{[coordinator,member,standalone].map(entry=><InstanceView key={entry.id} value={{type:{kind:"meta",name:"ViewInstance"},data:{id:entry.id,instance:entry.instance},provenance:{}}} engine={engine} mode="window"/>)}</>);});
  const plots=()=>tree.root.findAllByProps({className:"timeline-svg"});
  const target={getBoundingClientRect:()=>({left:0,width:1000}),focus(){},setPointerCapture(){},releasePointerCapture(){}};
  const select=async(index:number)=>{
    act(()=>plots()[index]!.props.onPointerDown({currentTarget:target,clientX:400,button:0,pointerId:1}));
    act(()=>plots()[index]!.props.onPointerMove({currentTarget:target,clientX:800}));
    await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
    act(()=>plots()[index]!.props.onPointerUp({currentTarget:target,clientX:800,pointerId:1}));
    await act(async()=>{await vi.advanceTimersByTimeAsync(200);});
  };
  await select(2);expect(states.get("group")!.fields.selection).toBeNull();
  expect(tree.root.findAllByProps({className:"timeline-selection"})).toHaveLength(1);
  await select(0);expect(tree.root.findAllByProps({className:"timeline-selection"})).toHaveLength(3);
  const groupSelection=states.get("group")!.fields.selection,groupRevision=states.get("group")!.revision;
  const standaloneRoot=tree.root.findAllByType(InstanceView)[2]!;
  const clear=standaloneRoot.findAllByType("button").find(button=>button.children[0]==="Clear selection")!;
  act(()=>clear.props.onClick());await act(async()=>{await vi.advanceTimersByTimeAsync(200);});
  expect(states.get("standalone")!.fields.selection).toBeNull();
  expect(states.get("group")!.fields.selection).toEqual(groupSelection);expect(states.get("group")!.revision).toBe(groupRevision);
  expect(tree.root.findAllByProps({className:"timeline-selection"})).toHaveLength(2);
  act(()=>tree.unmount());expect([...listeners.values()].every(set=>set.size===0)).toBe(true);
});

it("checks digest, membership, missing inputs and cycles before rendering",()=>{
  const base=frame();
  expect(()=>presentFrame({...base,instances:[{...base.instances[0]!,digest:"wrong"},base.instances[1]!]},context)).toThrow(/match/);
  expect(presentFrame({...base,instances:[{...base.instances[0]!,input:null},base.instances[1]!]},context)).toMatchObject({waiting:expect.stringContaining("Bind")});
  expect(()=>presentFrame({...base,instances:[{...base.instances[0]!,members:{members:["board"]}}]},context)).toThrow(/cycle/);
  expect(()=>presentFrame({...base,instances:[{...base.instances[0]!,members:{missing:["card"]}},base.instances[1]!]},context)).toThrow(/slot/);
});
it("reprepares a member when coordination changes even if its data entry is reused",()=>{
  const base=frame(),defaults=base.instances[1]!,group=definition("timeline-group"),line=definition("timeline");
  const parent={...defaults,id:"group",instance:"group-instance",definition:group.id,digest:group.digest,input:groupValue(),members:{members:["chart"]}};
  const chart={...defaults,id:"chart",instance:"chart-instance",definition:line.id,digest:line.digest,input:timelineValue(),members:{}};
  const f:ViewFrame={root:"board",instances:[{...base.instances[0]!,members:{members:["group","chart"]}},parent,chart]};
  const cache=new FramePresentationCache(),before=presentFrame(f,context,undefined,cache);
  const after=presentFrame({...f,instances:[f.instances[0]!,{...parent,members:{}},chart]},context,undefined,cache);
  if(before.kind!=="view"||after.kind!=="view")throw Error("dashboard");
  expect(before.children[1]).toHaveProperty("model.coordinated",true);
  expect(after.children[1]).toHaveProperty("model.coordinated",false);
  expect(after.children[1]).not.toBe(before.children[1]);
});
it("TimelineGroup receives prepared Timeline members without nesting their source records",()=>{
  const input=groupValue(0);
  const frame:ViewFrame={root:"group",instances:[
    {id:"group",instance:"group-identity",query:null,inputReference:{kind:"unlinked"},inputDelivery:"finite",observing:false,inputRevision:"0",inputProblem:null,inputCautions:[],linkedInputs:[],revision:"1",definition:"timeline-group",digest:definition("timeline-group").digest,input,members:{members:["chart"]}},
    {id:"chart",instance:"chart-identity",query:null,inputReference:{kind:"unlinked"},inputDelivery:"finite",observing:false,inputRevision:"0",inputProblem:null,inputCautions:[],linkedInputs:[],revision:"0",definition:"timeline",digest:definition("timeline").digest,input:timelineValue(),members:{}},
  ]};
  const node=presentFrame(frame,context);
  expect(node.kind).toBe("view");
  if(node.kind!=="view")throw new Error("view");
  expect(node.children).toHaveLength(1);
  expect((node.model as {input:Record<string,unknown>}).input).not.toHaveProperty("members");
  expect(node.children[0]!.kind).toBe("view");
});
it("coordinates three compact plots by declared owner without leaking hover or selection to another group",async()=>{
  vi.useFakeTimers();localPackage("timeline",timelineRenderer);localPackage("timeline-group",groupRenderer);
  const base=frame(),defaults=base.instances[1]!,group=definition("timeline-group"),line=definition("timeline");
  const groupEntry=(id:string,members:string[])=>({...defaults,id,instance:`${id}-instance`,definition:group.id,digest:group.digest,input:groupValue(),members:{members}});
  const current:ViewFrame={root:"board",instances:[{...base.instances[0]!,members:{members:["one","two"]}},groupEntry("one",["a","b","c"]),groupEntry("two",["d"]),...["a","b","c","d"].map(id=>({...defaults,id,instance:`${id}-instance`,definition:line.id,digest:line.digest,input:timelineValue(id),members:{}}))]};
  const viewport={start:"2030-06-15T08:00:00Z",end:"2030-06-15T08:10:00Z"};
  const owners:Record<string,string>={one:"one",a:"one",b:"one",c:"one",two:"two",d:"two"};
  const states=new Map(["one","two"].map(owner=>[owner,{owner,identity:`${owner}-instance`,definitionRevision:"0",revision:"1",definition:group.id,digest:group.digest,artifact:group.artifact,fields:{viewport,selection:null,selectedItem:null},outputs:{selection:null,selectedItem:null}} as import("./shared-state").SharedState]));
  const listeners=new Map<string,Set<(sample:FrameSample<import("./shared-state").SharedState>)=>void>>();
  const commitViewState=vi.fn(async(_id:string,_identity:string,_generation:string,edit:import("./shared-state").SharedCommit)=>{
    const previous=states.get(edit.owner)!,state={...previous,fields:edit.fields,outputs:edit.outputs,revision:String(BigInt(previous.revision)+1n)};
    states.set(edit.owner,state);listeners.get(edit.owner)?.forEach(listener=>listener({frame:state}));return {state,conflict:false};
  });
  const engine={viewGeneration:()=>"session",watchViewFrame:(_id:string,_identity:string,listener:(sample:FrameSample)=>void)=>{listener({frame:current});return ()=>{};},
    watchViewState:(id:string,_identity:string,_generation:string,listener:(sample:FrameSample<import("./shared-state").SharedState>)=>void)=>{const owner=owners[id]!,set=listeners.get(owner)??new Set();listeners.set(owner,set);set.add(listener);listener({frame:states.get(owner)!});return ()=>{set.delete(listener);};},commitViewState} as unknown as import("../engine").Engine;
  let tree!:ReturnType<typeof create>;act(()=>{tree=create(<InstanceView value={{type:{kind:"meta",name:"ViewInstance"},data:{id:"board",instance:"board-identity"},provenance:{}}} engine={engine} mode="window"/>);});
  const plots=()=>tree.root.findAllByProps({className:"timeline-svg"});
  expect(plots()).toHaveLength(4);
  expect(tree.root.findAllByProps({className:"timeline-toolbar"})).toHaveLength(2);
  expect(tree.root.findAllByProps({className:"timeline-inspection"})).toHaveLength(0);
  const target={getBoundingClientRect:()=>({left:0,width:1000}),focus(){},setPointerCapture(){},releasePointerCapture(){}};
  act(()=>plots()[0]!.props.onPointerMove({currentTarget:target,clientX:500}));
  await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
  expect(tree.root.findAllByProps({className:"timeline-cursor"})).toHaveLength(3);
  expect(commitViewState).not.toHaveBeenCalled();
  act(()=>{plots()[0]!.props.onPointerDown({currentTarget:target,clientX:300,button:0,pointerId:1});plots()[0]!.props.onPointerMove({currentTarget:target,clientX:700});});
  await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
  expect(tree.root.findAllByProps({className:"timeline-draft"})).toHaveLength(3);
  act(()=>plots()[0]!.props.onPointerUp({currentTarget:target,clientX:700,pointerId:1}));
  await act(async()=>{await vi.advanceTimersByTimeAsync(200);});
  expect(tree.root.findAllByProps({className:"timeline-selection"})).toHaveLength(3);
  expect(states.get("two")!.fields.selection).toBeNull();
  expect(commitViewState).toHaveBeenCalledTimes(1);
  act(()=>tree.unmount());
});
it("coalesces finite invalidations, shares mount reads and ignores disposed replies",async()=>{
  vi.useFakeTimers();
  let resolve!:(value:{frame:ViewFrame;etag:string})=>void;
  const fetch=vi.fn(()=>new Promise<{frame:ViewFrame;etag:string}>(r=>{resolve=r;}));
  const reader=new ViewFrameReader(fetch),a:FrameSample[]=[],b:FrameSample[]=[];
  const closeA=reader.watch("board","session",s=>a.push(s));
  const closeB=reader.watch("board","session",s=>b.push(s));
  for(let i=0;i<1000;i++)reader.invalidate();
  await vi.advanceTimersByTimeAsync(100);
  expect(fetch).toHaveBeenCalledTimes(1);
  const result=frame();resolve({frame:result,etag:"one"});await vi.advanceTimersByTimeAsync(1);
  expect(a.at(-1)?.frame).toBe(result);expect(b.at(-1)?.frame).toBe(result);
  reader.invalidate();await vi.advanceTimersByTimeAsync(100);
  closeA();closeB();resolve({frame:frame(),etag:"two"});await vi.advanceTimersByTimeAsync(1);
  expect(a.at(-1)?.frame).toBe(result);expect(b.at(-1)?.frame).toBe(result);
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("retains the last frame across bounded transient reads and clears it on a terminal refusal",async()=>{
  vi.useFakeTimers();
  const original=frame(), updated={...original,root:"updated"};
  let result:{frame:ViewFrame;etag:string}|{retry:true}={frame:original,etag:"one"};
  let denied=false;
  const read=vi.fn(async()=>{if(denied)throw Error("Forbidden");return result;});
  const reader=new ViewFrameReader(read,250),seen:FrameSample[]=[];
  const close=reader.watch("view","generation",v=>seen.push(v));
  await vi.advanceTimersByTimeAsync(251);
  result={retry:true};await vi.advanceTimersByTimeAsync(500);
  expect(seen.at(-1)?.frame).toBe(original);expect(seen.at(-1)?.problem).toBeUndefined();
  result={frame:updated,etag:"two"};await vi.advanceTimersByTimeAsync(250);
  expect(seen.at(-1)?.frame).toBe(updated);
  denied=true;await vi.advanceTimersByTimeAsync(250);
  expect(seen.at(-1)).toEqual({problem:"Forbidden"});
  const calls=read.mock.calls.length;await vi.advanceTimersByTimeAsync(5000);
  expect(read).toHaveBeenCalledTimes(calls);close();
});
it("caps transient retries even for polling views",async()=>{
  vi.useFakeTimers();
  const read=vi.fn(async()=>({retry:true as const})),seen:FrameSample[]=[];
  const reader=new ViewFrameReader(read,250),close=reader.watch("view","generation",v=>seen.push(v));
  await vi.advanceTimersByTimeAsync(10000);
  expect(read).toHaveBeenCalledTimes(9);expect(seen.at(-1)?.problem).toMatch(/busy/);
  close();
});

it("reads shared states for all members of admitted roots while keeping only two requests in flight",async()=>{
  vi.useFakeTimers();
  const finish:(()=>void)[]=[],seen:FrameSample<number>[][]=[];
  let active=0,peak=0;
  const read=vi.fn(async()=>{
    active++;peak=Math.max(peak,active);
    await new Promise<void>(resolve=>finish.push(resolve));active--;
    return {frame:1,etag:"one"};
  });
  const capacity=max_active_view_roots()*max_view_frame_instances();
  const reader=new ViewFrameReader<number>(read,undefined,undefined,capacity);
  const closes=Array.from({length:32},(_,n)=>{const samples:FrameSample<number>[]=[];seen.push(samples);return reader.watch(`member${n}`,"generation",sample=>samples.push(sample));});
  await vi.advanceTimersByTimeAsync(100);expect(read).toHaveBeenCalledTimes(2);
  for(let n=0;n<16;n++){finish.splice(0).forEach(resolve=>resolve());await vi.advanceTimersByTimeAsync(100);}
  expect(read).toHaveBeenCalledTimes(32);expect(peak).toBe(2);
  expect(seen.every(samples=>samples.at(-1)?.frame===1&&!samples.some(sample=>sample.problem))).toBe(true);
  closes.forEach(close=>close());
});
it("updates small linked input fields without preparing unrelated members again",()=>{
  const artifact="a".repeat(64);
  const module=externalModule({digest:artifact,definition:{...rangeSummary,artifact},javascript:"export {};",css:""});
  const dispose=valueViewModules.register(module);
  const f=frame(), summary=definition("range-summary");
  const base:import("../protocol").StoredValue={type:{kind:"record",name:"RangeSummary",fields:[
    {name:"view",type:{kind:"primitive",name:"TEXT"}},{name:"title",type:{kind:"primitive",name:"TEXT"}},
    {name:"selection",type:{kind:"option",element:{kind:"primitive",name:"INTERVAL"}}},
  ]},data:{view:"range-summary",title:"Selected range",selection:{kind:"none"}},provenance:{}};
  const linked:ViewFrame={...f,instances:[{...f.instances[0]!,members:{members:["card","detail"]}},f.instances[1]!,{
    id:"detail",instance:"detail-identity",definition:summary.id,digest:summary.digest,revision:"1",query:null,inputReference:{kind:"unlinked"},inputDelivery:"finite",observing:false,inputRevision:"0",inputProblem:null,inputCautions:[],linkedInputs:["selection"],input:base,members:{},
  }]};
  const cache=new FramePresentationCache();
  const card=vi.spyOn(valueViewModules.named("metric")!,"present");
  const detail=vi.spyOn(valueViewModules.named("range-summary")!,"present");
  const none={type:{kind:"option" as const,element:{kind:"primitive" as const,name:"INTERVAL"}},data:{kind:"none"},provenance:{}};
  const first=presentFrame(linked,context,{cautions:{},values:{detail:{selection:none}},problems:{}},cache);
  const text="2026-01-01T00:00:00Z/2026-01-01T00:10:00Z";
  const second=presentFrame(linked,context,{cautions:{},values:{detail:{selection:{...none,data:{kind:"some",value:text}}}},problems:{}},cache);
  expect(card).toHaveBeenCalledTimes(1);expect(detail).toHaveBeenCalledTimes(2);
  if(first.kind!=="view"||second.kind!=="view")throw new Error("view");
  expect(first.children[0]).toBe(second.children[0]);
  expect(base.data).toEqual({view:"range-summary",title:"Selected range",selection:{kind:"none"}});
  expect(second.children[1]).toHaveProperty("model.input.selection",text);
  const pending=presentFrame(linked,context,{cautions:{},values:{},problems:{detail:"Waiting for a committed source output"}},cache);
  if(pending.kind!=="view")throw new Error("view");
  expect(pending.children[0]).toBe(first.children[0]);
  expect(pending.children[1]).toMatchObject({waiting:"Waiting for a committed source output"});
  card.mockRestore();detail.mockRestore();dispose();
});

it("polls only while a frame declares active observation and stops after the final mount",async()=>{
  vi.useFakeTimers();let observing=true;
  const read=vi.fn(async()=>({frame:{...frame(),instances:frame().instances.map(i=>({...i,inputReference:{kind:"current" as const,node:"feed",port:"data" as const,fields:[],shownRun:"r1"},inputDelivery:"window" as const,observing}))},etag:"v"}));
  const reader=new ViewFrameReader(read,undefined,f=>f.instances.some(i=>i.observing));
  const close=reader.watch("board","session",()=>{});
  await vi.advanceTimersByTimeAsync(100);expect(read).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(500);expect(read).toHaveBeenCalledTimes(3);
  observing=false;await vi.advanceTimersByTimeAsync(250);expect(read).toHaveBeenCalledTimes(4);
  await vi.advanceTimersByTimeAsync(1000);expect(read).toHaveBeenCalledTimes(4);
  close();
});

it("reuses exact delta entries and rejects cross-instance or missing base frames",()=>{
  const original=frame();
  const delta={...original,instances:original.instances.map(i=>({...i,input:null,unchanged:true}))};
  const result=mergeViewFrame(delta,original);
  expect(result.instances[0]).toBe(original.instances[0]);expect(result.instances[1]).toBe(original.instances[1]);
  expect(()=>mergeViewFrame(delta)).toThrow(/base/);
  expect(()=>mergeViewFrame({...delta,instances:delta.instances.map(i=>({...i,instance:"reused-node"}))},original)).toThrow(/base/);
  expect(()=>mergeViewFrame({...delta,instances:delta.instances.map(i=>({...i,inputRevision:"99"}))},original)).toThrow(/base/);
});

it("keeps available dashboard members visible when a saved source result is unavailable",()=>{
  const f=frame();const partial={...f,instances:f.instances.map(i=>i.id==="card"?{...i,input:null,inputProblem:"Saved source result is unavailable"}:i)};
  const node=presentFrame(partial,context);
  if(node.kind!=="view")throw new Error("dashboard");
  expect(node.children[0]).toMatchObject({kind:"view",waiting:"Saved source result is unavailable"});
});

it("renders the previous query result while running and starts work only on explicit Apply",async()=>{
  const base=frame();
  let current:ViewFrame={...base,instances:base.instances.map(i=>i.id==="card"?{...i,query:{environment:null,template:"Observe",mode:"finite" as const,adapter:null,source:"selection",output:"selection",trigger:"commit" as const,running:false}}:i)};
  let receive!:(sample:FrameSample)=>void;
  const applyViewQuery=vi.fn(async()=>{}),viewObservation=vi.fn(async()=>{}),close=vi.fn();
  const engine={viewGeneration:()=>"session",watchViewFrame:vi.fn((_id,_identity,listener)=>{receive=listener;listener({frame:current});return close;}),applyViewQuery,viewObservation} as unknown as import("../engine").Engine;
  const value:import("../protocol").StoredValue={type:{kind:"meta",name:"ViewInstance"},data:{id:"board",instance:"board-identity"},provenance:{}};
  let tree!:ReturnType<typeof create>;
  act(()=>{tree=create(<InstanceView value={value} engine={engine} mode="window"/>);});
  expect(applyViewQuery).not.toHaveBeenCalled();
  await act(async()=>{tree.root.findAllByType("button").find(b=>b.children.includes("Apply"))!.props.onClick();});
  expect(applyViewQuery).toHaveBeenCalledWith("card","session");
  current={...current,instances:current.instances.map(i=>i.query?{...i,observing:true,query:{...i.query,running:true}}:i)};
  act(()=>receive({frame:current}));
  expect(JSON.stringify(tree.toJSON())).toContain("previous result remains visible");
  expect(JSON.stringify(tree.toJSON())).toContain("1250");
  await act(async()=>{tree.root.findAllByType("button").find(b=>b.children.includes("Stop query"))!.props.onClick();});
  expect(viewObservation).toHaveBeenCalledWith("card","card-identity","session",false);
  act(()=>tree.unmount());expect(close).toHaveBeenCalledOnce();
});
it("captures evidence through a normal engine command without starting a query",async()=>{
  const captureViewResult=vi.fn(async()=>{}),applyViewQuery=vi.fn(),engine={viewGeneration:()=>"session",watchViewFrame:(_id:string,_identity:string,listener:(sample:FrameSample)=>void)=>{listener({frame:frame()});return ()=>{};},captureViewResult,applyViewQuery} as unknown as import("../engine").Engine;
  let tree!:ReturnType<typeof create>;
  act(()=>{tree=create(<InstanceView value={{type:{kind:"meta",name:"ViewInstance"},data:{id:"board",instance:"board-identity"},provenance:{}}} engine={engine} mode="window"/>);});
  const button=tree.root.findAllByType("button").find(button=>button.children.includes("Save snapshot as result"))!;
  await act(async()=>{await button.props.onClick();});
  expect(captureViewResult).toHaveBeenCalledWith("board","session",undefined);expect(applyViewQuery).not.toHaveBeenCalled();
  act(()=>tree.unmount());
});
it("explains what a static view snapshot creates without inventing output ports",async()=>{
  const base=frame().instances[1]!,current:ViewFrame={root:base.id,instances:[base]};
  const captureViewResult=vi.fn(async()=>{}),applyViewQuery=vi.fn(),engine={viewGeneration:()=>"session",watchViewFrame:(_id:string,_identity:string,listener:(sample:FrameSample)=>void)=>{listener({frame:current});return ()=>{};},captureViewResult,applyViewQuery} as unknown as import("../engine").Engine;
  let tree!:ReturnType<typeof create>;
  act(()=>{tree=create(<InstanceView value={{type:{kind:"meta",name:"ViewInstance"},data:{id:base.id,instance:base.instance},provenance:{}}} engine={engine} mode="window"/>);});
  expect(tree.root.findByType("summary").children.join("")).toMatch(/snapshot/i);
  const text=JSON.stringify(tree.toJSON());
  expect(text).toMatch(/inputs/i);expect(text).toMatch(/where they came from/i);expect(text).toMatch(/separate result/i);
  expect(text).not.toContain("Committed data only");
  expect(text).not.toContain("connected views");
  expect(text).not.toContain("confirmed view state");
  expect(tree.root.findAllByType("button").map(button=>button.children.join(""))).toEqual(["Save snapshot as result","Pin input"]);
  expect(captureViewResult).not.toHaveBeenCalled();
  const button=tree.root.findAllByType("button").find(button=>button.children.includes("Save snapshot as result"))!;
  await act(async()=>{await button.props.onClick();});
  expect(captureViewResult).toHaveBeenCalledWith(base.id,"session",undefined);
  expect(applyViewQuery).not.toHaveBeenCalled();
  act(()=>tree.unmount());
});
it("starts and stops observation of committed results without applying a query or capturing a result",async()=>{
  const base=frame();
  let current:ViewFrame={...base,instances:base.instances.map(i=>i.id==="card"?{...i,query:{environment:null,template:"Observe",mode:"finite" as const,adapter:null,source:"selection",output:"selection",trigger:"commit" as const,running:false}}:{...i,inputReference:{kind:"current" as const,node:"orders",port:"data" as const,fields:["body"],shownRun:"r1"}})};
  let receive!:(sample:FrameSample)=>void;
  const applyViewQuery=vi.fn(async()=>{}),viewObservation=vi.fn(async()=>{}),captureViewResult=vi.fn(async()=>{}),submit=vi.fn(async()=>{});
  const engine={viewGeneration:()=>"session",watchViewFrame:vi.fn((_id,_identity,listener)=>{receive=listener;listener({frame:current});return ()=>{};}),applyViewQuery,viewObservation,captureViewResult,submit} as unknown as import("../engine").Engine;
  const value:import("../protocol").StoredValue={type:{kind:"meta",name:"ViewInstance"},data:{id:"board",instance:"board-identity"},provenance:{}};
  let tree!:ReturnType<typeof create>;
  act(()=>{tree=create(<InstanceView value={value} engine={engine} mode="window"/>);});
  const observe=(name:string)=>tree.root.findAllByType("button").find(b=>b.children.includes(name))!;
  await act(async()=>observe("Start observing").props.onClick());
  expect(viewObservation).toHaveBeenCalledWith("board","board-identity","session",true);
  current={...current,instances:current.instances.map(i=>i.id==="board"?{...i,observing:true}:i)};
  act(()=>receive({frame:current}));
  expect(observe("Stop observing").props["aria-description"]).toContain("Source commands keep running");
  expect(observe("Stop observing").props["aria-description"]).toContain("reopening the view does not resume reading");
  await act(async()=>observe("Stop observing").props.onClick());
  expect(viewObservation).toHaveBeenLastCalledWith("board","board-identity","session",false);
  // A source problem revokes intent in the engine; the control then shows the effective state, not a pause.
  current={...current,instances:current.instances.map(i=>i.id==="board"?{...i,observing:false,inputProblem:"Source unavailable"}:i)};
  act(()=>receive({frame:current}));
  expect(JSON.stringify(tree.toJSON())).toContain("Not reading");expect(JSON.stringify(tree.toJSON())).not.toContain("paused");
  expect(JSON.stringify(tree.toJSON())).toContain("Query not running · Apply to run it");
  expect(applyViewQuery).not.toHaveBeenCalled();expect(captureViewResult).not.toHaveBeenCalled();expect(submit).not.toHaveBeenCalled();
  act(()=>tree.unmount());
});

it("holds Apply until the mounted selection has been confirmed",async()=>{
  vi.useFakeTimers();
  const text={kind:"primitive" as const,name:"TEXT" as const};
  const choice=definition("choice"),base=frame();
  const selected={...base.instances[1]!,id:"choice",instance:"choice-identity",definition:"choice",digest:choice.digest,input:{type:{kind:"record" as const,name:"Selection",fields:[{name:"view",type:text},{name:"title",type:text},{name:"options",type:{kind:"list" as const,element:{kind:"record" as const,name:"Option",fields:[{name:"value",type:text},{name:"label",type:text}]}}}]},data:{view:"choice",title:"Feed",options:[{value:"a",label:"A"},{value:"b",label:"B"}]},provenance:{}}};
  const current:ViewFrame={root:"board",instances:[{...base.instances[0]!,members:{members:["choice","card"]}},selected,{...base.instances[1]!,query:{environment:null,template:"Observe",mode:"finite",adapter:null,source:"choice",output:"value",trigger:"manual",running:false}}]};
  const shared={owner:"choice",identity:"choice-identity",definitionRevision:"0",revision:"1",definition:"choice",digest:choice.digest,fields:{value:"a"},outputs:{value:"a"}};
  let confirm!:(value:unknown)=>void;
  const engine={viewGeneration:()=>"session",watchViewFrame:(_id:unknown,_identity:unknown,listener:(sample:FrameSample)=>void)=>{listener({frame:current});return ()=>{};},watchViewState:(_id:unknown,_identity:unknown,_generation:unknown,listener:(sample:unknown)=>void)=>{listener({frame:shared});return ()=>{};},commitViewState:vi.fn(()=>new Promise(resolve=>{confirm=resolve;}))} as unknown as import("../engine").Engine;
  let tree!:ReturnType<typeof create>;
  act(()=>{tree=create(<InstanceView value={{type:{kind:"meta",name:"ViewInstance"},data:{id:"board",instance:"board-identity"},provenance:{}}} engine={engine} mode="window"/>);});
  const apply=()=>tree.root.findAllByType("button").find(b=>b.children.includes("Apply"))!;
  expect(apply().props.disabled).toBe(false);
  act(()=>tree.root.findByType("select").props.onChange({target:{value:"b"}}));expect(apply().props.disabled).toBe(true);
  await act(async()=>{await vi.advanceTimersByTimeAsync(50);});expect(apply().props.disabled).toBe(true);
  await act(async()=>{confirm({state:{...shared,revision:"2",fields:{value:"b"},outputs:{value:"b"}},conflict:false});await Promise.resolve();});
  expect(apply().props.disabled).toBe(false);act(()=>tree.unmount());
});

function referenceView(over:Partial<ViewFrame["instances"][number]>,pinViewInput=vi.fn(async()=>"card_pin")){
  let current:ViewFrame={root:"card",instances:[{...frame().instances[1]!,...over}]};
  let receive!:(sample:FrameSample)=>void;
  const viewObservation=vi.fn(async()=>{}),captureViewResult=vi.fn(async()=>{}),submit=vi.fn(async()=>{});
  const engine={viewGeneration:()=>"session",watchViewFrame:(_id:string,_identity:string,listener:(sample:FrameSample)=>void)=>{receive=listener;listener({frame:current});return ()=>{};},
    viewObservation,captureViewResult,submit,pinViewInput,watchViewInputs:()=>()=>{}} as unknown as import("../engine").Engine;
  let tree!:ReturnType<typeof create>;
  act(()=>{tree=create(<InstanceView value={{type:{kind:"meta",name:"ViewInstance"},data:{id:"card",instance:"card-identity"},provenance:{}}} engine={engine} mode="window"/>);});
  const footer=()=>tree.root.findByProps({"aria-label":"View input"});
  const footerText=()=>footer().findAll(node=>typeof node.type==="string").flatMap(node=>node.children.filter(child=>typeof child==="string")).join(" ");
  const button=(name:string)=>footer().findAllByType("button").find(b=>b.children.join("")===name);
  const update=(next:Partial<ViewFrame["instances"][number]>)=>{current={...current,instances:[{...current.instances[0]!,...next}]};act(()=>receive({frame:current}));};
  return {tree,footer,footerText,button,update,pinViewInput,viewObservation,captureViewResult,submit};
}

it("should_LabelACurrentReferenceBelowTheView_When_TheInputFollowsAWorkOutput",()=>{
  // Arrange
  const view=referenceView({inputReference:{kind:"current",node:"orders",port:"data",fields:["body"],shownRun:"r7"},inputDelivery:"window",observing:true});
  // Act
  const text=view.footerText();
  // Assert
  expect(view.footer().findByType("strong").children.join("")).toBe("Current");
  expect(text).toContain("$orders.body · stream window");
  const mainLabel=()=>view.footer().findByProps({className:"view-reference-label"}).findAll(node=>typeof node.type==="string").flatMap(node=>node.children.filter(child=>typeof child==="string")).join(" ");
  const stableLabel=mainLabel();
  expect(stableLabel).not.toContain("r7");
  expect(view.footer().findByType("details").findByType("summary").children.join("")).toBe("Input details");
  expect(text).toContain("Input run r7");
  expect(text).toContain("it is not the stream connection ID");
  view.update({inputReference:{kind:"current",node:"orders",port:"data",fields:["body"],shownRun:"r8"}});
  expect(mainLabel()).toBe(stableLabel);
  expect(view.footerText()).toContain("Input run r8");
  expect(view.button("Stop observing")).toBeDefined();
  expect(text).not.toMatch(/mode|snapshot|latest/i);
  act(()=>view.tree.unmount());
});

it("should_PinTheExactVisibleFrameOnce_When_PinInputIsPressed",async()=>{
  // Arrange
  const view=referenceView({inputReference:{kind:"current",node:"orders",port:"data",fields:[],shownRun:"r7"},revision:"4",inputRevision:"9"});
  // Act
  await act(async()=>view.button("Pin input")!.props.onClick());
  // Assert
  expect(view.pinViewInput).toHaveBeenCalledTimes(1);
  expect(view.pinViewInput).toHaveBeenCalledWith(expect.objectContaining({id:"card",instance:"card-identity",revision:"4",inputRevision:"9"}),"session");
  expect(view.submit).not.toHaveBeenCalled();expect(view.captureViewResult).not.toHaveBeenCalled();expect(view.viewObservation).not.toHaveBeenCalled();
  expect(JSON.stringify(view.tree.toJSON())).toContain("Pin requested as $card_pin");
  expect(view.footer().findByType("strong").children.join("")).toBe("Current");
  act(()=>view.tree.unmount());
});

it("should_ShowPinnedOnlyFromTheEngineReference_When_ThePinBindAdvancesTheRevision",async()=>{
  // Arrange
  const view=referenceView({inputReference:{kind:"current",node:"orders",port:"data",fields:[],shownRun:"r7"},revision:"4"});
  await act(async()=>view.button("Pin input")!.props.onClick());
  // Act
  view.update({inputReference:{kind:"retained",node:"card_pin",run:"r12",handle:"h1",origin:{node:"orders",port:"data",fields:[],run:"r7"}},revision:"5",observing:false});
  // Assert
  expect(view.footer().findByType("strong").children.join("")).toBe("Pinned");
  expect(JSON.stringify(view.tree.toJSON())).toContain("$card_pin run r12 · from $orders run r7");
  expect(JSON.stringify(view.tree.toJSON())).not.toContain("Pin requested");
  expect(view.button("Start observing")).toBeUndefined();expect(view.button("Stop observing")).toBeUndefined();
  expect(view.button("Pin input")!.props.disabled).toBe(true);
  act(()=>view.tree.unmount());
});

it("should_DisablePinWithAReason_When_TheViewHasLinkedInputFields",()=>{
  // Arrange
  const view=referenceView({linkedInputs:["selection"]});
  // Act
  const pin=view.button("Pin input")!;
  // Assert
  expect(pin.props.disabled).toBe(true);
  expect(pin.props["aria-description"]).toMatch(/linked input fields yet\. Nothing was saved/);
  act(()=>view.tree.unmount());
});

it("should_ReportARefusedPinRequestWithoutRetrying_When_SubmissionFails",async()=>{
  // Arrange
  const view=referenceView({},vi.fn(async()=>{throw new Error("Connection lost; outcome unknown");}));
  // Act
  await act(async()=>view.button("Pin input")!.props.onClick());
  // Assert
  expect(view.pinViewInput).toHaveBeenCalledTimes(1);
  expect(JSON.stringify(view.tree.toJSON())).toContain("Connection lost; outcome unknown");
  expect(JSON.stringify(view.tree.toJSON())).not.toContain("Pin requested");
  act(()=>view.tree.unmount());
});

it("should_ScopeTheObservationStatusToMembers_When_ThePinnedRootHasCurrentMembers",()=>{
  // Arrange
  const base=frame();
  const current:ViewFrame={...base,instances:base.instances.map(i=>i.id==="board"
    ?{...i,inputReference:{kind:"retained" as const,node:"board_pin",run:"r3",handle:"h3",origin:null}}
    :{...i,inputReference:{kind:"current" as const,node:"orders",port:"data" as const,fields:[],shownRun:"r1"},observing:true})};
  const engine={viewGeneration:()=>"session",watchViewFrame:(_id:string,_identity:string,listener:(sample:FrameSample)=>void)=>{listener({frame:current});return ()=>{};},watchViewInputs:()=>()=>{}} as unknown as import("../engine").Engine;
  let tree!:ReturnType<typeof create>;
  // Act
  act(()=>{tree=create(<InstanceView value={{type:{kind:"meta",name:"ViewInstance"},data:{id:"board",instance:"board-identity"},provenance:{}}} engine={engine} mode="window"/>);});
  const footer=tree.root.findByProps({"aria-label":"View input"});
  const text=footer.findAll(node=>typeof node.type==="string").flatMap(node=>node.children.filter(child=>typeof child==="string")).join(" ");
  const stop=footer.findAllByType("button").find(button=>button.children.join("")==="Stop observing")!;
  // Assert
  expect(footer.findByType("strong").children.join("")).toBe("Pinned");
  expect(text).toContain("Members: reading committed results");
  expect(text).not.toMatch(/(^|[^:] )Reading committed results/);
  expect(stop.props["aria-description"]).toContain("this view's own input does not change");
  act(()=>tree.unmount());
});
