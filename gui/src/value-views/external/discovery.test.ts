import {afterEach,expect,it,vi} from "vitest";
import {Engine} from "../../engine";
import type {StoredValue} from "../../protocol";
import type {ViewAsset} from "./document";
import {ValuePackages} from "./discovery";
import {loadPackages} from "./load";
import {builtinValueViews,valueViewModules,bindValueViews} from "../registry";
import {prepareSync} from "../../presentation/prepare";
import {present} from "../../presentation/present";
import {Registry} from "../../presentation/registry";
import {viewsFor} from "../../surface/result-views";

vi.mock("../../workspace-events",()=>({WorkspaceEvents:function(path:string){return new EventSource(path);}}));
class Events {
  static current:Events;
  onmessage?:(message:{data:string})=>void;
  constructor(){Events.current=this;}
  close(){}
  emit(event:object){this.onmessage?.({data:JSON.stringify(event)});}
}
const disposers:(()=>void)[]=[];
afterEach(()=>{disposers.splice(0).forEach(fn=>fn());vi.restoreAllMocks();vi.unstubAllGlobals();});
let sequence=0;
function fixture(){
  const base=valueViewModules.named("metric")!.definition!,artifact=(++sequence).toString(16).padStart(64,"0");
  const id=`synthetic-badge-${sequence}`,definition={...base,id,name:`SyntheticBadge${sequence}`,artifact,
    contracts:{...base.contracts,MetricKind:{...base.contracts.MetricKind!,constraints:{...base.contracts.MetricKind!.constraints,enum:[id]}}}};
  const asset:ViewAsset={digest:artifact,definition,javascript:"throw new Error('only executed in isolated renderer');",css:""};
  const value:StoredValue={type:{kind:"record",name:"SyntheticReading",fields:[{name:"view",type:{kind:"primitive",name:"TEXT"}},{name:"value",type:{kind:"primitive",name:"INT"}}]},data:{view:id,value:7},provenance:{}};
  const register=valueViewModules.register;
  vi.spyOn(valueViewModules,"register").mockImplementation(module=>{const off=register(module);disposers.push(off);return off;});
  return {asset,value,definition};
}
function connect(binding="synthetic",generation="g"){
  vi.stubGlobal("EventSource",Events);
  vi.stubGlobal("window",{fetch:globalThis.fetch});
  const engine=new Engine(binding),off=engine.listen(()=>{},()=>{}),events=Events.current;
  disposers.push(off);events.emit({event:"session",workspace:null,generation});return {engine,events};
}
function draw(value:StoredValue){return present({prepared:prepareSync(value),registry:Registry.core(),context:{mode:"window",columns:80,lines:480,density:"normal",locale:"en-GB",timeZone:"UTC"}});}

it("renders a fresh ordinary result through discovery without creating or opening a managed View",async()=>{
  const {asset,value,definition}=fixture();expect(draw(value).root.kind).not.toBe("view");
  const fetch=vi.fn(async(url:string)=>new Response(JSON.stringify(url.startsWith("/values/")?value:url==="/language/views"?[definition]:asset)));
  vi.stubGlobal("fetch",fetch);const {engine}=connect("synthetic workspace");
  const observed=await engine.fetch("saved-reading");
  expect(draw(observed).root).toMatchObject({kind:"view",view:`${definition.id}--${asset.digest}`});
  expect(viewsFor({value:observed}).map(v=>v.name)).toContain(definition.id);
  expect(fetch.mock.calls.map(([url])=>url)).toEqual(["/values/saved-reading","/language/views",`/view-packages/${asset.digest}`]);
  for(const call of fetch.mock.calls){const init=(call as unknown as [string,RequestInit])[1];expect(init.headers).toMatchObject({"X-Wes-Workspace":"synthetic%20workspace","X-Wes-Session":"g"});}
});

it("shares catalogue and immutable asset reads across concurrent and repeated values",async()=>{
  const {asset,value,definition}=fixture();
  const fetch=vi.fn(async(url:string)=>new Response(JSON.stringify(url.startsWith("/values/")?value:url==="/language/views"?[definition]:asset)));
  vi.stubGlobal("fetch",fetch);const {engine}=connect();
  const values=await Promise.all([engine.fetch("one"),engine.fetch("two"),engine.fetch("one")]);
  await engine.fetch("three");
  expect(values.every(v=>draw(v).root.kind==="view")).toBe(true);
  expect(fetch.mock.calls.filter(([url])=>url==="/language/views")).toHaveLength(1);
  expect(fetch.mock.calls.filter(([url])=>url.startsWith("/view-packages/"))).toHaveLength(1);
});

it("keeps cached code from selecting a renderer in a workspace that has no such package",async()=>{
  const {asset,value,definition}=fixture();
  const fetch=vi.fn(async(url:string,init:RequestInit)=>new Response(JSON.stringify(url.startsWith("/values/")?value:url==="/language/views"?( (init.headers as Record<string,string>)["X-Wes-Workspace"]==="installed"?[definition]:[]):asset)));
  vi.stubGlobal("fetch",fetch);
  const installed=connect("installed"),absent=connect("absent");
  expect(draw(await installed.engine.fetch("reading")).root.kind).toBe("view");
  const observed=await absent.engine.fetch("reading");
  expect(draw(observed).root.kind).not.toBe("view");expect(viewsFor({value:observed}).map(v=>v.name)).not.toContain(definition.id);
});

it("refreshes package discovery after planning and chooses only the installed artifact",async()=>{
  const {asset,value,definition}=fixture(),next={...asset,digest:"a".repeat(64),definition:{...definition,artifact:"a".repeat(64)}};
  let catalogue:unknown[]=[];
  const fetch=vi.fn(async(url:string)=>new Response(JSON.stringify(url.startsWith("/values/")?value:url==="/language/views"?catalogue:url.endsWith(next.digest)?next:asset)));
  vi.stubGlobal("fetch",fetch);const {engine,events}=connect();
  expect(draw(await engine.fetch("before")).root.kind).not.toBe("view");
  catalogue=[definition];events.emit({event:"planned"});expect(draw(await engine.fetch("installed")).root).toMatchObject({kind:"view",view:`${definition.id}--${asset.digest}`});
  catalogue=[next.definition];events.emit({event:"ready"});expect(draw(await engine.fetch("replaced")).root).toMatchObject({kind:"view",view:`${definition.id}--${next.digest}`});
});

it.each(["catalogue","asset","identity"])("keeps stored data readable on %s failure and never submits source",async failure=>{
  const {asset,value,definition}=fixture();
  const fetch=vi.fn(async(url:string)=>{
    if(url==="/language/views"&&failure==="catalogue"||url.startsWith("/view-packages/")&&failure==="asset")return new Response("unavailable",{status:503});
    return new Response(JSON.stringify(url.startsWith("/values/")?value:url==="/language/views"?[definition]:{...asset,digest:failure==="identity"?"wrong":asset.digest}));
  });
  vi.stubGlobal("fetch",fetch);const {engine}=connect();const observed=await engine.fetch("reading");
  expect(observed).toEqual(value);expect(draw(observed).root.kind).not.toBe("view");
  expect(fetch.mock.calls.some(([url])=>url==="/submit")).toBe(false);
});

it("rejects a result when the workspace changes during asset discovery",async()=>{
  const {asset,value,definition}=fixture();let finish!:(response:Response)=>void;
  const fetch=vi.fn(async(url:string)=>url.startsWith("/view-packages/")?await new Promise<Response>(resolve=>{finish=resolve;}):new Response(JSON.stringify(url.startsWith("/values/")?value:[definition])));
  vi.stubGlobal("fetch",fetch);const {engine,events}=connect();const reading=engine.fetch("reading");
  await vi.waitFor(()=>expect(finish).toBeTypeOf("function"));
  events.emit({event:"session",workspace:null,generation:"new"});finish(new Response(JSON.stringify(asset)));
  await expect(reading).rejects.toThrow("Workspace changed");expect(valueViewModules.named(definition.id,asset.digest)).toBeUndefined();
});

it("discovers nested marked records but does not fetch assets for malformed markers or ordinary text",async()=>{
  const {asset,value,definition}=fixture(),catalogue=vi.fn(async()=>[definition]),read=vi.fn(async()=>asset);
  const packages=new ValuePackages(catalogue,read);
  expect(await packages.modules({...value,data:"plain text"})).toBe(builtinValueViews);expect(catalogue).not.toHaveBeenCalled();
  expect(await packages.modules({...value,data:{view:definition.id,value:"invalid"}})).toEqual(builtinValueViews);expect(read).not.toHaveBeenCalled();
  const nested:StoredValue={type:{kind:"option",element:value.type},data:{kind:"some",value:value.data},provenance:{}};
  bindValueViews(nested,await packages.modules(nested));expect(draw(nested).offers).toContain(definition.id);expect(read).toHaveBeenCalledOnce();
});

it("verifies contract identity even when an artifact is already cached",async()=>{
  const {asset,definition}=fixture();await loadPackages([{definition:definition.id,digest:definition.digest,artifact:asset.digest}],async()=>asset);
  await expect(loadPackages([{definition:definition.id,digest:"b".repeat(64),artifact:asset.digest}],async()=>asset)).rejects.toThrow("mismatched");
});
