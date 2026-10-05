import {act,create,type ReactTestInstance,type ReactTestRenderer} from "react-test-renderer";
import {afterEach,beforeEach,expect,it,vi} from "vitest";
import {isLogValue,logRows,LogView} from "./LogView";
import {ExactNumber} from "../../exact-json";
import type {StoredValue} from "../../protocol";
const value:StoredValue={type:{kind:"list",element:{kind:"record",name:"DockerLogEvent",fields:[]}},provenance:{},data:[
 {container:"synthetic",sequence:1,received_at_ns:new ExactNumber("1790955000123456789"),timestamp_ns:null,stream:"stderr",text:"same message",partial:true,lossy:false,line_truncated:true},
 {container:"synthetic",sequence:2,received_at_ns:new ExactNumber("1790955000223456789"),timestamp_ns:new ExactNumber("1790955000200000000"),stream:"stdout",text:"same message",partial:false,lossy:true,line_truncated:false}]};

/** A bounded source window of synthetic events `[from, to)`, as a rolling stream presents it. */
const window=(from:number,to:number):StoredValue=>({...value,data:Array.from({length:to-from},(_,at)=>({container:"fixture",sequence:from+at,received_at_ns:new ExactNumber(String(1790955000000000000n+BigInt(from+at)*1000n)),timestamp_ns:null,stream:at%7===3?"stderr":"stdout",text:`synthetic event ${from+at}${(from+at)%5===0?" checkpoint":""}`,partial:false,lossy:false,line_truncated:false}))});
const key=(sequence:number)=>JSON.stringify(["fixture",String(sequence)]);

/**
 * Browser-like geometry for the log's own scroller: rows stack in rendered order with fractional
 * heights, `scrollTop` is clamped and rounded like a device pixel, and a resize observer can be fired.
 */
function geometry() {
 const state={keys:[] as string[],top:100,client:100,width:600,scrollTop:0,height:(_key:string,_width:number)=>19.5};
 const offsetOf=(key:string)=>{let y=0;for(const each of state.keys){if(each===key)return y;y+=state.height(each,state.width);}return Number.NaN;};
 const total=()=>state.keys.reduce((sum,each)=>sum+state.height(each,state.width),0);
 const box={
  get clientHeight(){return state.client;},get clientWidth(){return state.width;},get scrollHeight(){return Math.max(state.client,total());},
  get scrollTop(){return state.scrollTop;},set scrollTop(next:number){state.scrollTop=Math.round(Math.max(0,Math.min(next,box.scrollHeight-state.client)));},
  getBoundingClientRect:()=>({top:state.top,bottom:state.top+state.client}),
 };
 const scrolledIntoView=vi.fn();
 const row=(key:string)=>({scrollIntoView:scrolledIntoView,getBoundingClientRect:()=>{const top=state.top+offsetOf(key)-state.scrollTop;return {top,bottom:top+state.height(key,state.width)};}});
 /** Where a row sits relative to the scroller's top edge, or undefined when it is not drawn. */
 const offset=(key:string)=>state.keys.includes(key)?row(key).getBoundingClientRect().top-state.top:undefined;
 const visible=(key:string)=>{const at=offset(key);return at!==undefined && at>=0 && at+state.height(key,state.width)<=state.client+0.5;};
 const atBottom=()=>state.scrollTop+state.client>=box.scrollHeight-1;
 return {state,box,offset,visible,atBottom,scrolledIntoView,createNodeMock:(element:{props:Record<string,unknown>})=>element.props.className==="log-scroll"?box:typeof element.props["data-item-key"]==="string"?row(element.props["data-item-key"]):null};
}
/** Rows land within one device pixel of their anchor; rounding never accumulates across samples. */
const expectPlaced=(actual:number|undefined,expected:number)=>{expect(actual).toBeDefined();expect(Math.abs(actual!-expected)).toBeLessThanOrEqual(1);};
const observers:(()=>void)[]=[];
beforeEach(()=>{observers.length=0;vi.stubGlobal("ResizeObserver",class{constructor(private callback:()=>void){observers.push(()=>this.callback());}observe(){}disconnect(){}});});
afterEach(()=>{vi.unstubAllGlobals();});
const resized=()=>act(()=>observers.forEach(callback=>callback()));

function mount(initial:StoredValue,{mode="expanded",identity="run-a"}:{mode?:string;identity?:string}={}) {
 const sim=geometry();sim.state.keys=logRows(initial).map(row=>row.key);
 let tree!:ReactTestRenderer;
 act(()=>{tree=create(<LogView value={initial} mode={mode} identity={identity}/>,{createNodeMock:sim.createNodeMock});});
 let time=1000;
 const scroller=()=>tree.root.findByProps({className:"log-scroll"});
 const button=(label:string)=>tree.root.findAll(node=>node.type==="button" && node.children[0]===label)[0] as ReactTestInstance;
 return {sim,tree,
  update:(next:StoredValue,props:{mode?:string;identity?:string}={})=>{sim.state.keys=logRows(next).map(row=>row.key);act(()=>tree.update(<LogView value={next} mode={props.mode??mode} identity={props.identity??identity}/>));},
  /** A scroll event not caused by the reader: restoration, clamping or layout. */
  programmaticScroll:(to?:number)=>{time+=5000;if(to!==undefined)sim.state.scrollTop=to;act(()=>scroller().props.onScroll({timeStamp:time}));},
  wheel:(deltaY:number)=>{time+=16;act(()=>scroller().props.onWheel({deltaY,timeStamp:time}));sim.box.scrollTop=sim.state.scrollTop+deltaY;time+=16;act(()=>scroller().props.onScroll({timeStamp:time}));},
  drag:(to:number)=>{time+=16;act(()=>scroller().props.onPointerDown({button:0,timeStamp:time}));sim.box.scrollTop=to;time+=16;act(()=>scroller().props.onScroll({timeStamp:time}));act(()=>scroller().props.onPointerUp({timeStamp:time}));},
  touch:(by:number)=>{time+=16;act(()=>scroller().props.onTouchStart({timeStamp:time}));sim.box.scrollTop=sim.state.scrollTop+by;time+=16;act(()=>scroller().props.onScroll({timeStamp:time}));},
  key:(name:string,by:number)=>{time+=16;act(()=>scroller().props.onKeyDown({key:name,shiftKey:false,timeStamp:time}));sim.box.scrollTop=sim.state.scrollTop+by;time+=16;act(()=>scroller().props.onScroll({timeStamp:time}));},
  click:(label:string)=>act(()=>button(label).props.onClick()),
  following:()=>button("follow").props["aria-pressed"] as boolean,
  reading:()=>button("reading").props["aria-pressed"] as boolean,
  notice:()=>tree.root.findAll(node=>node.props.role==="status" && String(node.props.className).includes("log-notice")).map(node=>node.children.join("")),
 };
}

it("uses declared log type and exact sequence identity, preserves received time and decoder flags",()=>{
 expect(isLogValue(value)).toBe(true);expect(isLogValue({...value,type:{kind:"unknown"}})).toBe(false);
 const rows=logRows(value);expect(rows.map(row=>row.key)).toEqual(['["synthetic","1"]','["synthetic","2"]']);expect(rows[0]?.received).toBe(true);expect(rows[1]?.received).toBe(false);expect(rows[0]?.flags).toEqual(["partial","truncated"]);
 let tree!:ReturnType<typeof create>;act(()=>{tree=create(<LogView value={value} mode="expanded"/>);});
 expect(tree.root.findAllByProps({"data-item-key":'["synthetic","1"]'})).toHaveLength(1);
 act(()=>tree.root.findByProps({"aria-label":"Log stream"}).props.onChange({target:{value:"stderr"}}));
 expect(tree.root.findAllByProps({"data-item-key":'["synthetic","2"]'})).toHaveLength(0);expect(tree.root.findByProps({className:"mono-dim log-counts"}).children.join("")).toBe("showing 1 of 2 events");act(()=>tree.unmount());
});

it("follows on open and keeps the newest row visible through appends, rolling eviction and source-caused scroll events",()=>{
 const log=mount(window(0,40));
 expect(log.following()).toBe(true);expect(log.reading()).toBe(false);
 expect(log.sim.visible(key(39))).toBe(true);
 log.update(window(0,45));expect(log.sim.visible(key(44))).toBe(true);
 for(let end=500;end<=560;end+=7){log.update(window(end-500,end));log.programmaticScroll();expect(log.sim.visible(key(end-1))).toBe(true);}
 // A clamp or layout scroll that lands elsewhere is not the reader's; follow stays and the next sample returns to the tail.
 log.programmaticScroll(200);expect(log.following()).toBe(true);
 log.update(window(61,561));expect(log.sim.visible(key(560))).toBe(true);expect(log.sim.atBottom()).toBe(true);
 act(()=>log.tree.unmount());
});

it("a click inside the log without scrolling does not turn later source scrolling into a reader gesture",()=>{
 const log=mount(window(0,60));
 act(()=>log.tree.root.findByProps({className:"log-scroll"}).props.onPointerDown({button:0,timeStamp:10}));
 act(()=>log.tree.root.findByProps({className:"log-scroll"}).props.onPointerUp({timeStamp:20}));
 log.update(window(5,65));log.programmaticScroll(300);
 expect(log.following()).toBe(true);log.update(window(6,66));expect(log.sim.visible(key(65))).toBe(true);
 act(()=>log.tree.unmount());
});

it("keeps following the visible tail through filter changes, resizing and wrapping",()=>{
 const log=mount(window(0,200));
 act(()=>log.tree.root.findByProps({"aria-label":"Filter log messages"}).props.onChange({target:{value:"checkpoint"}}));
 log.sim.state.keys=logRows(window(0,200)).filter(row=>row.text.includes("checkpoint")).map(row=>row.key);resized();
 expect(log.sim.visible(key(195))).toBe(true);
 act(()=>log.tree.root.findByProps({"aria-label":"Filter log messages"}).props.onChange({target:{value:""}}));
 log.sim.state.keys=logRows(window(0,200)).map(row=>row.key);resized();
 log.sim.state.client=260;resized();expect(log.sim.visible(key(199))).toBe(true);
 log.sim.state.width=200;log.sim.state.height=(row,width)=>width<300 && row.endsWith('9"]')?39:19.5;resized();
 expect(log.sim.visible(key(199))).toBe(true);expect(log.following()).toBe(true);
 act(()=>log.tree.unmount());
});

it("a wheel up switches to reading and keeps the read row at its pixel offset through a rolling window",()=>{
 const log=mount(window(0,500));
 log.wheel(-400);
 expect(log.following()).toBe(false);expect(log.reading()).toBe(true);
 const anchored=logRows(window(0,500)).map(row=>row.key).find(row=>(log.sim.offset(row) ?? -1)+19.5>0)!;
 const before=log.sim.offset(anchored)!;
 for(let end=501;end<=530;end++){log.update(window(end-500,end));log.programmaticScroll();}
 expectPlaced(log.sim.offset(anchored),before);
 expect(log.reading()).toBe(true);
 act(()=>log.tree.unmount());
});

it("restoration ignores scroll events it did not receive from the reader, so a drifted position is never recaptured",()=>{
 const log=mount(window(0,300));
 log.wheel(-600);
 const anchored=logRows(window(0,300)).map(row=>row.key).find(row=>(log.sim.offset(row) ?? -1)+19.5>0)!;
 const before=log.sim.offset(anchored)!;
 // Something other than the reader moves the box (layout clamp, a focus jump): it must not become the anchor.
 log.programmaticScroll(log.sim.state.scrollTop-137);
 log.update(window(3,303));
 expectPlaced(log.sim.offset(anchored),before);
 act(()=>log.tree.unmount());
});

it("keeps the read row across wrapping and box resizes",()=>{
 const log=mount(window(0,200));
 log.drag(1200);expect(log.reading()).toBe(true);
 const anchored=logRows(window(0,200)).map(row=>row.key).find(row=>(log.sim.offset(row) ?? -1)+19.5>0)!;
 const before=log.sim.offset(anchored)!;
 log.sim.state.width=180;log.sim.state.height=(_row,width)=>width<300?39:19.5;resized();
 expectPlaced(log.sim.offset(anchored),before);
 log.sim.state.client=40;resized();expectPlaced(log.sim.offset(anchored),before);
 log.update(window(2,202));expectPlaced(log.sim.offset(anchored),before);
 act(()=>log.tree.unmount());
});

it("scrollbar drag, touch and scroll keys away from the tail switch to reading; resize and new rows never do",()=>{
 for(const gesture of ["drag","touch","key"] as const) {
  const log=mount(window(0,120));
  log.sim.state.client=180;resized();log.update(window(1,121));expect(log.following()).toBe(true);
  if(gesture==="drag")log.drag(400);else if(gesture==="touch")log.touch(-200);else log.key("PageUp",-160);
  expect(log.reading()).toBe(true);
  act(()=>log.tree.unmount());
 }
});

it("switching to reading keeps the current place; returning to follow jumps to the newest row",()=>{
 const log=mount(window(0,100));
 log.click("reading");expect(log.reading()).toBe(true);
 const anchored=logRows(window(0,100)).map(row=>row.key).find(row=>(log.sim.offset(row) ?? -1)+19.5>0)!;
 const before=log.sim.offset(anchored)!;
 log.update(window(0,140));expectPlaced(log.sim.offset(anchored),before);expect(log.sim.visible(key(139))).toBe(false);
 log.click("follow");expect(log.following()).toBe(true);expect(log.sim.visible(key(139))).toBe(true);
 log.update(window(0,150));expect(log.sim.visible(key(149))).toBe(true);
 act(()=>log.tree.unmount());
});

it("an evicted anchor shows the honest notice at the oldest row and stays in reading until follow is chosen",()=>{
 const log=mount(window(0,500));
 log.wheel(-9000);expect(log.reading()).toBe(true);
 log.update(window(100,600));
 expect(log.notice()).toEqual(["The anchored row left this source window; showing the oldest available row."]);
 expect(log.sim.state.scrollTop).toBe(0);expect(log.reading()).toBe(true);
 log.update(window(103,603));expect(log.reading()).toBe(true);expect(log.sim.visible(key(602))).toBe(false);
 log.click("follow");expect(log.notice()).toEqual([]);expect(log.sim.visible(key(602))).toBe(true);
 act(()=>log.tree.unmount());
});

it("find moves within the log scroller only, switches to reading and holds the match in place",()=>{
 const log=mount(window(0,300));
 act(()=>log.tree.root.findByProps({"aria-label":"Find in log"}).props.onChange({target:{value:"event 120"}}));
 act(()=>log.tree.root.findByProps({"aria-label":"Next log match"}).props.onClick());
 expect(log.reading()).toBe(true);
 const shown=log.tree.root.findAll(node=>typeof node.props.className==="string" && node.props.className.includes("log-match") && node.props["data-item-key"]!==undefined);
 expect(shown).toHaveLength(1);const matched=shown[0]!.props["data-item-key"] as string;
 expect(log.sim.visible(matched)).toBe(true);expect(log.sim.scrolledIntoView).not.toHaveBeenCalled();
 const before=log.sim.offset(matched)!;
 log.update(window(5,305));expectPlaced(log.sim.offset(matched),before);
 act(()=>log.tree.unmount());
});

it("a new run identity resets to follow, clears notices and keeps the filter text",()=>{
 const log=mount(window(0,500));
 act(()=>log.tree.root.findByProps({"aria-label":"Filter log messages"}).props.onChange({target:{value:"event"}}));
 log.wheel(-9000);log.update(window(200,700));expect(log.notice()).toHaveLength(1);
 log.update(window(0,30),{identity:"run-b"});
 expect(log.following()).toBe(true);expect(log.notice()).toEqual([]);expect(log.sim.visible(key(29))).toBe(true);
 expect(log.tree.root.findByProps({"aria-label":"Filter log messages"}).props.value).toBe("event");
 act(()=>log.tree.unmount());
});

it("a preview offers the same follow and reading controls, so a reader can always return to the newest row",()=>{
 const log=mount(window(0,80),{mode:"preview"});
 expect(log.tree.root.findAllByProps({"aria-label":"Filter log messages"})).toHaveLength(0);
 const group=log.tree.root.findByProps({role:"group","aria-label":"Log scrolling"});
 expect(group.findAllByType("button").map(button=>button.children[0])).toEqual(["follow","reading"]);
 log.wheel(-300);expect(log.reading()).toBe(true);
 log.update(window(0,90));expect(log.sim.visible(key(89))).toBe(false);
 log.click("follow");expect(log.sim.visible(key(89))).toBe(true);
 log.update(window(0,90),{mode:"expanded"});expect(log.following()).toBe(true);expect(log.sim.visible(key(89))).toBe(true);
 act(()=>log.tree.unmount());
});
