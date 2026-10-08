/** Browser host adapter. Author code and React execute only inside the sandbox document. */
import {useLayoutEffect,useRef, type ReactNode} from "react";
import {createRoot} from "react-dom/client";
import {parseExactJson,stringifyExactJson} from "./values";
import type {ViewDefinition} from "./contract";
import type {ViewRenderer} from "./index";
import {applyFrameTheme} from "./theme";
import authoring from "./authoring.json";
import {slotClip} from "./geometry";
import {FrameDatasets} from "./datasets";
type Renderer=ViewRenderer<unknown,unknown,unknown,unknown,Record<string,unknown>> & {definition:ViewDefinition};
const MAX_INPUT=authoring.runtimeLimits.inputBytes, MAX_EVENT=authoring.runtimeLimits.messageCharacters;
export function startFrame(view:Renderer) {
  let port:MessagePort|undefined,input:unknown,state:unknown=null,revision=0,sequence=0,acknowledged=0,ready=false,pending=false;
  let slots:Record<string,readonly {key:string;height:number}[]>={},root:ReturnType<typeof createRoot>;
  let context:{mode:"preview"|"expanded"|"window";instance:string|null;coordinated?:boolean;inspectionOnly?:boolean;inspectionOutlet?:boolean;inspectionActive?:boolean;active?:boolean;datasets?:boolean;datasetEpoch?:number}={mode:"window",instance:null};
  const queue:unknown[]=[];
  const send=(value:unknown,limit=MAX_EVENT)=>{const text=stringifyExactJson(value);if(text.length>limit)throw new Error("Frame message budget");port?.postMessage(text);};
  const fail=()=>{queue.length=0;pending=true;datasets.cancelAll("failed");try{send({kind:"error",message:"View renderer failed. Close and reopen to retry."});}catch{}};
  // Dataset pages are read only through the host; the frame never holds an address or token.
  const datasets=new FrameDatasets(value=>send(value));
  function emit(event:unknown) {
    if(!view.definition.interaction||pending&&queue.length>=authoring.runtimeLimits.pendingEvents)return fail();
    try {if(stringifyExactJson(event).length>MAX_EVENT)throw new Error();queue.push(event);flush();}catch{fail();}
  }
  function flush(){
    if(pending||!queue.length)return;
    try {const event=queue.shift(),next=view.reduce!(state,event);
      send({kind:"event",base:revision,event,state:next,outputs:view.outputs!(next),events:view.eventOutputs?.(state,next,event)??[]});pending=true;
    }catch{fail();}
  }
  let geometryTimer:ReturnType<typeof setTimeout>|undefined;
  const geometry=()=>{if(geometryTimer!==undefined)return;geometryTimer=setTimeout(()=>{
    geometryTimer=undefined;
    const boxes=[...document.querySelectorAll<HTMLElement>("[data-wes-slot]")].slice(0,32).map(el=>{const r=el.getBoundingClientRect();return {key:el.dataset.wesSlot,x:r.x,y:r.y,width:r.width,height:r.height,clip:slotClip(el,r)};});
    try{send({kind:"geometry",height:Math.min(20000,Math.ceil(document.getElementById("root")!.scrollHeight)),boxes});}catch{fail();}
  },100);};
  function Slot({slot}:{slot:{key:string;height:number}}){
    const ref=useRef<HTMLDivElement>(null);
    useLayoutEffect(()=>{const observer=new ResizeObserver(geometry);observer.observe(ref.current!);geometry();return()=>observer.disconnect();},[]);
    return <div ref={ref} data-wes-slot={slot.key} style={{minHeight:slot.height,minWidth:0}} tabIndex={0} onFocus={()=>{try{send({kind:"focus-slot",key:slot.key});}catch{fail();}}}/>;
  }
  function Frame(){
    useLayoutEffect(()=>{if(sequence!==acknowledged){send({kind:"ack",sequence});acknowledged=sequence;}geometry();});
    const nodes:Record<string,readonly ReactNode[]>={};
    for(const [name,items] of Object.entries(slots))nodes[name]=items.map(slot=><Slot key={slot.key} slot={slot}/>);
    const Component=context.inspectionOnly ? view.Inspection : view.Component;
    if(!Component)throw new Error("Inspection presentation unavailable");
    const rootElement=document.getElementById('root')!,style=getComputedStyle(rootElement);
    const measure=document.createElement('canvas').getContext('2d');
    if(measure)measure.font=`${style.getPropertyValue('--type-mono-size').trim()||'13px'} ${style.getPropertyValue('--type-mono-family').trim()||'monospace'}`;
    const advance=measure?.measureText('0000000000').width?measure.measureText('0000000000').width/10:7.8;
    const width=document.documentElement.clientWidth,height=window.innerHeight,leading=parseFloat(style.lineHeight)||21;
    const allocation={width,height,columns:Math.max(1,Math.floor(width/advance)),rows:Math.max(1,Math.floor(height/leading))};
    const {datasets:readable,datasetEpoch:_epoch,...shown}=context;
    return <Component input={input} state={state} revision={revision} emit={emit} slots={nodes} context={{...shown,allocation,datasets:readable?datasets:undefined,inspect:context.inspectionOutlet && view.Inspection ? ()=>send({kind:"inspect"}) : undefined}}/>;
  }
  function paint(){try{root.render(<Frame/>);}catch{fail();}}
  const connect=(event:MessageEvent)=>{
    if(port||event.source!==parent||event.data!=="wes-view-connect"||event.ports.length!==1)return;
    port=event.ports[0]!;window.removeEventListener("message",connect);
    root=createRoot(document.getElementById("root")!);
    port.onmessage=message=>{
      try {
        if(typeof message.data!=="string"||message.data.length>MAX_INPUT)throw new Error();
        const update=parseExactJson(message.data) as {kind:string;sequence:number;input:unknown;state?:unknown;revision?:number;slots?:typeof slots;accepted?:boolean;css?:unknown;context?:typeof context};
        if(update.kind==="render"){
          input=update.input;sequence=update.sequence;slots=update.slots??{};
          if(update.context){if(!["preview","expanded","window"].includes(update.context.mode)||(update.context.instance!==null&&typeof update.context.instance!=="string")||(update.context.coordinated!==undefined&&typeof update.context.coordinated!=="boolean"))throw new Error("Invalid View context");
            for(const key of ["inspectionOnly","inspectionOutlet","inspectionActive","active","datasets"] as const)if(update.context[key]!==undefined && typeof update.context[key]!=="boolean")throw new Error("Invalid inspection context");
            if(update.context.datasetEpoch!==undefined&&!Number.isSafeInteger(update.context.datasetEpoch))throw new Error("Invalid dataset context");
            // A new input binding settles every read still in flight for the old one.
            if(update.context.datasetEpoch!==context.datasetEpoch||!update.context.datasets)datasets.cancelAll("changed");
            context=update.context;document.getElementById("root")!.dataset.mode=context.mode;}
          if(!ready){state=view.initial?.(input)??null;ready=true;send({kind:"ready",state,outputs:view.outputs?.(state)??{},digest:view.definition.digest,inspectable:!!view.Inspection});}
          if(update.state!==undefined){state=update.state;revision=update.revision??revision;}
          paint();
        }else if(update.kind==="theme"){
          applyFrameTheme(document,update.css);geometry();
          void document.fonts?.ready.then(geometry);
        }else if(update.kind==="state"||update.kind==="event-reply"){
          state=update.state;revision=update.revision??revision;
          if(update.kind==="event-reply"){pending=false;if(!update.accepted)queue.length=0;}
          paint();flush();
        }else if(update.kind==="dataset-reply")datasets.settle(update as never);
        else if(update.kind==="scroll")window.scrollBy(0,Number((update as unknown as {delta:number}).delta));
        else throw new Error();
      }catch{fail();}
    };
    port.start();
  };
  window.addEventListener("message",connect);
  document.addEventListener("pointerdown",()=>{try{send({kind:"focus"});}catch{fail();}},true);
  document.addEventListener("focusin",()=>{try{send({kind:"focus"});}catch{fail();}});
  window.addEventListener("focus",()=>{try{send({kind:"focus"});}catch{fail();}});
  window.addEventListener("error",fail);window.addEventListener("unhandledrejection",fail);
  window.addEventListener("scroll",geometry,{passive:true,capture:true});
  window.addEventListener("resize",()=>{if(ready)paint();geometry();});
  document.addEventListener("keydown",event=>{
    if((event.ctrlKey||event.metaKey)&&["Tab","Enter","r","R","m","M"].includes(event.key)){
      event.preventDefault();try{send({kind:"shortcut",key:event.key,ctrl:event.ctrlKey,meta:event.metaKey,shift:event.shiftKey});}catch{fail();}
    }
  });
  new ResizeObserver(geometry).observe(document.getElementById("root")!);
}
