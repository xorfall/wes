import {act,create,type ReactTestRenderer} from "react-test-renderer";
import {useState} from "react";
import {afterEach,expect,it,vi} from "vitest";
import {startFrame} from "../../../../packages/view-sdk/frame";

const host=vi.hoisted(()=>({render:vi.fn()}));
vi.mock("react-dom/client",()=>({createRoot:()=>({render:host.render})}));
afterEach(()=>{vi.unstubAllGlobals();vi.useRealTimers();host.render.mockReset();});

it("changes only the theme stylesheet while committed and local state, pending events and revision survive",()=>{
  vi.useFakeTimers();
  let connect:(event:unknown)=>void=()=>{},resize:()=>void=()=>{};
  const parent={},style={tagName:"STYLE",textContent:""},rootElement={scrollHeight:100,dataset:{}};
  const viewport={clientWidth:800};
  vi.stubGlobal("parent",parent);
  vi.stubGlobal("document",{
    getElementById:(id:string)=>id==="wes-view-theme"?style:rootElement,
    querySelectorAll:()=>[],addEventListener:()=>{},fonts:{ready:Promise.resolve()},
    documentElement:viewport,createElement:()=>({getContext:()=>({font:'',measureText:()=>({width:80})})}),
  });
  vi.stubGlobal('getComputedStyle',()=>({getPropertyValue:()=>'',lineHeight:'20px'}));
  vi.stubGlobal("window",{innerHeight:400,addEventListener:(name:string,callback:typeof connect)=>{if(name==="message")connect=callback;if(name==='resize')resize=callback as ()=>void;},removeEventListener:()=>{}});
  vi.stubGlobal("ResizeObserver",class{observe(){} disconnect(){}});
  const port={onmessage:undefined as undefined|((message:{data:string})=>void),postMessage:vi.fn(),start:vi.fn()};
  const initial=vi.fn(()=>({selected:"first"})),reduce=vi.fn((_state:unknown,event:{selected:string})=>event),outputs=vi.fn((state:{selected:string})=>state);
  let renderer:ReactTestRenderer|undefined;
  host.render.mockImplementation(element=>act(()=>{if(renderer)renderer.update(element);else renderer=create(element);}));
  const Component=({state,emit,revision,context}:{state:{selected:string};emit:(event:{selected:string})=>void;revision:number;context:{mode:string;instance:string|null;allocation:{columns:number;rows:number}}})=>{
    const [local,setLocal]=useState(0);
    return <button data-columns={context.allocation.columns} data-rows={context.allocation.rows} data-mode={context.mode} data-instance={context.instance} onClick={()=>{setLocal(count=>count+1);emit({selected:"second"});}}>{state.selected}:{revision}:{local}</button>;
  };
  startFrame({definition:{interaction:{},digest:"fixture"},initial,reduce,outputs,Component} as unknown as Parameters<typeof startFrame>[0]);
  connect({source:parent,data:"wes-view-connect",ports:[port]});
  const receive=(message:unknown)=>port.onmessage!({data:JSON.stringify(message)});
  receive({kind:"render",input:{title:"services"},sequence:1,context:{mode:"preview",instance:"leaf-identity"}});
  expect(renderer!.root.findByType("button").props["data-instance"]).toBe("leaf-identity");
  act(()=>renderer!.root.findByType("button").props.onClick());
  const rootBefore=renderer,paints=host.render.mock.calls.length;
  receive({kind:"theme",css:':root{--surface:#14161A;--type-mono-family:"Menlo"}'});
  expect(style.textContent).toContain('"Menlo"');
  expect(host.render).toHaveBeenCalledTimes(paints);
  expect(initial).toHaveBeenCalledOnce();expect(reduce).toHaveBeenCalledOnce();
  receive({kind:"event-reply",accepted:true,state:{selected:"second"},revision:7});
  expect(renderer).toBe(rootBefore);
  expect(renderer!.root.findByType("button").children.join("")).toBe("second:7:1");
  viewport.clientWidth=320;
  resize();
  expect(renderer!.root.findByType('button').props['data-columns']).toBe(40);
  expect(renderer!.root.findByType('button').props['data-rows']).toBe(20);
  expect(renderer!.root.findByType('button').children.join('')).toBe('second:7:1');
  receive({kind:"theme",css:':root{--surface:#FFFFFF;--space-md:12px}'});
  expect(renderer!.root.findByType("button").children.join("")).toBe("second:7:1");
  expect(initial).toHaveBeenCalledOnce();expect(reduce).toHaveBeenCalledOnce();
  expect(port.postMessage.mock.calls.map(([text])=>JSON.parse(text)).filter(message=>message.kind==="event")).toHaveLength(1);
  expect(port.postMessage.mock.calls.map(([text])=>JSON.parse(text)).filter(message=>message.kind==="ack")).toEqual([{kind:"ack",sequence:1}]);
  receive({kind:"render",input:{title:"updated"},sequence:2,context:{mode:"window",instance:"leaf-identity"}});
  expect(renderer!.root.findByType("button").props["data-mode"]).toBe("window");
  expect(renderer!.root.findByType("button").children.join("")).toBe("second:7:1");
  expect(port.postMessage.mock.calls.map(([text])=>JSON.parse(text)).filter(message=>message.kind==="ack")).toEqual([{kind:"ack",sequence:1},{kind:"ack",sequence:2}]);
  act(()=>renderer!.unmount());
});

it("rejects a context whose active flag is not a boolean, and notifies the host when the frame window gains focus",()=>{
  let connect:(event:unknown)=>void=()=>{};const windowListeners=new Map<string,()=>void>();
  const parent={},style={tagName:"STYLE",textContent:""},rootElement={scrollHeight:100,dataset:{}};
  vi.stubGlobal("parent",parent);
  vi.stubGlobal("document",{getElementById:(id:string)=>id==="wes-view-theme"?style:rootElement,querySelectorAll:()=>[],addEventListener:()=>{},fonts:{ready:Promise.resolve()},documentElement:{clientWidth:800},createElement:()=>({getContext:()=>({font:'',measureText:()=>({width:80})})})});
  vi.stubGlobal('getComputedStyle',()=>({getPropertyValue:()=>'',lineHeight:'20px'}));
  vi.stubGlobal("window",{innerHeight:400,addEventListener:(name:string,callback:typeof connect)=>{if(name==="message")connect=callback;else windowListeners.set(name,callback as ()=>void);},removeEventListener:()=>{}});
  vi.stubGlobal("ResizeObserver",class{observe(){} disconnect(){}});
  const port={onmessage:undefined as undefined|((message:{data:string})=>void),postMessage:vi.fn(),start:vi.fn()};
  host.render.mockImplementation(()=>{});
  startFrame({definition:{interaction:null,digest:"fixture"},Component:()=>null} as unknown as Parameters<typeof startFrame>[0]);
  connect({source:parent,data:"wes-view-connect",ports:[port]});
  const kinds=()=>port.postMessage.mock.calls.map(([text])=>JSON.parse(text).kind);
  windowListeners.get("focus")!();
  expect(kinds()).toContain("focus");
  port.onmessage!({data:JSON.stringify({kind:"render",input:{},sequence:1,context:{mode:"preview",instance:null,active:"yes"}})});
  expect(kinds()).toContain("error");
});
