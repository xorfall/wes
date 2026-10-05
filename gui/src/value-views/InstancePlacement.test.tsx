import {act,create,type ReactTestRenderer} from "react-test-renderer";
import {afterEach,beforeEach,expect,it,vi} from "vitest";
import type {Engine} from "../engine";
import type {StoredValue} from "../protocol";
import {InstanceView} from "./InstanceView";
import type {FrameSample,ViewFrame} from "./instances";
import {localPackage} from "./package.test-support";
import dashboard from "../../../views/dashboard/View";
import metric from "../../../views/metric/View";
import {valueViewModules} from "./registry";
import {ViewDisplays,displayIdentity} from "./displays";
beforeEach(()=>{localPackage("dashboard",dashboard);localPackage("metric",metric);});
afterEach(()=>vi.restoreAllMocks());
const value=(instance="board-instance"):StoredValue=>({type:{kind:"meta",name:"ViewInstance"},data:{id:"board",instance,definition:"Dashboard"},provenance:{}});
function frame(count=1):ViewFrame {
  const settings={query:null,inputReference:{kind:"unlinked" as const},inputDelivery:"finite" as const,observing:false,inputRevision:"0",inputProblem:null,inputCautions:[],linkedInputs:[],revision:"0"};
  const definition=(id:string)=>valueViewModules.named(id)!.definition!;
  return {root:"board",instances:[{...settings,id:"board",instance:"board-instance",definition:"dashboard",digest:definition("dashboard").digest,artifact:definition("dashboard").artifact,
    input:{type:{kind:"record",name:"Dashboard",fields:[{name:"view",type:{kind:"primitive",name:"TEXT"}},{name:"title",type:{kind:"primitive",name:"TEXT"}}]},data:{view:"dashboard",title:"Overview"},provenance:{}},members:{members:Array.from({length:count},(_,n)=>`card${n}`)}},
    ...Array.from({length:count},(_,n)=>({...settings,id:`card${n}`,instance:`card${n}-instance`,definition:"metric",digest:definition("metric").digest,artifact:definition("metric").artifact,members:{},
      input:{type:{kind:"record" as const,name:"Metric",fields:[{name:"view",type:{kind:"primitive" as const,name:"TEXT"}},{name:"value",type:{kind:"primitive" as const,name:"INT"}}]},data:{view:"metric",value:80},provenance:{}}})),
  ]};
}
function fixture(){
  let generation="first",current=frame();
  const records:{listener:(sample:FrameSample)=>void;close:ReturnType<typeof vi.fn>}[]=[];
  const watchViewFrame=vi.fn((_id:string,_instance:string,listener:(sample:FrameSample)=>void)=>{const close=vi.fn();records.push({listener,close});listener({frame:current});return close;});
  const applyViewQuery=vi.fn(),viewObservation=vi.fn(),scrollIntoView=vi.fn(),focus=vi.fn();
  // Every operation that could create a result or run work; placement must call none of them.
  const work={applyViewQuery,viewObservation,captureViewResult:vi.fn(),submit:vi.fn(),rerun:vi.fn(),cancel:vi.fn(),liveView:vi.fn()};
  const engine={viewGeneration:()=>generation,watchViewFrame,...work} as unknown as Engine;
  const render=(referenceOnly=false)=><><InstanceView value={value()} engine={engine} mode="preview" display={{label:"$overview",referenceOnly}}/><InstanceView value={value()} engine={engine} mode="preview" display={{label:"$overview",referenceOnly:true}}/></>;
  let tree!:ReactTestRenderer;
  const mount=(referenceOnly=false)=>act(()=>{tree=create(render(referenceOnly),{createNodeMock:()=>({getBoundingClientRect:()=>({height:410}),scrollIntoView,focus})});});
  return {records,watchViewFrame,applyViewQuery,viewObservation,work,scrollIntoView,focus,mount,render,get tree(){return tree;},
    send:(next:ViewFrame)=>act(()=>{current=next;records.at(-1)!.listener({frame:next});}),generation:(next:string)=>act(()=>{generation=next;tree.update(render());})};
}
it("keeps one anchor through member updates and navigates references without reloading or source execution",()=>{
  const f=fixture();f.mount();const anchors=()=>f.tree.root.findAllByProps({className:"view-instance-anchor"});
  expect(anchors()).toHaveLength(1);expect(f.watchViewFrame).toHaveBeenCalledOnce();const first=f.tree.root.findByProps({className:"metric"});
  expect(anchors()[0]!.props.style).toBeUndefined();f.send(frame(4));
  expect(anchors()).toHaveLength(1);expect(anchors()[0]!.props.style).toBeUndefined();
  expect(f.tree.root.findAllByProps({className:"metric"})).toHaveLength(2);expect(f.tree.root.findAllByProps({className:"metric"})[0]).toBe(first);
  const reference=f.tree.root.findByProps({className:"view-instance-reference"});
  expect(reference.findAllByType("span").flatMap(span=>span.children).join("")).toContain("4 members");
  const go=reference.findByType("button");act(()=>go.props.onClick());
  expect(f.scrollIntoView).toHaveBeenCalledWith({block:"center",inline:"nearest"});expect(f.focus).toHaveBeenCalledWith({preventScroll:true});
  expect(f.watchViewFrame).toHaveBeenCalledOnce();expect(f.records[0]!.close).not.toHaveBeenCalled();
  expect(f.applyViewQuery).not.toHaveBeenCalled();expect(f.viewObservation).not.toHaveBeenCalled();
  f.send(frame(0));expect(anchors()[0]!.props.style).toBeUndefined();
  act(()=>f.tree.unmount());expect(f.records[0]!.close).toHaveBeenCalledOnce();
});
it("keeps connect results compact until explicit opening, without letting a later creator steal that anchor",()=>{
  const f=fixture();f.mount(true);expect(f.watchViewFrame).not.toHaveBeenCalled();
  expect(f.tree.root.findAllByProps({className:"view-instance-anchor"})).toHaveLength(0);
  expect(f.tree.root.findAllByProps({className:"view-instance-reference"}).every(row=>row.props.style===undefined)).toBe(true);
  act(()=>f.tree.root.findAllByType("button")[1]!.props.onClick());expect(f.watchViewFrame).toHaveBeenCalledOnce();
  act(()=>f.tree.update(f.render()));expect(f.watchViewFrame).toHaveBeenCalledOnce();
  expect(f.tree.root.findAllByProps({className:"view-instance-anchor"})).toHaveLength(1);act(()=>f.tree.unmount());
});
it("reserves a previously rendered allocation when its slot becomes a reference",()=>{
  const f=fixture();f.mount();act(()=>f.tree.update(f.render(true)));expect(f.records[0]!.close).toHaveBeenCalledOnce();
  const references=f.tree.root.findAllByProps({className:"view-instance-reference"});expect(references[0]!.props.style).toEqual({minHeight:410});expect(references[1]!.props.style).toBeUndefined();act(()=>f.tree.unmount());
});
it("discards prior workspace frames and recreates only one anchor after a generation change",()=>{
  const f=fixture();f.mount();const old=f.records[0]!;f.generation("second");expect(old.close).toHaveBeenCalledOnce();expect(f.watchViewFrame).toHaveBeenCalledTimes(2);
  act(()=>old.listener({frame:frame(9)}));expect(f.tree.root.findAllByProps({className:"metric"})).toHaveLength(1);act(()=>f.tree.unmount());
});
it("uses handle identity and generation rather than incidental metadata to share placement",()=>{
  const a=value();expect(displayIdentity(a,"first")).toBe(displayIdentity({...a,data:{definition:"Other",instance:"board-instance",id:"board"}},"first"));
  expect(displayIdentity(a,"first")).not.toBe(displayIdentity(value("other-instance"),"first"));expect(displayIdentity(a,"first")).not.toBe(displayIdentity(a,"second"));
});
it("keeps identical handles in separate workspace engines independently open",()=>{
  const a=fixture(),b=fixture();a.mount();b.mount();
  expect(a.tree.root.findAllByProps({className:"view-instance-anchor"})).toHaveLength(1);
  expect(b.tree.root.findAllByProps({className:"view-instance-anchor"})).toHaveLength(1);
  expect(a.watchViewFrame).toHaveBeenCalledOnce();expect(b.watchViewFrame).toHaveBeenCalledOnce();
  act(()=>a.tree.unmount());expect(b.records[0]!.close).not.toHaveBeenCalled();act(()=>b.tree.unmount());
});
it("keeps owner summaries authoritative and releases the shared registry after its final participant",()=>{
  const store=new ViewDisplays(),a=Symbol(),b=Symbol(),first={eligible:true,label:"$overview",target:()=>null,changed:vi.fn()},second={...first,changed:vi.fn()};
  const leaveA=store.join("view",a,first),leaveB=store.join("view",b,second);expect(second.changed.mock.calls.at(-1)?.[0].owner).toBe(a);
  store.describe("view",b,{members:9});expect(second.changed.mock.calls.at(-1)?.[0].members).toBeUndefined();store.describe("view",a,{members:4});
  expect(second.changed.mock.calls.at(-1)?.[0].members).toBe(4);leaveA();expect(second.changed.mock.calls.at(-1)?.[0].owner).toBe(b);leaveB();
  const next={...first,changed:vi.fn()},leave=store.join("view",Symbol(),next);expect(next.changed.mock.calls.at(-1)?.[0].members).toBeUndefined();leave();
});
it("labels opening and going to an existing view as placement that runs nothing, and keeps it that way",()=>{
  const f=fixture();f.mount(true);
  const button=(name:string)=>f.tree.root.findAllByType("button").filter(node=>node.children[0]===name);
  const [open]=button("Open view");
  expect(open!.props["aria-description"]).toMatch(/existing view.*runs nothing/);
  act(()=>open!.props.onClick());
  expect(f.watchViewFrame).toHaveBeenCalledOnce();
  const [go]=button("Go to view");
  expect(go!.props["aria-description"]).toMatch(/already shown.*Runs nothing/);
  act(()=>go!.props.onClick());act(()=>go!.props.onClick());
  expect(f.scrollIntoView).toHaveBeenCalledTimes(2);expect(f.watchViewFrame).toHaveBeenCalledOnce();
  for(const call of Object.values(f.work))expect(call).not.toHaveBeenCalled();
  act(()=>f.tree.unmount());
});
