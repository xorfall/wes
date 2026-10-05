import { describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { useLayoutEffect } from "react";
import { Cell, describeDependents, FOLD_ROWS, type CellActions, type CellBlock, type CellProps } from "./Cell";
import { MonoLine, lineText } from "./MonoLine";
import { ViewHost } from "./ViewHost";
import { ResultType } from "./ResultType";
const identity=(id:string)=>({id,label:`$${id}`,glyph:"ready" as const});
const block=(id:string,hasValue=true):CellBlock=>({key:id,identity:identity(id),open:true,hasValue,content:<span>{id} value</span>});
const props:CellProps={theme:"keys",state:"default",label:"c",rows:[{segments:[{text:"synthetic > result"}],nodes:[identity("result")]}],verdict:[{segments:[{text:"ok"}],keep:true}],time:"09:14",blocks:[block("result")]};
function draw(extra:Partial<CellProps>={}) {let tree!:ReactTestRenderer;act(()=>{tree=create(<Cell {...props} {...extra}/>);});return tree;}
const key=(tree:ReactTestRenderer,value:string,extra:Record<string,unknown>={})=>act(()=>tree.root.findAllByType("section").find(node=>node.props["data-cell"]==="c")!.props.onKeyDown({key:value,preventDefault(){},stopPropagation(){},...extra}));
const text=(tree:ReactTestRenderer)=>tree.root.findAllByType(MonoLine).map(node=>lineText(node.props.segments)).join("\n");

describe("Ledger cell contract",()=>{
 it("retains the active cell boundary and footer while its type popup owns focus",()=>{
  vi.stubGlobal("document",{addEventListener:vi.fn(),removeEventListener:vi.fn()});
  vi.stubGlobal("window",{addEventListener:vi.fn(),removeEventListener:vi.fn()});
  try {
  const tree=draw({blocks:[{...block("result"),type:{kind:"primitive",name:"INT"}}]});
  act(()=>tree.root.findByProps({"aria-label":"Type of $result"}).props.onClick());
  expect(tree.root.findByProps({"data-cell":"c"}).props.className).toContain("cell-type-open");
  act(()=>tree.root.findByType(ResultType).props.onOpenChange(false));
  expect(tree.root.findByProps({"data-cell":"c"}).props.className).not.toContain("cell-type-open");
  act(()=>tree.unmount());
  } finally {vi.unstubAllGlobals();}
 });
 it("keeps plain result identities inert and selects by frame interaction",()=>{
  const open=vi.fn(),setView=vi.fn();const tree=draw({blocks:[{...block("first"),identity:{id:"first",label:"id1",glyph:"ready"}},block("last")],actions:{open,setView}});
  const name=tree.root.findByProps({className:"result-name","aria-label":"id1, ready"});
  expect(name.type).toBe("span");expect(name.props.onClick).toBeUndefined();
  act(()=>tree.root.findByProps({"data-node":"first"}).props.onClick({metaKey:false}));
  key(tree,"o");expect(open).toHaveBeenCalledWith("first");
  expect(tree.root.findAllByProps({role:"radio"})).toHaveLength(6);
  act(()=>tree.unmount());
 });
 it("routes header and keyboard size to the selected result and retains sibling hosts",()=>{
  const setView=vi.fn(),cycle=vi.fn(),setHeight=vi.fn(),open=vi.fn();
  const tree=draw({blocks:[{...block("first"),view:"expanded",height:18},{...block("last"),view:"collapsed"}],actions:{setView,cycle,setHeight,open}});
  const hosts=tree.root.findAllByType(ViewHost);expect(hosts.map(host=>host.props.mode)).toEqual(["expanded","collapsed"]);
  act(()=>tree.root.findByProps({"aria-label":"collapsed $first"}).props.onClick());
  expect(setView).toHaveBeenCalledWith("collapsed","first");
  key(tree," ");expect(cycle).toHaveBeenCalledWith("first");
  act(()=>hosts[0]!.props.onResize(22));expect(setHeight).toHaveBeenCalledWith(22,"first");
  key(tree,"o");expect(open).toHaveBeenCalledWith("first");
  expect(tree.root.findAllByType(ViewHost)[1]).toBe(hosts[1]);act(()=>tree.unmount());
 });
 it("puts identity in the result header and time in run evidence, with distinct bottom action groups",()=>{
  const tree=draw({actions:{repeat:vi.fn(),open:vi.fn(),cycle:vi.fn()}});
  expect(text(tree)).toContain("✓ $result");
  const run=tree.root.findAllByType("section").find(node=>node.props["data-zone"]==="run")!;
  expect(run.findByType("time").children).toEqual(["09:14"]);
  expect(tree.root.findByProps({"aria-label":"Run actions"}).findAllByType("button").map(node=>node.props["aria-keyshortcuts"])).toEqual(["r"]);
  expect(tree.root.findByProps({"aria-label":"Data actions"}).findAllByType("button").map(node=>node.props["aria-keyshortcuts"])).toEqual(["Space","o"]);
  act(()=>tree.unmount());
 });
 it.each(["keys","controls"] as const)("answers keyboard actions in %s style",theme=>{
  const actions:CellActions={repeat:vi.fn(),open:vi.fn(),json:vi.fn(),edit:vi.fn(),pin:vi.fn(),cycle:vi.fn(),history:vi.fn()};
  const tree=draw({theme,actions});for(const value of ["r","o","v","e","p"," ","h"])key(tree,value);
  for(const action of Object.values(actions))expect(action).toHaveBeenCalledOnce();
  act(()=>tree.unmount());
 });
 it("leaves Space to buttons, inputs, tabs and view controls",()=>{
  const cycle=vi.fn(),tree=draw({actions:{cycle}});
  for(const control of ["button","input","tab"])key(tree," ",{target:{closest:()=>control}});
  expect(cycle).not.toHaveBeenCalled();key(tree," ");expect(cycle).toHaveBeenCalledOnce();act(()=>tree.unmount());
 });
 it("explicitly targets one multi-result value, hides actions of unreadable targets and keeps navigation",()=>{
  const open=vi.fn(),json=vi.fn(),setView=vi.fn(),tree=draw({blocks:[block("first"),block("skipped",false)],actions:{open,json,setView}});
  const group=()=>tree.root.findByProps({"aria-label":"Data actions"});
  expect(group().findAllByProps({"aria-label":"o inspect"})).toHaveLength(0);
  expect(group().findAll(node=>node.children.some(child=>child==="no value"))).toHaveLength(0);
  expect(group().findAllByProps({"aria-label":"Previous result"})).toHaveLength(1);
  const skipped=tree.root.findByProps({"data-node":"skipped"});
  expect(skipped.findAllByProps({className:"cell-action result-inspect"})).toHaveLength(0);
  expect(skipped.findAllByProps({role:"radio"})).toHaveLength(0);
  expect(tree.root.findByProps({"data-node":"first"}).findAllByProps({role:"radio"})).toHaveLength(3);
  key(tree,"o");expect(open).not.toHaveBeenCalled();key(tree,"[");key(tree,"o");key(tree,"v");
  expect(open).toHaveBeenCalledWith("first");expect(json).toHaveBeenCalledWith("first");
  expect(group().findAllByProps({"aria-label":"o inspect"})).toHaveLength(1);
  act(()=>tree.unmount());
 });
 it("cycles previous and next through every sibling, unreadable ones included",()=>{
  const open=vi.fn(),history=vi.fn(),tree=draw({blocks:[block("first"),block("skipped",false),block("last")],actions:{open,history}});
  act(()=>tree.root.findByProps({"aria-label":"Next result"}).props.onClick());key(tree,"o");expect(open).toHaveBeenLastCalledWith("first");
  act(()=>tree.root.findByProps({"aria-label":"Next result"}).props.onClick());key(tree,"o");expect(open).toHaveBeenCalledTimes(1);
  key(tree,"h");expect(history).toHaveBeenLastCalledWith("skipped");
  key(tree,"]");key(tree,"o");expect(open).toHaveBeenLastCalledWith("last");
  key(tree,"[");key(tree,"h");expect(history).toHaveBeenLastCalledWith("skipped");key(tree,"o");expect(open).toHaveBeenCalledTimes(2);
  key(tree,"[");key(tree,"o");expect(open).toHaveBeenLastCalledWith("first");
  act(()=>tree.unmount());
 });
 it("omits the Data group for a single unreadable result instead of drawing disabled controls",()=>{
  const tree=draw({blocks:[block("result",false)],actions:{open:vi.fn(),json:vi.fn(),setView:vi.fn()}});
  expect(tree.root.findAllByProps({"aria-label":"Data actions"})).toHaveLength(0);
  expect(tree.root.findAllByType("button").some(node=>node.props.disabled)).toBe(false);
  act(()=>tree.unmount());
 });
 it("never prefixes an absent primary fact with a separator",()=>{
  const tree=draw({blocks:[{...block("first"),header:[],duration:"94 ms"},{...block("last"),header:[{text:"3 rows"}],duration:"5 ms"}]});
  const facts=tree.root.findAllByProps({className:"result-facts"});
  expect(facts.map(node=>node.props.title)).toEqual(["94 ms","3 rows · 5 ms"]);
  expect(facts[0]!.findByProps({className:"result-duration"}).children.join("")).toBe("94 ms");
  expect(facts[1]!.findByProps({className:"result-duration"}).children.join("")).toBe(" · 5 ms");
  act(()=>tree.unmount());
 });
 it("draws no facts slot when the header is empty and no duration is known",()=>{
  const tree=draw({blocks:[{...block("result"),header:[]}]});
  expect(tree.root.findAllByProps({className:"result-facts"})).toHaveLength(0);
  act(()=>tree.unmount());
 });
 it("folds only the command and keeps every result accessible",()=>{
  const rows=Array.from({length:10},(_,at)=>({segments:[{text:`line ${at}`}],nodes:[identity(String(at))]}));
  const tree=draw({rows,blocks:[block("middle")]});expect(FOLD_ROWS).toBe(4);expect(text(tree)).not.toContain("line 4");
  expect(tree.root.findAllByType("span").some(node=>node.children.join("").includes("middle value"))).toBe(true);
  act(()=>tree.root.findByProps({className:"cell-fold"}).props.onClick());expect(text(tree)).toContain("line 4");act(()=>tree.unmount());
 });
 it("folds a single wrapped command by visual height and remeasures after resizing",()=>{
  let height=220, resize=()=>{};
  vi.stubGlobal("getComputedStyle",()=>({lineHeight:"20px"}));
  vi.stubGlobal("ResizeObserver",class {constructor(callback:()=>void){resize=callback;} observe(){} disconnect(){} });
  const element={get scrollHeight(){return height;}};
  let tree!:ReactTestRenderer;
  try {
    act(()=>{tree=create(<Cell {...props} rows={[{segments:[{text:"very long synthetic command"}],nodes:[]}]}/>,{createNodeMock:node=>node.type==="div" && !node.props.className ? element : null});});
    expect(tree.root.findByProps({className:"cell-fold"}).props["aria-expanded"]).toBe(false);
    act(()=>tree.root.findByProps({className:"cell-fold"}).props.onClick());
    expect(tree.root.findByProps({className:"cell-fold"}).props["aria-expanded"]).toBe(true);
    act(()=>tree.root.findByProps({className:"cell-fold"}).props.onClick());
    height=80;act(()=>resize());expect(tree.root.findAllByProps({className:"cell-fold"})).toHaveLength(0);
    expect(tree.root.findAllByProps({className:"cell-name"})).toHaveLength(0);
    act(()=>tree.unmount());
  } finally {vi.unstubAllGlobals();}
 });
 it("keeps diagnostics in the run zone",()=>{
  const tree=draw({blocks:[{key:"notice",zone:"run",open:true,content:<span>notice</span>},block("result")]});
  const run=tree.root.findAllByType("section").find(node=>node.props["data-zone"]==="run")!;expect(run.findAllByType("span").some(node=>node.children.includes("notice"))).toBe(true);act(()=>tree.unmount());
 });
 it("a guarded repeat requires explicit confirmation and can be dismissed",()=>{
  const repeat=vi.fn(),tree=draw({actions:{repeat},confirmRepeat:{what:"synthetic external operation",dependents:["$a"]}});
  key(tree,"r");expect(repeat).not.toHaveBeenCalled();key(tree,"Escape");expect(repeat).not.toHaveBeenCalled();key(tree,"r");key(tree,"Enter");expect(repeat).toHaveBeenCalledWith(true);act(()=>tree.unmount());
 });
 it("never consumes modified shortcuts",()=>{
  const repeat=vi.fn(),tree=draw({actions:{repeat}});for(const flag of ["metaKey","ctrlKey","altKey"])key(tree,"r",{[flag]:true});expect(repeat).not.toHaveBeenCalled();act(()=>tree.unmount());
 });
 it("reviews deletion once, preserving Shift+D and the authoritative confirmation",async()=>{
  const preview=vi.fn(async()=>({token:"t",cells:["c"],nodes:["result"],dependents:[],labels:{c:"synthetic"},payloads:[],protected:[],sharedWorkspaces:[],expiresInSeconds:120})),confirm=vi.fn();
  const tree=draw({actions:{deleteWork:{preview,confirm}}});key(tree,"D",{shiftKey:false});expect(preview).not.toHaveBeenCalled();
  await act(async()=>{key(tree,"D",{shiftKey:true});});expect(preview).toHaveBeenCalledOnce();expect(confirm).not.toHaveBeenCalled();expect(tree.root.findAllByProps({"aria-label":"Review cell deletion"})).toHaveLength(1);act(()=>tree.unmount());
 });
 it("copies and peeks without executing a command",()=>{
  const peek=vi.fn(),repeat=vi.fn(),tree=draw({actions:{peek,repeat}});act(()=>tree.root.findByProps({className:"cell-source"}).props.onClick({metaKey:true,button:0,preventDefault(){},stopPropagation(){}}));expect(peek).toHaveBeenCalledWith("source");expect(repeat).not.toHaveBeenCalled();act(()=>tree.unmount());
 });
 it("describes a bounded list of dependents",()=>{expect(describeDependents(["$a"])).toBe("1 dependent: $a");expect(describeDependents(["a","b","c","d"])).toBe("4 dependents");});
});

it("hides footers and opens the same guarded actions with Cmd+M",()=>{
 const repeat=vi.fn(),tree=draw({tailKeys:false,actions:{repeat},confirmRepeat:{what:"synthetic effect",dependents:[]}});
 expect(tree.root.findAllByType("footer")).toHaveLength(0);
 key(tree,"m",{metaKey:true});
 expect(tree.root.findByProps({"aria-label":"Cell actions"}).type).toBe("dialog");expect(repeat).not.toHaveBeenCalled();
 act(()=>tree.root.findByProps({"aria-label":"r repeat"}).props.onClick());
 expect(tree.root.findAllByType("dialog")).toHaveLength(0);expect(repeat).not.toHaveBeenCalled();
 act(()=>tree.root.findAllByType("button").find(b=>b.children.includes("confirm repeat"))!.props.onClick());
 expect(repeat).toHaveBeenCalledWith(true);
 key(tree,"m",{ctrlKey:true});act(()=>tree.root.findByType("dialog").props.onCancel({preventDefault(){}}));
 expect(tree.root.findAllByType("dialog")).toHaveLength(0);act(()=>tree.unmount());
});
it.each([["MacIntel","⌘M"],["Win32","⌃M"]])("names the cell actions chord for %s",(platform,chord)=>{
 vi.stubGlobal("navigator",{platform});
 try {
  const tree=draw({tailKeys:false});key(tree,"m",{[platform==="MacIntel"?"metaKey":"ctrlKey"]:true});
  expect(tree.root.findByType("dialog").findByType("header").findByType("kbd").children.join("")).toBe(chord);act(()=>tree.unmount());
 } finally {vi.unstubAllGlobals();}
});
it("retains multi-result targeting in the menu, without repeating a single target",()=>{
 const open=vi.fn(),tree=draw({tailKeys:false,blocks:[block("first"),block("last")],actions:{open}});
 key(tree,"m",{metaKey:true});act(()=>tree.root.findByProps({"aria-label":"Previous result"}).props.onClick());
 act(()=>tree.root.findByProps({"aria-label":"o inspect"}).props.onClick());expect(open).toHaveBeenCalledWith("first");act(()=>tree.unmount());
 const one=draw({actions:{open}});expect(one.root.findAllByProps({className:"cell-target"})).toHaveLength(0);act(()=>one.unmount());
});
describe("run action availability",()=>{
 const pending={id:"result",label:"$result",glyph:"pending" as const};
 const runLabels=(tree:ReactTestRenderer)=>tree.root.findByProps({"aria-label":"Run actions"}).findAllByType("button").map(node=>node.props["aria-label"]);
 const all=():CellActions=>({repeat:vi.fn(),branch:vi.fn(),cancel:vi.fn(),follow:vi.fn(),edit:vi.fn(),pin:vi.fn()});
 it("never repeats waiting work that is not live, while cancel, edit and pin remain",()=>{
  const actions=all(),tree=draw({state:"default",rows:[{segments:[{text:"synthetic"}],nodes:[pending]}],verdict:[{segments:[{text:"waiting"}],keep:true}],
   blocks:[{...block("result",false),identity:pending}],actions,confirmRepeat:{what:"synthetic effect",dependents:[]}});
  expect(runLabels(tree)).toEqual(["x cancel","e edit","p pin"]);
  for(const value of ["r","b","f"])key(tree,value);
  expect(actions.repeat).not.toHaveBeenCalled();expect(actions.branch).not.toHaveBeenCalled();expect(actions.follow).not.toHaveBeenCalled();
  expect(tree.root.findAllByProps({className:"cell-confirm"})).toHaveLength(0);
  key(tree,"x");key(tree,"e");key(tree,"p");
  expect(actions.cancel).toHaveBeenCalledOnce();expect(actions.edit).toHaveBeenCalledOnce();expect(actions.pin).toHaveBeenCalledOnce();
  key(tree,"m",{metaKey:true});
  expect(tree.root.findByProps({"aria-label":"Cell actions"}).findAllByProps({"aria-keyshortcuts":"r"})).toHaveLength(0);
  act(()=>tree.unmount());
 });
 it("offers cancel and follow for running work and no repeat in footer, menu or keys",()=>{
  const actions=all(),running={...pending,glyph:"running" as const};
  const tree=draw({state:"live",streamOutput:true,streamSource:true,rows:[{segments:[{text:"synthetic"}],nodes:[running]}],verdict:[{segments:[{text:"running"}],keep:true}],blocks:[{...block("result"),identity:running}],actions});
  expect(runLabels(tree)).toEqual(["x stop source","f follow","e edit","p pin"]);
  const stop=tree.root.findByProps({"aria-label":"x stop source"});
  expect(stop.props["aria-description"]).toMatch(/source run.*Holding or closing the display does not stop it/);
  key(tree,"r");expect(actions.repeat).not.toHaveBeenCalled();key(tree,"f");expect(actions.follow).toHaveBeenCalledOnce();
  act(()=>tree.unmount());
 });
 it("withdraws an open repeat confirmation once the work starts waiting",()=>{
  const repeat=vi.fn(),guard={what:"synthetic effect",dependents:[]};
  const tree=draw({actions:{repeat},confirmRepeat:guard});key(tree,"r");
  expect(tree.root.findAllByProps({className:"cell-confirm"})).toHaveLength(1);
  act(()=>tree.update(<Cell {...props} actions={{repeat}} confirmRepeat={guard} rows={[{segments:[{text:"synthetic"}],nodes:[pending]}]}/>));
  expect(tree.root.findAllByProps({className:"cell-confirm"})).toHaveLength(0);
  key(tree,"Enter");expect(repeat).not.toHaveBeenCalled();
  act(()=>tree.unmount());
 });
 it("says run for a cell that was not run, and retry when a refused repeat shows previous results",()=>{
  const notRun=[{segments:[{text:"not run"}],keep:true}];
  const fresh=draw({state:"failed",verdict:notRun,blocks:[],actions:{repeat:vi.fn()},confirmRepeat:{what:"synthetic effect",dependents:[]}});
  expect(runLabels(fresh)).toEqual(["r run"]);
  key(fresh,"r");expect(fresh.root.findAllByType("button").some(node=>node.children.includes("confirm run"))).toBe(true);
  expect(text(fresh)).toContain("▶ run synthetic effect");expect(text(fresh)).not.toContain("again");
  act(()=>fresh.unmount());
  const refused=draw({state:"failed",verdict:[...notRun,{segments:[{text:"previous results shown"}],keep:true}],actions:{repeat:vi.fn()}});
  expect(runLabels(refused)).toEqual(["r retry"]);act(()=>refused.unmount());
 });
});

describe("failed cell copy",()=>{
 it("names the copy by what its callback copies, the submitted source",()=>{
  const copy=vi.fn(),tree=draw({state:"failed",verdict:[{segments:[{text:"failed"}],keep:true}],actions:{copy}});
  const chip=tree.root.findByProps({"aria-label":"Run actions"}).findAllByType("button").find(node=>node.props["aria-keyshortcuts"]==="c")!;
  expect(chip.props["aria-label"]).toBe("c copy source");expect(chip.props.title).toMatch(/submitted source/);
  expect(text(tree)+chip.props["aria-label"]).not.toMatch(/copy error/);
  key(tree,"c");expect(copy).toHaveBeenCalledOnce();act(()=>tree.unmount());
 });
});

describe("cancel target",()=>{
 const running=(id:string)=>({id,label:`$${id}`,glyph:"running" as const});
 const cancelOf=(extra:Partial<CellProps>,nodes=[running("result")])=>{
  const tree=draw({state:"live",rows:[{segments:[{text:"synthetic"}],nodes}],verdict:[{segments:[{text:"running"}],keep:true}],
   blocks:nodes.map(node=>({...block(node.id),identity:node})),actions:{cancel:vi.fn()},...extra});
  const chip=tree.root.findByProps({"aria-keyshortcuts":"x"});const shown={label:chip.props["aria-label"],description:chip.props["aria-description"]};act(()=>tree.unmount());return shown;
 };
 it("does not call a finite refresh beside an old value a stream",()=>{
  expect(cancelOf({}).label).toBe("x cancel");
 });
 it("names a stream source only when the engine says this cell's node is one",()=>{
  expect(cancelOf({streamOutput:true,streamSource:true})).toEqual({label:"x stop source",description:expect.stringContaining("Holding or closing the display does not stop it")});
  const consumer=cancelOf({streamOutput:true});
  expect(consumer.label).toBe("x stop");expect(consumer.description).toContain("A stream source in another cell is not cancelled");
 });
 it("says a pipeline's cancel targets every stage, not one source",()=>{
  const nodes=[running("source"),running("parsed")];
  const stream=cancelOf({pipeline:true,streamOutput:true,streamSource:true},nodes);
  expect(stream.label).toBe("x stop pipeline");expect(stream.description).toContain("all 2 stages of this cell, including its stream source");
  expect(cancelOf({pipeline:true},nodes)).toEqual({label:"x cancel pipeline",description:"Ask the engine to cancel all 2 stages of this cell."});
 });
});

describe("cancellation requests",()=>{
 const running={id:"result",label:"$result",glyph:"running" as const};
 const live=(cancel:CellActions["cancel"],glyph:"running"|"cancelled"="running",attempt="attempt-1")=>({state:glyph==="running"?"live" as const:"failed" as const,attempt,outputIdentity:"g1:synthetic",
  rows:[{segments:[{text:"synthetic"}],nodes:[{...running,glyph}]}],verdict:[{segments:[{text:glyph}],keep:true}],blocks:[{...block("result",false),identity:{...running,glyph}}],actions:{cancel}});
 const deferred=()=>{let resolve!:()=>void,reject!:(error:Error)=>void;const promise=new Promise<void>((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};};
 const status=(tree:ReactTestRenderer)=>tree.root.findAllByProps({className:"cell-cancel-status mono-warn"}).map(node=>node.children.join(""));
 it("says cancel requested only after acknowledgement, sends once, and leaves the outcome to the node state",async()=>{
  let acknowledge!:()=>void;const cancel=vi.fn(()=>new Promise<void>(resolve=>{acknowledge=resolve;}));
  const tree=draw(live(cancel));
  key(tree,"x");key(tree,"x");
  expect(cancel).toHaveBeenCalledOnce();expect(status(tree)).toEqual(["requesting cancel…"]);
  expect(tree.root.findByProps({"aria-label":"x cancel"}).props.disabled).toBe(true);
  await act(async()=>acknowledge());
  expect(status(tree)).toEqual(["cancel requested"]);expect(text(tree)).not.toMatch(/\bcancelled\b/);
  act(()=>tree.update(<Cell {...props} {...live(cancel,"cancelled")}/>));
  expect(status(tree)).toEqual([]);expect(cancel).toHaveBeenCalledOnce();
  act(()=>tree.unmount());
 });
 it("clears the request when the engine refuses it, so it can be asked again",async()=>{
  const cancel=vi.fn(()=>Promise.reject(new Error("synthetic refusal")));
  const tree=draw(live(cancel));
  await act(async()=>{key(tree,"x");await Promise.resolve();});
  expect(status(tree)).toEqual([]);
  await act(async()=>{key(tree,"x");await Promise.resolve();});
  expect(cancel).toHaveBeenCalledTimes(2);act(()=>tree.unmount());
 });
 it("never shows an old attempt's acknowledgement on a newer running attempt",async()=>{
  const old=deferred(),cancel=vi.fn(()=>old.promise);
  const tree=draw(live(cancel));key(tree,"x");expect(status(tree)).toEqual(["requesting cancel…"]);
  act(()=>tree.update(<Cell {...props} {...live(cancel,"running","attempt-2")}/>));
  expect(status(tree)).toEqual([]);expect(tree.root.findByProps({"aria-label":"x cancel"}).props.disabled).toBe(false);
  await act(async()=>old.resolve());
  expect(status(tree)).toEqual([]);act(()=>tree.unmount());
 });
 it("never draws an earlier attempt's request on the first render of its replacement",()=>{
  // Records what every committed render showed, including the one before effects run.
  const seen:{status:string[];disabled:boolean}[]=[];
  const Probe=(cellProps:CellProps)=><><Cell {...cellProps}/><Recorder/></>;
  let tree!:ReactTestRenderer;
  function Recorder(){
   useLayoutEffect(()=>{if(!tree)return;const chip=tree.root.findByProps({"aria-keyshortcuts":"x"});seen.push({status:status(tree),disabled:chip.props.disabled});});
   return null;
  }
  const cancel=vi.fn(()=>new Promise<void>(()=>{}));
  act(()=>{tree=create(<Probe {...props} {...live(cancel)}/>);});
  key(tree,"x");expect(status(tree)).toEqual(["requesting cancel…"]);
  seen.length=0;
  act(()=>tree.update(<Probe {...props} {...live(cancel,"running","attempt-2")}/>));
  expect(seen.length).toBeGreaterThan(0);
  expect(seen.every(render=>render.status.length===0 && !render.disabled)).toBe(true);
  key(tree,"x");expect(cancel).toHaveBeenCalledTimes(2);expect(status(tree)).toEqual(["requesting cancel…"]);
  act(()=>tree.unmount());
 });
 it("ignores an old refusal that arrives after a newer request",async()=>{
  const old=deferred(),newer=deferred(),cancel=vi.fn().mockReturnValueOnce(old.promise).mockReturnValueOnce(newer.promise);
  const tree=draw(live(cancel));key(tree,"x");
  act(()=>tree.update(<Cell {...props} {...live(cancel,"running","attempt-2")}/>));key(tree,"x");
  expect(cancel).toHaveBeenCalledTimes(2);
  await act(async()=>old.reject(new Error("synthetic stale refusal")));
  expect(status(tree)).toEqual(["requesting cancel…"]);
  await act(async()=>newer.resolve());expect(status(tree)).toEqual(["cancel requested"]);
  act(()=>tree.unmount());
 });
 it("retires a request when the generation changes under the same attempt name",async()=>{
  const old=deferred(),tree=draw(live(vi.fn(()=>old.promise)));key(tree,"x");
  act(()=>tree.update(<Cell {...props} {...live(vi.fn(),"running")} outputIdentity="g2:synthetic"/>));
  await act(async()=>old.resolve());expect(status(tree)).toEqual([]);act(()=>tree.unmount());
 });
 it("contains a synchronous throw and ignores a response after unmount",async()=>{
  const throwing=draw(live(vi.fn(()=>{throw new Error("synthetic");})));
  expect(()=>key(throwing,"x")).not.toThrow();expect(status(throwing)).toEqual([]);
  expect(throwing.root.findByProps({"aria-label":"x cancel"}).props.disabled).toBe(false);act(()=>throwing.unmount());
  const late=deferred(),tree=draw(live(vi.fn(()=>late.promise)));key(tree,"x");act(()=>tree.unmount());
  await act(async()=>late.resolve());
 });
 it("shows no acknowledgement without an attempt identity, but still sends the request",()=>{
  const cancel=vi.fn(async()=>{}),tree=draw({...live(cancel),attempt:undefined});
  key(tree,"x");expect(cancel).toHaveBeenCalledOnce();expect(status(tree)).toEqual([]);act(()=>tree.unmount());
 });
});

describe("unknown outcome",()=>{
 const unknown={id:"result",label:"$result",glyph:"failed" as const};
 const doubt=(extra:Partial<CellProps>={})=>draw({state:"failed",rows:[{segments:[{text:"synthetic remote write"}],nodes:[unknown]}],
  verdict:[{segments:[{text:"outcome unknown"}],keep:true}],blocks:[{...block("result",false),identity:unknown}],...extra});
 const runLabels=(tree:ReactTestRenderer)=>tree.root.findByProps({"aria-label":"Run actions"}).findAllByType("button").map(node=>node.props["aria-label"]);
 it("offers review before repeat; review opens recorded details and never runs anything",()=>{
  const repeat=vi.fn(),details=vi.fn(),tree=doubt({actions:{repeat,details},confirmRepeat:{what:"synthetic remote write",against:"SYNTHETIC",dependents:[],unknownOutcome:true}});
  expect(runLabels(tree)).toEqual(["d review outcome","r repeat…"]);
  expect(runLabels(tree)).not.toContain("d details");
  key(tree,"d");act(()=>tree.root.findByProps({"aria-label":"d review outcome"}).props.onClick());
  expect(details).toHaveBeenCalledTimes(2);expect(details).toHaveBeenCalledWith("result");expect(repeat).not.toHaveBeenCalled();
  key(tree,"r");expect(repeat).not.toHaveBeenCalled();
  expect(text(tree)).toContain("the previous attempt's outcome is unknown");
  key(tree,"Escape");expect(repeat).not.toHaveBeenCalled();
  key(tree,"r");key(tree,"Enter");expect(repeat).toHaveBeenCalledOnce();expect(repeat).toHaveBeenCalledWith(true);
  act(()=>tree.unmount());
 });
 it("does not promise a question the engine does not require, and repeats only when asked",()=>{
  const repeat=vi.fn(),tree=doubt({actions:{repeat,details:vi.fn()}});
  expect(runLabels(tree)).toEqual(["d review outcome","r repeat"]);
  expect(tree.root.findByProps({"aria-label":"r repeat"}).props["aria-description"]).toContain("previous outcome stays unknown");
  expect(repeat).not.toHaveBeenCalled();key(tree,"r");expect(repeat).toHaveBeenCalledOnce();expect(repeat).toHaveBeenCalledWith(false);
  act(()=>tree.unmount());
 });
});

it("leaves Cmd+M to editors",()=>{
 const tree=draw({tailKeys:false});key(tree,"m",{metaKey:true,target:{closest:()=>"textarea"}});
 expect(tree.root.findAllByType("dialog")).toHaveLength(0);act(()=>tree.unmount());
});
