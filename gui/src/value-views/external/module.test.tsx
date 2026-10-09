import {afterEach,expect,it,vi} from "vitest";
import {act,create} from "react-test-renderer";
import type {ContextType} from "react";
import {externalModule} from "./module";
import {ViewBindingContext,type ViewBinding} from "../view-bindings";
import {ComposeContext} from "../../surface/dataset-management";
import {InstanceInteractionHost} from "../interactive";
import {valueViewModules} from "../registry";
import {decodeContract} from "../definition";
import {stringifyExactJson} from "../../exact-json";
import {timelineValue} from "../../../../tools/view-dev/work/timeline";
import {initial,outputs} from "../../../../views/timeline/navigation";
import type {Input} from "../../../../views/timeline/contract";

vi.mock("react-dom",async importOriginal=>({...await importOriginal<typeof import("react-dom")>(),createPortal:(children:unknown)=>children}));
vi.mock("./theme",()=>({currentTheme:()=>"",followTheme:()=>()=>{}}));
vi.mock("./document",()=>({frameDocument:async()=>"<!doctype html><body></body>"}));
afterEach(()=>{vi.unstubAllGlobals();vi.useRealTimers();});
type Host=NonNullable<ContextType<typeof InstanceInteractionHost>>;

async function mounted(member=false,commandSupport=false){
  vi.stubGlobal("ResizeObserver",class{observe(){} disconnect(){}});
  vi.stubGlobal("getComputedStyle",()=>({lineHeight:"21px"}));
  vi.useFakeTimers();
  const channels:{port1:{onmessage?: (event:{data:string})=>void;postMessage:ReturnType<typeof vi.fn>;close:ReturnType<typeof vi.fn>;start:ReturnType<typeof vi.fn>};port2:{close:ReturnType<typeof vi.fn>}}[]=[];
  vi.stubGlobal("MessageChannel",class{
    port1={onmessage:undefined,postMessage:vi.fn(),close:vi.fn(),start:vi.fn()};port2={close:vi.fn()};
    constructor(){channels.push(this);}
  });
  const definition=valueViewModules.named("timeline")!.definition!;
  const module=externalModule({definition,digest:definition.artifact!,javascript:"void 0",css:""});
  const value=timelineValue(),input=decodeContract(definition,definition.input,value.data,true,value.type) as Input;
  const model={input,path:"view/chart",identity:"chart-instance",slots:member?{members:[{kind:"leaf",path:"child"}]}:{},mode:"preview",coordinated:false};
  const write=vi.fn(),iframe={set srcdoc(value:string){write(value);},contentWindow:{postMessage:vi.fn()}};
  const close=vi.fn(),host=vi.fn<Host>(()=>close);
  const prepare=vi.fn().mockResolvedValue('@view{proof} publish body:"synthetic"'),adopt=vi.fn();
  const binding={engine:{captureComposition:()=>({generation:"g"}),prepareViewCommand:prepare},generation:"g",root:"chart",rootInstance:"chart-instance",member:"chart",revision:"0",inputRevision:"0",linkedInputs:[]} as unknown as ViewBinding;
  const view=(hostBinding:Host,shown=model)=><ComposeContext.Provider value={commandSupport?{reviewed:adopt,compose:vi.fn(),taken:new Set()}:undefined}><ViewBindingContext.Provider value={commandSupport?()=>binding:undefined}><InstanceInteractionHost.Provider value={hostBinding}><module.Component model={shown} children={[]} renderChild={()=><span>Retained member</span>}/></InstanceInteractionHost.Provider></ViewBindingContext.Provider></ComposeContext.Provider>;
  let tree!:ReturnType<typeof create>;
  await act(async()=>{tree=create(view(host),{createNodeMock:node=>node.type==="iframe"?iframe:{}});});
  act(()=>tree.root.findByType("iframe").props.onLoad());
  const receive=(message:unknown)=>act(()=>channels[0]!.port1.onmessage!({data:stringifyExactJson(message)}));
  const state=initial(input);
  receive({kind:"ready",digest:definition.digest,state,outputs:outputs(state)});
  return {tree,view,host,close,channels,write,iframe,receive,prepare,adopt,model,binding};
}

it("rebinds coordination while retaining the iframe, channel and committed controller state",async()=>{
  const mountedView=await mounted(),{tree,view,host,close,channels,write,iframe}=mountedView;
  const controller=host.mock.calls[0]![2];
  act(()=>{controller.adopt({cursor:"2030-06-15T08:05:00Z"});});
  const state=controller.committed(),revision=controller.committedRevision();
  const nextClose=vi.fn(),nextHost=vi.fn(()=>nextClose);
  act(()=>tree.update(view(nextHost)));
  expect(close).toHaveBeenCalledOnce();expect(nextHost).toHaveBeenCalledWith("view/chart",host.mock.calls[0]![1],controller);
  expect(controller.committed()).toBe(state);expect(controller.committedRevision()).toBe(revision);
  expect(channels).toHaveLength(1);expect(write).toHaveBeenCalledOnce();expect(iframe.contentWindow.postMessage).toHaveBeenCalledOnce();
  expect(channels[0]!.port1.close).not.toHaveBeenCalled();
  act(()=>tree.unmount());expect(nextClose).toHaveBeenCalledOnce();expect(close).toHaveBeenCalledOnce();
  expect(channels[0]!.port1.close).toHaveBeenCalledOnce();expect(channels[0]!.port2.close).toHaveBeenCalledOnce();
});

it("does not reopen a rejected renderer when coordination changes",async()=>{
  const {tree,view,receive,close,channels,write}=await mounted();
  receive({kind:"error"});
  const nextHost=vi.fn(()=>vi.fn());act(()=>tree.update(view(nextHost)));
  expect(close).toHaveBeenCalledOnce();expect(nextHost).not.toHaveBeenCalled();
  expect(tree.root.findByProps({role:"status"}).children.join("")).toContain("Close and reopen");
  expect(channels).toHaveLength(1);expect(write).toHaveBeenCalledOnce();
  act(()=>tree.unmount());expect(close).toHaveBeenCalledOnce();
});


it("keeps opaque member renderers mounted while clipping or local visibility hides their slot",async()=>{
  const {tree,receive}=await mounted(true);
  const rect={key:"members/0",x:0,y:30,width:400,height:24};
  receive({kind:"geometry",height:60,boxes:[{...rect,clip:{x:0,y:30,width:400,height:24}}]});
  const member=tree.root.findByType("span");
  receive({kind:"geometry",height:60,boxes:[{...rect,clip:{x:0,y:30,width:400,height:0}}]});
  expect(tree.root.findByType("span")).toBe(member);
  expect(tree.root.findByProps({"data-host-slot":"members/0"}).props.style.height).toBe(0);
  receive({kind:"geometry",height:60,boxes:[{...rect,clip:{x:0,y:30,width:400,height:24}}]});
  expect(tree.root.findByType("span")).toBe(member);
  act(()=>tree.unmount());
});

it("rejects inspection requests outside a window outlet",async()=>{
 const {tree,receive}=await mounted();receive({kind:"inspect"});
 expect(tree.root.findByProps({role:"status"}).children.join("")).toContain("communication rejected");act(()=>tree.unmount());
});

it("presents a member inspector beside window plots using its existing controller and channel authority",async()=>{
 vi.stubGlobal("ResizeObserver",class{observe(){} disconnect(){}});
 vi.stubGlobal("getComputedStyle",()=>({lineHeight:"21px"}));vi.useFakeTimers();
 const channels:any[]=[];
 vi.stubGlobal("MessageChannel",class{port1={onmessage:undefined,postMessage:vi.fn(),close:vi.fn(),start:vi.fn()};port2={close:vi.fn()};constructor(){channels.push(this);}});
 const definition=valueViewModules.named("timeline")!.definition!;
 const module=externalModule({definition,digest:definition.artifact!,javascript:"void 0",css:""});
 const value=timelineValue(),input=decodeContract(definition,definition.input,value.data,true,value.type) as Input;
 const member={kind:"empty" as const,path:"member",text:"empty" as const};
 const model={input,path:"view/group",identity:"group",slots:{members:[member]},mode:"window",coordinated:false};
 const source={input,path:"view/member",identity:"member-instance",slots:{},mode:"window",coordinated:true};
 const host=vi.fn<Host>(()=>()=>{}),iframes:any[]=[];
 let tree!:ReturnType<typeof create>;
 await act(async()=>{tree=create(<InstanceInteractionHost.Provider value={host}><module.Component model={model} children={[member]} renderChild={()=><module.Component model={source} children={[]} renderChild={()=>null}/>} /></InstanceInteractionHost.Provider>,{createNodeMock:node=>{
  if(node.type==="iframe"){const frame={srcdoc:"",contentWindow:{postMessage:vi.fn()}};iframes.push(frame);return frame;}
  return {clientWidth:1200};
 }});});
 const receive=(channel:number,message:unknown)=>act(()=>channels[channel].port1.onmessage({data:stringifyExactJson(message)}));
 act(()=>tree.root.findAllByType("iframe")[0]!.props.onLoad());
 receive(0,{kind:"ready",digest:definition.digest,state:initial(input),outputs:outputs(initial(input)),inspectable:true});
 receive(0,{kind:"geometry",height:200,boxes:[{key:"members/0",x:0,y:0,width:800,height:150}]});
 await act(async()=>{});
 act(()=>tree.root.findAllByType("iframe")[1]!.props.onLoad());
 receive(1,{kind:"ready",digest:definition.digest,state:initial(input),outputs:outputs(initial(input)),inspectable:true});
 const owner=host.mock.calls.find(call=>call[0]==="view/member")![2];
 const revision=owner.committedRevision(),bindings=host.mock.calls.length;
 receive(1,{kind:"inspect"});await act(async()=>{});
 expect(tree.root.findByProps({"aria-label":"Timeline source inspection"})).toBeDefined();
 expect(channels).toHaveLength(3); // second sandbox presentation, not a second instance writer
 act(()=>tree.root.findAllByType("iframe")[2]!.props.onLoad());
 receive(2,{kind:"ready",digest:definition.digest,state:initial(input),outputs:outputs(initial(input)),inspectable:true});
 expect(host).toHaveBeenCalledTimes(bindings);expect(owner.committedRevision()).toBe(revision);
 const state={...owner.committed() as Record<string,unknown>,cursor:"2030-06-15T08:05:00Z"};
 receive(2,{kind:"event",base:revision,event:{kind:"cursor",at:state.cursor},state,outputs:outputs(state as ReturnType<typeof initial>),events:[]});
 expect((owner.committed() as Record<string,unknown>).cursor).toBe(state.cursor);
 receive(2,{kind:"event",base:revision,event:{kind:"cursor",at:null},state:{...state,cursor:null},outputs:outputs({...state,cursor:null} as ReturnType<typeof initial>),events:[]});
 expect((owner.committed() as Record<string,unknown>).cursor).toBe(state.cursor); // stale revisions are never replayed
 act(()=>tree.root.findByProps({"aria-label":"Timeline source inspection"}).findByType("button").props.onClick());
 expect(channels[2].port1.close).toHaveBeenCalledOnce();expect(channels[1].port1.close).not.toHaveBeenCalled();act(()=>tree.unmount());
});

it("tells the View whether the reader's focus is inside it, without saying where focus went",async()=>{
  vi.stubGlobal("ResizeObserver",class{observe(){} disconnect(){}});
  vi.stubGlobal("getComputedStyle",()=>({lineHeight:"21px"}));
  vi.useFakeTimers();
  const channels:{port1:{onmessage?:(event:{data:string})=>void;postMessage:ReturnType<typeof vi.fn>;close:ReturnType<typeof vi.fn>;start:ReturnType<typeof vi.fn>}}[]=[];
  vi.stubGlobal("MessageChannel",class{port1={onmessage:undefined,postMessage:vi.fn(),close:vi.fn(),start:vi.fn()};port2={close:vi.fn()};constructor(){channels.push(this);}});
  const listeners=new Map<string,(()=>void)[]>(),on=(name:string,fn:()=>void)=>listeners.set(name,[...(listeners.get(name)??[]),fn]);
  const frameElement={description:"frame"},elsewhere={description:"another cell"};
  const doc={focused:true,hasFocus(){return this.focused;},activeElement:elsewhere as unknown,addEventListener:on,removeEventListener:()=>{},defaultView:{addEventListener:on,removeEventListener:()=>{}}};
  const box={ownerDocument:doc,contains:(node:unknown)=>node===frameElement};
  const definition=valueViewModules.named("timeline")!.definition!;
  const module=externalModule({definition,digest:definition.artifact!,javascript:"void 0",css:""});
  const value=timelineValue(),input=decodeContract(definition,definition.input,value.data,true,value.type) as Input;
  const model={input,path:"view/chart",identity:"chart-instance",slots:{},mode:"preview",coordinated:false};
  const iframe={set srcdoc(_value:string){},contentWindow:{postMessage:vi.fn()}};
  let tree!:ReturnType<typeof create>;
  await act(async()=>{tree=create(<InstanceInteractionHost.Provider value={()=>()=>{}}><module.Component model={model} children={[]} renderChild={()=>null}/></InstanceInteractionHost.Provider>,{createNodeMock:node=>node.type==="iframe"?iframe:box});});
  act(()=>tree.root.findByType("iframe").props.onLoad());
  const renders=()=>channels[0]!.port1.postMessage.mock.calls.map(([text])=>JSON.parse(text as string)).filter(m=>m.kind==="render");
  /** Draw, then acknowledge it, as a frame does. */
  const draw=()=>act(()=>{vi.advanceTimersByTime(200);const last=renders().at(-1);if(last)channels[0]!.port1.onmessage!({data:JSON.stringify({kind:"ack",sequence:last.sequence})});});
  const fire=(name:string)=>{act(()=>{for(const fn of listeners.get(name)??[])fn();vi.advanceTimersByTime(0);});draw();};
  draw();
  expect(renders().at(-1)!.context.active).toBe(false);
  doc.activeElement=frameElement;fire("focusin");
  expect(renders().at(-1)!.context.active).toBe(true);
  const drawn=renders().length;
  fire("pointerdown");
  expect(renders()).toHaveLength(drawn);
  doc.focused=false;fire("blur");
  expect(renders().at(-1)!.context.active).toBe(false);
  doc.focused=true;fire("focus");
  expect(renders().at(-1)!.context.active).toBe(true);
  doc.activeElement=elsewhere;fire("focusin");
  expect(renders().at(-1)!.context.active).toBe(false);
  expect(JSON.stringify(renders())).not.toContain("another cell");
  act(()=>tree.unmount());
});

it("reviews commands outside the sandbox, adopts only on native click and withdraws on input change",async()=>{
  const {tree,view,host,receive,prepare,adopt,model}=await mounted(false,true);
  await act(async()=>{receive({kind:"command-prepare",request:1,epoch:0,template:"publish",arguments:{body:"synthetic"}});});
  expect(prepare).toHaveBeenCalledOnce();expect(adopt).not.toHaveBeenCalled();
  const panel=tree.root.findByProps({"aria-label":"Review command"});
  expect(panel.findByType("pre").children.join("")).toContain("synthetic");
  act(()=>panel.findAllByType("button")[0]!.props.onClick());expect(adopt).toHaveBeenCalledOnce();
  await act(async()=>{receive({kind:"command-prepare",request:2,epoch:0,template:"publish",arguments:{body:"synthetic"}});});
  expect(tree.root.findAllByProps({"aria-label":"Review command"})).toHaveLength(1);
  act(()=>tree.update(view(host,{...model,input:{...model.input}})));
  expect(tree.root.findAllByProps({"aria-label":"Review command"})).toHaveLength(0);expect(adopt).toHaveBeenCalledOnce();
  act(()=>tree.unmount());
});


it("destroys the sandbox document when its authority realm changes",async()=>{
  const {tree,view,host,channels,binding}=await mounted(false,true);
  const old=tree.root.findByType("iframe");
  (binding as {authorityEpoch?:string}).authorityEpoch="replacement-realm";
  await act(async()=>{tree.update(view(host));});
  expect(channels[0]!.port1.close).toHaveBeenCalledOnce();
  expect(channels).toHaveLength(2);expect(tree.root.findByType("iframe")).not.toBe(old);
  act(()=>tree.unmount());
});
