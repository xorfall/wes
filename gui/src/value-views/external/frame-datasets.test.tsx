import {act,create,type ReactTestRenderer} from "react-test-renderer";
import {afterEach,expect,it,vi} from "vitest";
import {startFrame} from "../../../../packages/view-sdk/frame";
import {ViewDatasetError,type ViewDatasets} from "../../../../packages/view-sdk/datasets";

/* Synthetic frame: an invented static View reading an invented Dataset field by pointer only. */

const host=vi.hoisted(()=>({render:vi.fn()}));
vi.mock("react-dom/client",()=>({createRoot:()=>({render:host.render})}));
afterEach(()=>{vi.unstubAllGlobals();vi.useRealTimers();host.render.mockReset();});

function frame(){
  let connect:(event:unknown)=>void=()=>{};
  const parent={},rootElement={scrollHeight:100,dataset:{}};
  vi.stubGlobal("parent",parent);
  vi.stubGlobal("document",{getElementById:()=>rootElement,querySelectorAll:()=>[],addEventListener:()=>{},fonts:{ready:Promise.resolve()},
    documentElement:{clientWidth:800},createElement:()=>({getContext:()=>({font:"",measureText:()=>({width:80})})})});
  vi.stubGlobal("getComputedStyle",()=>({getPropertyValue:()=>"",lineHeight:"20px"}));
  vi.stubGlobal("window",{innerHeight:400,addEventListener:(name:string,callback:typeof connect)=>{if(name==="message")connect=callback;},removeEventListener:()=>{},scrollBy:()=>{}});
  vi.stubGlobal("ResizeObserver",class{observe(){} disconnect(){}});
  const port={onmessage:undefined as undefined|((message:{data:string})=>void),postMessage:vi.fn(),start:vi.fn()};
  let renderer:ReactTestRenderer|undefined,seen:ViewDatasets|undefined;
  host.render.mockImplementation(element=>act(()=>{if(renderer)renderer.update(element);else renderer=create(element);}));
  const Component=({context}:{context:{datasets?:ViewDatasets}})=>{seen=context.datasets;return <p>view</p>;};
  startFrame({definition:{interaction:null,digest:"fixture"},Component} as unknown as Parameters<typeof startFrame>[0]);
  connect({source:parent,data:"wes-view-connect",ports:[port]});
  const receive=(message:unknown)=>port.onmessage!({data:JSON.stringify(message)});
  const sent=()=>port.postMessage.mock.calls.map(([text])=>JSON.parse(text as string)).filter(message=>message.kind==="dataset-read");
  return {receive,sent,datasets:()=>seen,port};
}
const render=(epoch:number,datasets=true,input:unknown={title:"synthetic"})=>({kind:"render",input,sequence:epoch+1,context:{mode:"window",instance:"leaf",datasets,datasetEpoch:epoch}});

it("offers datasets only when the host can read them, and says so explicitly otherwise",()=>{
  const {receive,datasets}=frame();
  receive(render(0,false));
  expect(datasets()).toBeUndefined();
  receive(render(0,true));
  expect(datasets()).toBeDefined();
});

it("sends a relative pointer and position only, and settles from the host's reply",async()=>{
  const {receive,sent,datasets}=frame();
  receive(render(0));
  const reading=datasets()!.page("/events",{from:"9007199254740993"},20);
  expect(sent()).toEqual([{kind:"dataset-read",request:1,operation:"page",select:"/events",position:{from:"9007199254740993"},limit:20}]);
  receive({kind:"dataset-reply",request:1,ok:true,result:{generation:"3",records:"9007199254740994",lifecycle:"open",first:"9007199254740993",next:"9007199254740994",
    extentExhausted:true,cursor:null,rows:[{ordinal:"9007199254740993",sourceStart:"0",sourceEnd:"1",value:{label:"last"}}]}});
  const page=await reading;
  expect(page.rows[0]!.ordinal).toBe("9007199254740993");
  expect(Object.isFrozen(page.rows[0])).toBe(true);
  // A late or unknown reply settles nothing.
  receive({kind:"dataset-reply",request:1,ok:true,result:{}});
  receive({kind:"dataset-reply",request:99,ok:false,error:"withdrawn"});
});

it("refuses invalid requests before they leave and keeps at most two in flight",async()=>{
  const {receive,sent,datasets}=frame();
  receive(render(0));
  await expect(datasets()!.page("https://elsewhere/datasets")).rejects.toMatchObject({code:"invalid"});
  await expect(datasets()!.page("/events",{from:"01"})).rejects.toMatchObject({code:"invalid"});
  await expect(datasets()!.page("/events",undefined,500)).rejects.toMatchObject({code:"invalid"});
  void datasets()!.page("/events").catch(()=>undefined);void datasets()!.inspect("/events").catch(()=>undefined);
  await expect(datasets()!.page("/events")).rejects.toMatchObject({code:"busy"});
  expect(sent()).toHaveLength(2);
});

it("rejects reads in flight as changed when the input binding changes, with coarse typed errors only",async()=>{
  const {receive,datasets}=frame();
  receive(render(0));
  const first=datasets()!.page("/events"),second=datasets()!.inspect("/events");
  receive(render(1));
  await expect(first).rejects.toBeInstanceOf(ViewDatasetError);
  await expect(second).rejects.toMatchObject({code:"changed"});
  const third=datasets()!.page("/events");
  receive({kind:"dataset-reply",request:3,ok:false,error:"withdrawn",detail:"/private/path"});
  const error=await third.then(()=>undefined,(caught:unknown)=>caught as ViewDatasetError) as ViewDatasetError;
  expect(error.code).toBe("withdrawn");
  expect(error.message).not.toContain("private");
});
