import {afterEach,expect,it,vi} from "vitest";
import {act,create} from "react-test-renderer";
import type {ContextType} from "react";
import {externalModule} from "./module";
import {InstanceInteractionHost} from "../interactive";
import {valueViewModules} from "../registry";
import {decodeContract} from "../definition";
import {stringifyExactJson} from "../../exact-json";
import {timelineValue} from "../../../../tools/view-dev/work/timeline";
import {initial,outputs} from "../../../../views/timeline/navigation";
import type {Input} from "../../../../views/timeline/contract";
import {RenderObservationContext,RenderStatusRegistry,type RenderObservation,type RenderScope} from "../../view-render-status";

vi.mock("react-dom",async importOriginal=>({...await importOriginal<typeof import("react-dom")>(),createPortal:(children:unknown)=>children}));
vi.mock("./theme",()=>({currentTheme:()=>"",followTheme:()=>()=>{}}));
vi.mock("./document",()=>({frameDocument:async()=>"<!doctype html><body></body>"}));
afterEach(()=>{vi.unstubAllGlobals();vi.useRealTimers();});
type Host=NonNullable<ContextType<typeof InstanceInteractionHost>>;
type Channel={port1:{onmessage?:(event:{data:string})=>void;postMessage:ReturnType<typeof vi.fn>;close:ReturnType<typeof vi.fn>;start:ReturnType<typeof vi.fn>};port2:{close:ReturnType<typeof vi.fn>}};
const scope:RenderScope={workspace:"research",generation:"g1",node:"chart",instance:"chart-instance"};
const RECEIPT_FIELDS=["ackSequence","digest","drawnInputRevision","error","host","mode","requestedInputRevision","sentSequence","status"];

async function harness(){
  vi.stubGlobal("ResizeObserver",class{observe(){} disconnect(){}});
  vi.stubGlobal("getComputedStyle",()=>({lineHeight:"21px"}));
  vi.useFakeTimers();
  const channels:Channel[]=[];
  vi.stubGlobal("MessageChannel",class{port1={onmessage:undefined,postMessage:vi.fn(),close:vi.fn(),start:vi.fn()};port2={close:vi.fn()};constructor(){channels.push(this as unknown as Channel);}});
  const definition=valueViewModules.named("timeline")!.definition!;
  const module=externalModule({definition,digest:definition.artifact!,javascript:"void 0",css:""});
  const value=timelineValue(),input=decodeContract(definition,definition.input,value.data,true,value.type) as Input;
  const model=(inputRevision:string,identity:string|null="chart-instance")=>({input,path:"view/chart",identity:identity??undefined,inputRevision,slots:{},mode:"preview",coordinated:false});
  const registry=new RenderStatusRegistry();
  const observation=(generation:string):RenderObservation=>({registry,scope:(path,instance)=>path==="view/chart"&&instance==="chart-instance"?{...scope,generation}:undefined});
  const host=vi.fn<Host>(()=>()=>{}),iframes:{srcdoc:string;contentWindow:{postMessage:ReturnType<typeof vi.fn>}}[]=[];
  const view=(models:readonly object[],observed:RenderObservation|undefined)=><InstanceInteractionHost.Provider value={host}><RenderObservationContext.Provider value={observed}>
    {models.map((m,index)=><module.Component key={index} model={m} children={[]} renderChild={()=>null}/>)}
  </RenderObservationContext.Provider></InstanceInteractionHost.Provider>;
  let tree!:ReturnType<typeof create>;
  const mount=async(models:readonly object[],observed:RenderObservation|undefined)=>{await act(async()=>{tree=create(view(models,observed),{createNodeMock:node=>{
    if(node.type==="iframe"){const frame={srcdoc:"",contentWindow:{postMessage:vi.fn()}};iframes.push(frame);return frame;}return {};
  }});});return tree;};
  const load=(index:number)=>act(()=>tree.root.findAllByType("iframe")[index]!.props.onLoad());
  const receive=(index:number,message:unknown)=>act(()=>channels[index]!.port1.onmessage!({data:stringifyExactJson(message)}));
  const ready=(index:number)=>{const state=initial(input);receive(index,{kind:"ready",digest:definition.digest,state,outputs:outputs(state)});};
  const advance=(ms:number)=>act(()=>{vi.advanceTimersByTime(ms);});
  return {definition,model,registry,observation,view,mount,load,receive,ready,advance,channels,iframes,host,tree:()=>tree};
}

it("should_TrackEachMountWithExactFlightRevisions_When_InputChangesDuringFlightAndAcksArriveLate",async()=>{
  const h=await harness(),first=h.model("r1"),second=h.model("r1");
  await h.mount([first,second],h.observation("g1"));
  const hosts=h.registry.receipts(scope);
  expect(hosts).toHaveLength(2);expect(new Set(hosts.map(r=>r.host)).size).toBe(2);
  expect(hosts.every(r=>r.status==="loading"&&r.sentSequence===0&&r.digest===h.definition.digest&&r.mode==="preview")).toBe(true);
  h.load(0);
  expect(h.registry.receipts(scope)[0]).toMatchObject({sentSequence:0,requestedInputRevision:null}); // scheduled, not yet sent
  h.advance(100);
  expect(h.registry.receipts(scope)[0]).toMatchObject({status:"loading",sentSequence:1,requestedInputRevision:"r1",ackSequence:0,drawnInputRevision:null});
  const render=h.channels[0]!.port1.postMessage.mock.calls.map(([text])=>JSON.parse(text)).find(message=>message.kind==="render");
  expect(render).toBeDefined();expect(Object.keys(render.context)).not.toContain("inputRevision"); // the revision label is host-side only
  expect(Object.keys(render)).not.toContain("inputRevision");
  h.ready(0);expect(h.registry.receipts(scope)[0]!.status).toBe("ready");
  h.receive(0,{kind:"ack",sequence:5});
  expect(h.registry.receipts(scope)[0]).toMatchObject({status:"ready",ackSequence:0,drawnInputRevision:null});
  act(()=>h.tree().update(h.view([h.model("r2"),second],h.observation("g1"))));h.advance(500);
  expect(h.registry.receipts(scope)[0]).toMatchObject({sentSequence:1,requestedInputRevision:"r1"}); // r2 waits for the flight
  h.receive(0,{kind:"ack",sequence:1});
  expect(h.registry.receipts(scope)[0]).toMatchObject({status:"drawn",ackSequence:1,drawnInputRevision:"r1",requestedInputRevision:"r1"});
  h.advance(100);
  expect(h.registry.receipts(scope)[0]).toMatchObject({sentSequence:2,requestedInputRevision:"r2",ackSequence:1,drawnInputRevision:"r1"});
  h.receive(0,{kind:"ack",sequence:1});
  expect(h.registry.receipts(scope)[0]).toMatchObject({ackSequence:1,drawnInputRevision:"r1"}); // late ack is not counted
  h.receive(0,{kind:"ack",sequence:2});
  expect(h.registry.receipts(scope)[0]).toMatchObject({status:"drawn",ackSequence:2,drawnInputRevision:"r2",error:null});
  expect(h.registry.receipts(scope)[1]).toMatchObject({status:"loading",sentSequence:0,ackSequence:0}); // the other mount is untouched
  act(()=>h.tree().unmount());
  expect(h.registry.receipts(scope)).toEqual([]);expect(h.registry.scopeCount).toBe(0);
});

it("should_HaveNoSideEffects_When_ReceiptsAreRead",async()=>{
  const h=await harness();
  await h.mount([h.model("r1")],h.observation("g1"));
  h.load(0);h.advance(100);h.ready(0);
  const posted=h.channels[0]!.port1.postMessage.mock.calls.length,connects=h.iframes[0]!.contentWindow.postMessage.mock.calls.length,bindings=h.host.mock.calls.length,channels=h.channels.length;
  for(let i=0;i<10;i++){h.registry.receipts(scope);h.registry.receipts({...scope,node:"absent"});}
  h.advance(1000);
  expect(h.channels[0]!.port1.postMessage.mock.calls.length).toBe(posted);
  expect(h.iframes[0]!.contentWindow.postMessage.mock.calls.length).toBe(connects);
  expect(h.host.mock.calls.length).toBe(bindings);expect(h.channels.length).toBe(channels);
  expect(h.registry.scopeCount).toBe(1);
  act(()=>h.tree().unmount());
});

it("should_ReportOnlyEnumFailure_When_RendererFailsWithPrivateDetails",async()=>{
  const h=await harness();
  await h.mount([h.model("r1"),h.model("r1")],h.observation("g1"));
  h.load(0);h.advance(100);
  h.receive(0,{kind:"error",message:"secret-token in https://internal.example"});
  h.load(1);h.receive(1,{kind:"unknown",payload:"another-secret"});
  const [failed,rejected]=h.registry.receipts(scope);
  expect(failed).toMatchObject({status:"failed",error:"renderer_failed",sentSequence:1,requestedInputRevision:"r1"});
  expect(rejected).toMatchObject({status:"failed",error:"communication_rejected"});
  for(const receipt of [failed!,rejected!])expect(Object.keys(receipt).sort()).toEqual(RECEIPT_FIELDS);
  const text=JSON.stringify(h.registry.receipts(scope));
  for(const secret of ["secret-token","internal.example","another-secret","View renderer failed","communication rejected"])expect(text).not.toContain(secret);
  h.receive(0,{kind:"ack",sequence:1});
  expect(h.registry.receipts(scope)[0]).toMatchObject({status:"failed",ackSequence:0});
  act(()=>h.tree().unmount());
});

it("should_AnswerDatasetReadWithoutFailingReceipt_When_DrawingHasNoDatasetSource",async()=>{
  // Arrange
  const h=await harness();
  await h.mount([h.model("r1")],h.observation("g1"));
  h.load(0);h.advance(100);h.ready(0);h.receive(0,{kind:"ack",sequence:1});
  // Act
  h.receive(0,{kind:"dataset-read",request:1,operation:"page",select:"/synthetic",position:{from:"0"},limit:20});
  // Assert
  const replies=h.channels[0]!.port1.postMessage.mock.calls.map(([text])=>JSON.parse(text)).filter(message=>message.kind==="dataset-reply");
  expect(replies).toEqual([{kind:"dataset-reply",request:1,ok:false,error:"unavailable"}]);
  expect(h.registry.receipts(scope)[0]).toMatchObject({status:"drawn",error:null,drawnInputRevision:"r1"});
  act(()=>h.tree().unmount());
});

it("should_ReportDrawTimeout_When_FlightIsNeverAcknowledged",async()=>{
  const h=await harness();
  await h.mount([h.model("r1")],h.observation("g1"));
  h.load(0);h.advance(100);h.ready(0);h.advance(5100);
  expect(h.registry.receipts(scope)[0]).toMatchObject({status:"failed",error:"draw_timeout",sentSequence:1,ackSequence:0});
  act(()=>h.tree().unmount());
});

it("should_MoveEntriesToNewScope_When_GenerationChanges",async()=>{
  const h=await harness(),models=[h.model("r1")];
  await h.mount(models,h.observation("g1"));
  const [before]=h.registry.receipts(scope);
  act(()=>h.tree().update(h.view(models,h.observation("g2"))));
  expect(h.registry.receipts(scope)).toEqual([]);
  expect(h.registry.receipts({...scope,generation:"g2"}).map(r=>r.host)).toEqual([before!.host]);
  act(()=>h.tree().unmount());expect(h.registry.scopeCount).toBe(0);
});

it("should_LeaveCanvasUnobserved_When_NoLiveInstanceScopeExists",async()=>{
  const h=await harness();
  await h.mount([h.model("r1",null)],h.observation("g1"));
  h.load(0);h.advance(100);
  expect(h.registry.scopeCount).toBe(0);
  act(()=>h.tree().unmount());
  await h.mount([h.model("r1")],undefined);
  expect(h.registry.scopeCount).toBe(0);
  act(()=>h.tree().unmount());
});
