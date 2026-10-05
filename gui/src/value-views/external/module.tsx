import {createContext,useContext,useEffect,useLayoutEffect,useRef,useState} from "react";
import {createPortal} from "react-dom";
import type {ValueViewModule,ViewComponentProps} from "../contract";
import {decodeContract} from "../definition";
import {InteractionController} from "../interaction";
import {InstanceInteractionHost} from "../interactive";
import {parseExactJson,stringifyExactJson} from "../../exact-json";
import {frameHeight} from "../layout";
import type {Mode} from "../../presentation/types";
import {LatestDelivery} from "./delivery";
import {frameDocument,type ViewAsset} from "./document";
import {currentTheme,followTheme} from "./theme";
import {retainAsset} from "./assets";
import {MessageRate} from "./message-rate";
import fonts from "../../surface/fonts.css?inline";
import authoring from "@wes/view-sdk/authoring.json";
import type {PresentationNode} from "../../presentation/types";
interface Model {input:unknown;identity?:string;path:string;slots:Readonly<Record<string,readonly PresentationNode[]>>;mode:Mode;coordinated:boolean}
interface InspectionSession { controller:InteractionController<unknown,unknown>; apply:(message:Record<string,unknown>)=>boolean }
interface InspectionOutlet { selected?:string; target:HTMLElement|null; select:(key:string|undefined)=>void }
const InspectionContext=createContext<InspectionOutlet|undefined>(undefined);
interface Rectangle {x:number;y:number;width:number;height:number}
interface Box extends Rectangle {key:string;clip?:Rectangle}
const rectangle=(b:unknown):b is Rectangle=>object(b)&&[b.x,b.y,b.width,b.height].every(v=>typeof v==="number"&&Number.isFinite(v)&&Math.abs(v)<20001)&&Number(b.width)>=0&&Number(b.height)>=0;
const object=(v:unknown):v is Record<string,unknown>=>!!v&&typeof v==="object"&&!Array.isArray(v);
function outputs(asset:ViewAsset,value:unknown){
  if(!object(value))throw new Error("Invalid outputs");
  const ports=Object.entries(asset.definition.outputs).filter(([,p])=>p.mode==="state");
  if(Object.keys(value).length!==ports.length)throw new Error("Invalid output fields");
  for(const [name,p] of ports){if(!Object.hasOwn(value,name))throw new Error("Missing output");decodeContract(asset.definition,p.type,value[name]);}
}
export function externalModule(asset:ViewAsset):ValueViewModule {
  retainAsset(asset);
  const module:ValueViewModule={id:`${asset.definition.id}--${asset.digest}`,definition:asset.definition,matches:(type,data)=>{
      const schema=asset.definition.contracts[asset.definition.input],marker=schema?.fields?.view;
      if(!marker||!object(data)||!asset.definition.contracts[marker.type]?.constraints.enum.includes(data.view as string))return false;
      try{decodeContract(asset.definition,asset.definition.input,data,true,type);return true;}catch{return false;}
    },
    present(input,host){const data=decodeContract(asset.definition,asset.definition.input,input.data,true,input.type);host.spend(8);
      return {model:{input:data,identity:input.instanceKey,path:input.path,slots:input.slots??{},mode:input.context.mode,coordinated:input.coordinated??false} satisfies Model,children:Object.values(input.slots??{}).flat(),ownLines:8,summary:[{text:asset.definition.name,tone:"dim"}]};},
    Component:props=><ExternalView key={(props.model as Model).identity} {...props} module={module} asset={asset}/>,
  };return module;
}
function ExternalView(props:ViewComponentProps & {module:ValueViewModule;asset:ViewAsset}) {
  const inherited=useContext(InspectionContext);
  const model=props.model as Model,ref=useRef<HTMLDivElement>(null);
  const [wide,setWide]=useState(false),[selected,select]=useState<string>(),[target,setTarget]=useState<HTMLElement|null>(null);
  useLayoutEffect(()=>{
    if(!ref.current || typeof ResizeObserver==="undefined")return;
    const measure=()=>setWide(ref.current!.clientWidth>=960);
    measure();const observer=new ResizeObserver(measure);observer.observe(ref.current);return ()=>observer.disconnect();
  },[]);
  const grouped=model.mode==="window" && Object.values(model.slots).some(nodes=>nodes.length>0);
  const enabled=grouped && wide;
  return <div ref={ref} className={`external-view-shell${enabled && selected ? " external-inspecting" : ""}`}>
    <InspectionContext.Provider value={enabled ? {selected,select,target} : inherited}>
      <ExternalCanvas {...props}/>
      {enabled && selected && <aside className="external-inspection" aria-label="Timeline source inspection" onKeyDown={event=>{if(event.key==="Escape"){event.preventDefault();event.stopPropagation();select(undefined);}}}>
        <header><strong>Source inspection</strong><button className="cell-action" onClick={()=>select(undefined)}>Close</button></header>
        <div ref={setTarget}/>
      </aside>}
    </InspectionContext.Provider>
  </div>;
}
function ExternalCanvas({model:raw,renderChild,module,asset,mirror}:ViewComponentProps & {module:ValueViewModule;asset:ViewAsset;mirror?:InspectionSession}){
  const model=raw as Model,modelRef=useRef(model);modelRef.current=model;
  const outlet=useContext(InspectionContext),outletRef=useRef(outlet);outletRef.current=outlet;
  const inspectionKey=`${model.path}:${model.identity??""}`;
  const [session,setSession]=useState<InspectionSession>();
  const inspectionOutlet=Boolean(outlet && model.coordinated && !mirror);
  const inspectionActive=outlet?.selected===inspectionKey;
  useEffect(()=>()=>{if(!mirror && outletRef.current?.selected===inspectionKey)outletRef.current.select(undefined);},[inspectionKey,mirror]);
  const box=useRef<HTMLDivElement>(null),frame=useRef<HTMLIFrameElement>(null);
  const host=useContext(InstanceInteractionHost),hostRef=useRef(host);hostRef.current=host;
  const rebindShared=useRef<()=>void>();
  useLayoutEffect(()=>rebindShared.current?.(),[host]);
  const delivery=useRef<LatestDelivery>(),port=useRef<MessagePort>(),didLoad=useRef(false),connect=useRef<()=>void>();
  const theme=useRef(""),sendTheme=useRef<()=>void>();
  const [document,setDocument]=useState<string>(),[problem,setProblem]=useState<string>(),[height,setHeight]=useState(100),[boxes,setBoxes]=useState<readonly Box[]>([]);
  const slots=Object.entries(model.slots).flatMap(([name,nodes])=>nodes.map((node,index)=>({key:`${name}/${index}`,node})));
  const heights=useRef<Record<string,number>>({}),children=new Map(slots.map(slot=>[slot.key,slot.node]));
  const update=()=>delivery.current?.update(()=>({input:modelRef.current.input,context:{mode:modelRef.current.mode,instance:modelRef.current.identity??null,coordinated:modelRef.current.coordinated,inspectionOnly:!!mirror,inspectionActive:outletRef.current?.selected===inspectionKey,inspectionOutlet:!!outletRef.current && modelRef.current.coordinated && !mirror},slots:Object.fromEntries(Object.entries(modelRef.current.slots).map(([name,nodes])=>[name,nodes.map((_node,index)=>({key:`${name}/${index}`,height:heights.current[`${name}/${index}`]??120}))]))}));
  useLayoutEffect(update,[model,inspectionOutlet,inspectionActive]);
  useEffect(()=>{
    if(!box.current)return;
    theme.current=currentTheme(box.current!);
    let closed=false;void frameDocument(asset,theme.current,fonts).then(doc=>{if(!closed)setDocument(doc);}).catch(()=>{if(!closed)setProblem("View assets could not be opened safely.");});
    return ()=>{closed=true;};
  },[asset]);
  useEffect(()=>box.current?followTheme(box.current,css=>{theme.current=css;sendTheme.current?.();}):undefined,[]);
  useEffect(()=>{
    if(!document)return;
    const iframe=frame.current!,channel=new MessageChannel(),abort=new AbortController();
    let controller:InteractionController<unknown,unknown>|undefined,remote:ValueViewModule|undefined,closeShared:(()=>void)|undefined,stopController:(()=>void)|undefined;
    let inspectable=false;
    let nextState:unknown,lastOutputs:unknown={},lastEvents:readonly {port:string;value:unknown}[]=[];
    const limits=authoring.runtimeLimits;
    const rate=new MessageRate();let closed=false,failed=false;
    const detachShared=()=>{closeShared?.();closeShared=undefined;};
    const attachShared=()=>{detachShared();if(!closed&&!failed&&controller&&remote)closeShared=hostRef.current?.(modelRef.current.path,remote,controller);};
    rebindShared.current=attachShared;
    const fail=(message:string)=>{if(closed||failed)return;failed=true;setSession(undefined);setProblem(message);delivery.current?.close();channel.port1.close();detachShared();stopController?.();stopController=undefined;};
    const send=(value:unknown)=>{if(closed||failed)return;const text=stringifyExactJson(value);if(text.length>limits.messageCharacters)throw new Error("Message budget");channel.port1.postMessage(text);};
    const sync=()=>{if(controller)send({kind:"state",state:controller.committed(),revision:controller.committedRevision()});};
    const applyEvent=(message:Record<string,unknown>)=>{
      if(!controller||!asset.definition.interaction||closed||failed)throw new Error("Unexpected event");
      const event=decodeContract(asset.definition,asset.definition.interaction.event,message.event);
      const candidate=decodeContract(asset.definition,asset.definition.interaction.state,message.state);outputs(asset,message.outputs);
      if(!Array.isArray(message.events)||message.events.length>16)throw new Error("Invalid events");
      const events=message.events.map(item=>{if(!object(item)||typeof item.port!=="string")throw new Error();const p=asset.definition.outputs[item.port];if(!p||p.mode!=="event")throw new Error();return {port:item.port,value:decodeContract(asset.definition,p.type,item.value)};});
      if(message.base!==controller.committedRevision())return false;
      const previousState=nextState,previousOutputs=lastOutputs,previousEvents=lastEvents;
      nextState=candidate;lastOutputs=message.outputs;lastEvents=events;
      const accepted=controller.emit(event);
      if(!accepted){nextState=previousState;lastOutputs=previousOutputs;lastEvents=previousEvents;}
      return accepted;
    };
    const d=new LatestDelivery(text=>channel.port1.postMessage(text),fail);delivery.current=d;port.current=channel.port1;
    channel.port1.onmessage=event=>{
      if(closed||failed)return;
      try{
        if(typeof event.data!=="string"||event.data.length>limits.messageCharacters)throw new Error("Message budget");
        const message=parseExactJson(event.data) as Record<string,unknown>;
        if(!object(message))throw new Error("Invalid message");rate.take(message.kind,performance.now());
        if(message.kind==="ack"){if(typeof message.sequence!=="number"||!Number.isSafeInteger(message.sequence))throw new Error();d.ack(message.sequence);}
        else if(message.kind==="ready"){
          if(controller||message.digest!==asset.definition.digest)throw new Error("Definition mismatch");
          outputs(asset,message.outputs);lastOutputs=message.outputs;
          if(message.inspectable!==undefined && typeof message.inspectable!=="boolean")throw new Error("Invalid inspection capability");
          inspectable=message.inspectable===true;
          const interaction=asset.definition.interaction;
          if(mirror){
            if(!inspectable)throw new Error("Inspection unavailable");
            controller=mirror.controller;stopController=controller.subscribe(sync);sync();
          }else if(interaction){
            const initial=decodeContract(asset.definition,interaction.state,message.state);
            const valid=(name:string,value:unknown):value is unknown=>{try{decodeContract(asset.definition,name,value);return true;}catch{return false;}};
            controller=new InteractionController({protocol:{id:interaction.protocol,state:(v):v is unknown=>valid(interaction.state,v),event:(v):v is unknown=>valid(interaction.event,v)},initial:()=>initial,reduce:()=>nextState},modelRef.current.input);
            stopController=controller.subscribe(()=>{setProblem(controller!.snapshot().error);sync();});
            remote={...module,outputSnapshot:()=>lastOutputs,outputEvents:()=>lastEvents};
            attachShared();setSession({controller,apply:applyEvent});sync();
          }
        }else if(message.kind==="event"){
          const accepted=mirror ? mirror.apply(message) : applyEvent(message);
          if(!controller)throw new Error("Unexpected event before readiness");
          send({kind:"event-reply",accepted,state:controller.committed(),revision:controller.committedRevision()});
        }else if(message.kind==="inspect"){
          if(!inspectable || mirror || !modelRef.current.coordinated || !outletRef.current)throw new Error("Inspection is not available here");
          outletRef.current.select(inspectionKey);
        }else if(message.kind==="geometry"){
          if(typeof message.height!=="number"||!Number.isFinite(message.height)||message.height<0||!Array.isArray(message.boxes)||message.boxes.length>32)throw new Error();
          const allowed=new Set(Object.entries(modelRef.current.slots).flatMap(([name,nodes])=>nodes.map((_n,i)=>`${name}/${i}`)));
          const next=message.boxes.map(b=>{if(!object(b)||typeof b.key!=="string"||!allowed.has(b.key)||!rectangle(b)||(b.clip!==undefined&&!rectangle(b.clip)))throw new Error();return b as unknown as Box;});
          if(new Set(next.map(b=>b.key)).size!==next.length)throw new Error();
          setBoxes(old=>JSON.stringify(old)===JSON.stringify(next)?old:next);setHeight(frameHeight(asset.definition.layout,modelRef.current.mode,message.height,parseFloat(getComputedStyle(box.current!).lineHeight)));
        }else if(message.kind==="focus"){
          iframe.dispatchEvent(new FocusEvent("focusin",{bubbles:true}));
        }else if(message.kind==="focus-slot"){
          if(typeof message.key!=="string")throw new Error();const el=[...box.current!.querySelectorAll<HTMLElement>("[data-host-slot]")].find(e=>e.dataset.hostSlot===message.key);
          el?.querySelector<HTMLElement>("button,input,select,textarea,[tabindex='0']")?.focus();
        }else if(message.kind==="shortcut"){
          if(!["Tab","Enter","r","R","m","M"].includes(String(message.key))||!(message.ctrl||message.meta))throw new Error();
          iframe.dispatchEvent(new KeyboardEvent("keydown",{key:String(message.key),ctrlKey:!!message.ctrl,metaKey:!!message.meta,shiftKey:!!message.shift,bubbles:true,cancelable:true}));
        }else if(message.kind==="error")fail("View renderer failed. Close and reopen to retry.");else throw new Error("Unknown message");
      }catch{fail("View communication rejected: invalid data or message rate exceeded. Close and reopen to retry.");}
    };
    let connected=false;
    sendTheme.current=()=>{if(connected&&!closed&&!failed){try{send({kind:"theme",css:theme.current});}catch{fail("View theme exceeds the message budget.");}}};
    const loaded=()=>{if(closed||failed)return;if(connected){fail("View navigation is not permitted. Close and reopen to retry.");return;}connected=true;iframe.contentWindow!.postMessage("wes-view-connect","*",[channel.port2]);sendTheme.current?.();update();};
    connect.current=loaded;
    channel.port1.start();
    didLoad.current=false;iframe.srcdoc=document;
    return ()=>{closed=true;abort.abort();d.close();channel.port1.close();channel.port2.close();detachShared();stopController?.();if(rebindShared.current===attachShared)rebindShared.current=undefined;delivery.current=undefined;port.current=undefined;connect.current=undefined;sendTheme.current=undefined;};
  },[document,asset,module,mirror]);
  return <><div ref={box} style={{position:"relative",maxWidth:"100%",minWidth:0,overflow:"hidden"}}>
    {problem&&<p className="mono-warn" role="status">{problem}</p>}
    {document&&<iframe ref={frame} onLoad={()=>{didLoad.current=true;connect.current?.();}} sandbox="allow-scripts" aria-label={asset.definition.name} style={{display:"block",border:0,width:"100%",height}}/>}
    {boxes.map(rect=>{const child=children.get(rect.key),clip=rect.clip??rect;return child?<div key={rect.key} data-host-slot={rect.key} style={{position:"absolute",left:clip.x,top:clip.y,width:clip.width,height:clip.height,overflow:"hidden"}}>
      <div style={{position:"relative",left:rect.x-clip.x,top:rect.y-clip.y,width:rect.width || "100%",maxHeight:rect.height || undefined,overflow:"auto"}}><SlotSize changed={size=>{if(size>0 && heights.current[rect.key]!==size){heights.current[rect.key]=size;update();}}}>{renderChild(child)}</SlotSize></div>
    </div>:null;})}
  </div>
    {!mirror && session && outlet?.selected===inspectionKey && outlet.target && createPortal(
      <InspectionContext.Provider value={undefined}><ExternalCanvas model={model} children={[]} renderChild={renderChild} module={module} asset={asset} mirror={session}/></InspectionContext.Provider>,outlet.target)}
  </>;
}
function SlotSize({children,changed}:{children:React.ReactNode;changed:(height:number)=>void}){
  const ref=useRef<HTMLDivElement>(null),callback=useRef(changed);callback.current=changed;
  useLayoutEffect(()=>{const observer=new ResizeObserver(()=>callback.current(Math.min(20000,Math.ceil(ref.current!.scrollHeight))));observer.observe(ref.current!);return()=>observer.disconnect();},[]);
  return <div ref={ref}>{children}</div>;
}
