import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, expect, it, vi } from "vitest";
import { InstanceView } from "./InstanceView";
import { localPackage } from "./package.test-support";
import metric from "../../../views/metric/View";
import { valueViewModules } from "./registry";
import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";
import type { FrameSample } from "./instances";

afterEach(()=>{vi.unstubAllGlobals();vi.restoreAllMocks();vi.useRealTimers();});

function fixture(count=20){
  vi.useFakeTimers();
  const visibility=new Set<()=>void>();
  const owner={hidden:false,addEventListener:(_event:string,listener:()=>void)=>visibility.add(listener),removeEventListener:(_event:string,listener:()=>void)=>visibility.delete(listener)};
  const intersection=vi.fn();
  vi.stubGlobal("IntersectionObserver",intersection);
  let generation="first",active=0;
  const records:{node:string;generation:string;listener:(sample:FrameSample)=>void;close:ReturnType<typeof vi.fn>}[]=[];
  const watchViewFrame=vi.fn((node:string,_identity:string,listener:(sample:FrameSample)=>void)=>{
    active++;
    const close=vi.fn(()=>{active--;});
    records.push({node,generation,listener,close});
    return close;
  });
  const applyViewQuery=vi.fn(),viewObservation=vi.fn();
  const engine={viewGeneration:()=>generation,watchViewFrame,applyViewQuery,viewObservation} as unknown as Engine;
  const values:StoredValue[]=Array.from({length:count},(_,n)=>({type:{kind:"meta",name:"ViewInstance"},data:{id:`n${n}`,instance:`identity${n}`},provenance:{}}));
  const render=()=> <>{values.map((value,n)=><InstanceView key={n} value={value} engine={engine} mode="window"/>)}</>;
  let tree!:ReactTestRenderer;
  act(()=>{tree=create(render(),{createNodeMock:()=>({ownerDocument:owner})});});
  return {tree,watchViewFrame,records,applyViewQuery,viewObservation,intersection,
    get active(){return active;},
    hide:(hidden:boolean)=>act(()=>{owner.hidden=hidden;visibility.forEach(listener=>listener());}),
    rerender:()=>act(()=>{tree.update(render());}),
    generation:(next:string)=>act(()=>{generation=next;tree.update(render());}),
  };
}

it("keeps all mounted workspace views resident without viewport or hidden-document disposal",()=>{
  const f=fixture();
  expect(f.active).toBe(20);expect(f.watchViewFrame).toHaveBeenCalledTimes(20);
  expect(f.intersection).not.toHaveBeenCalled();
  localPackage("metric",metric);
  const definition=valueViewModules.named("metric")!.definition!;
  act(()=>f.records.forEach((record,n)=>record.listener({frame:{root:record.node,instances:[{
    id:record.node,instance:`identity${n}`,revision:"0",definition:definition.id,digest:definition.digest,artifact:definition.artifact,
    inputReference:{kind:"unlinked"},inputDelivery:"finite",observing:false,inputRevision:"0",inputProblem:null,inputCautions:[],linkedInputs:[],query:null,members:{},
    input:{type:{kind:"record",name:"Metric",fields:[{name:"view",type:{kind:"primitive",name:"TEXT"}},{name:"value",type:{kind:"primitive",name:"INT"}}]},data:{view:"metric",value:80},provenance:{}},
  }]}})));
  const drawings=f.tree.root.findAllByProps({className:"metric"});expect(drawings).toHaveLength(20);
  f.hide(true);act(()=>{vi.advanceTimersByTime(5000);});f.hide(false);f.rerender();
  expect(f.active).toBe(20);expect(f.watchViewFrame).toHaveBeenCalledTimes(20);
  expect(f.tree.root.findAllByProps({className:"metric"})).toEqual(drawings);
  expect(f.records.every(record=>record.close.mock.calls.length===0)).toBe(true);
  expect(JSON.stringify(f.tree.toJSON())).not.toContain("outside the visible workspace");
  expect(f.applyViewQuery).not.toHaveBeenCalled();expect(f.viewObservation).not.toHaveBeenCalled();
  act(()=>f.tree.unmount());expect(f.active).toBe(0);
  expect(f.records.every(record=>record.close.mock.calls.length===1)).toBe(true);
});

it("releases resident views when workspace generation changes and ignores late prior frames",()=>{
  const f=fixture(1),old=f.records[0]!;
  f.generation("second");expect(f.active).toBe(1);
  expect(old.close).toHaveBeenCalledOnce();expect(f.records[1]!.generation).toBe("second");
  act(()=>old.listener({problem:"late response"}));
  expect(JSON.stringify(f.tree.toJSON())).not.toContain("late response");
  expect(f.applyViewQuery).not.toHaveBeenCalled();expect(f.viewObservation).not.toHaveBeenCalled();
  act(()=>f.tree.unmount());expect(f.active).toBe(0);
});

it("reopens a failed display only on explicit action, without applying a query or starting observation",()=>{
  const f=fixture(1);
  act(()=>f.records[0]!.listener({problem:"View display session expired or closed"}));
  f.hide(true);f.hide(false);f.rerender();
  const reopen=f.tree.root.findAllByType("button").find(button=>button.children[0]==="Reopen view")!;
  expect(f.watchViewFrame).toHaveBeenCalledOnce();
  act(()=>reopen.props.onClick());
  expect(f.records[0]!.close).toHaveBeenCalledOnce();expect(f.watchViewFrame).toHaveBeenCalledTimes(2);
  expect(f.active).toBe(1);
  expect(f.applyViewQuery).not.toHaveBeenCalled();expect(f.viewObservation).not.toHaveBeenCalled();
  expect(JSON.stringify(f.tree.toJSON())).not.toContain("expired or closed");
  act(()=>f.tree.unmount());
});
