import { act, create } from "react-test-renderer";
import { afterEach, expect, it, vi } from "vitest";
import { LiveView } from "./LiveView";
import { DisplayReadError, liveReader, type DisplayRead } from "../live-view-reader";
import type { Engine } from "../engine";
import type { WorkspaceNode } from "../workspace";
import type { StoredValue } from "../protocol";
import { ExactNumber } from "../exact-json";
import { registryStore } from "../presentation/registry-store";
const node={id:"n",command:"events watch",state:"ready",streamOutput:true} as WorkspaceNode;
const sample=(n:number,value:StoredValue,run="r1"):DisplayRead=>({value,metadata:{revision:String(n),epochs:[["source",run]],sources:[{node:"source",run,phase:"open"}]}});
const read=(n:number,run="r1"):DisplayRead=>sample(n,{type:{kind:"primitive",name:"Int"},data:n,provenance:{}},run);
afterEach(()=>{vi.useRealTimers();registryStore.reset();});
/** A synthetic build log entry: identity is a nested digest and an exact ordinal, declared by name. */
const BUILD_LOG=`version: 1
type: BuildLogLine
applies: [list]
fields:
  source: SourceRef
  ordinal: Int
  text: Text
present:
  kind: log
  mapping:
    key: [source.digest, ordinal]
    text: text
`;
const buildLogType={kind:"list",element:{kind:"record",name:"BuildLogLine",fields:[{name:"source",type:{kind:"record",name:"SourceRef",fields:[]}},{name:"ordinal",type:{kind:"primitive",name:"INT"}},{name:"text",type:{kind:"primitive",name:"TEXT"}}]}} as StoredValue["type"];
const buildLine=(ordinal:number,text=`synthetic line ${ordinal}`)=>({source:{digest:"d1"},ordinal:new ExactNumber(String(ordinal)),text});
const buildLog=(n:number,data:unknown[])=>sample(n,{type:buildLogType,data,provenance:{}});
const lines=(from:number,to:number)=>Array.from({length:to-from},(_,at)=>buildLine(from+at));
/** The live body's capture of a scroll that left its bottom edge, as a reader's scroll of the log would. */
const scrollAway=(tree:ReturnType<typeof create>)=>act(()=>tree.root.find(item=>item.type==="div" && String(item.props.className).startsWith("live-view")).props.onScrollCapture({target:{scrollTop:0,clientHeight:100,scrollHeight:1000}}));
const mountLive=async(engine:Engine)=>{let tree!:ReturnType<typeof create>;await act(async()=>{tree=create(<LiveView engine={engine} generation="g" node={node}/>);await vi.advanceTimersByTimeAsync(100);});return tree;};
const holdLabel=(tree:ReturnType<typeof create>)=>tree.root.findByProps({className:"cell-action stream-hold"}).props["aria-label"] as string;
const messages=(tree:ReturnType<typeof create>)=>tree.root.findAll(item=>item.props.className==="log-message").map(item=>String(item.children[0]));
const NO_IDENTITY="no item identity · reading holds the display";
type Instance=ReturnType<typeof create>["root"];
const textOf=(node:Instance|string):string=>typeof node==="string"?node:node.children.map(textOf).join("");
it("automatically reads visible output, holds the drawn sample, resumes latest and clears authority",async()=>{
 vi.useFakeTimers();let reply=read(1);const liveView=vi.fn(async()=>reply),engine={liveView} as unknown as Engine;let tree!:ReturnType<typeof create>;
 await act(async()=>{tree=create(<LiveView engine={engine} generation="g" node={node}/>);await vi.advanceTimersByTimeAsync(100);});
 expect(liveView).toHaveBeenCalledOnce();
 await act(async()=>{tree.root.findByProps({"aria-label":"Hold displayed snapshot"}).props.onClick();});
 reply=read(2);await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
 expect(liveReader(engine).cellSnapshot("n","g")?.sample.value?.data).toBe(1);
 await act(async()=>{tree.root.findByProps({"aria-label":"Resume live display"}).props.onClick();});
 expect(liveReader(engine).cellSnapshot("n","g")).toBeUndefined();expect(JSON.stringify(tree.toJSON())).toContain('2');
 await act(async()=>{tree.update(<LiveView engine={engine} generation="g" node={{...node,private:true}}/>);});
 const calls=liveView.mock.calls.length;await act(async()=>{await vi.advanceTimersByTimeAsync(1000);});expect(liveView).toHaveBeenCalledTimes(calls);expect(JSON.stringify(tree.toJSON())).toContain("unavailable");await act(async()=>tree.unmount());
});
it("keeps a fitting held snapshot on read budget failure; read again never executes the source",async()=>{
 vi.useFakeTimers();let fail=false;const liveView=vi.fn(async()=>{if(fail)throw new DisplayReadError("display budget exceeded","budget");return read(7);}),engine={liveView} as unknown as Engine;let tree!:ReturnType<typeof create>;
 await act(async()=>{tree=create(<LiveView engine={engine} generation="g" node={node}/>);await vi.advanceTimersByTimeAsync(100);});
 await act(async()=>tree.root.findByProps({"aria-label":"Hold displayed snapshot"}).props.onClick());fail=true;
 await act(async()=>{await vi.advanceTimersByTimeAsync(1000);});expect(liveView).toHaveBeenCalledTimes(2);expect(liveReader(engine).cellSnapshot("n","g")?.sample.value?.data).toBe(7);
 fail=false;await act(async()=>tree.root.findAllByType("button").find(button=>button.children.includes("read again"))!.props.onClick());await act(async()=>{await vi.advanceTimersByTimeAsync(100);});expect(liveView).toHaveBeenCalledTimes(3);await act(async()=>tree.unmount());
});


it("labels each source's counters in its own scope and never invents a multi-source total",async()=>{
 vi.useFakeTimers();
 const counts={windowItems:12,omitted:"4",rejected:"2",accepted:"16"};
 let reply=read(1);reply.metadata!.sources[0]!.counts=counts;
 const liveView=vi.fn(async()=>reply),engine={liveView} as unknown as Engine;let tree!:ReturnType<typeof create>;
 const label=(id:string)=>id==="source"?"$logs":"$edge";
 await act(async()=>{tree=create(<LiveView engine={engine} generation="g" node={node} sourceLabel={label}/>);await vi.advanceTimersByTimeAsync(100);});
 const line=()=>tree.root.findByProps({className:"stream-counts mono-dim"}).children.map(entry=>textOf(entry));
 expect(line()).toEqual(["16 accepted: 12 in window + 4 outside window · 2 rejected"]);
 const help=tree.root.findByProps({className:"stream-counts mono-dim"}).props.title;
 expect(help).toContain("not lost in transit");expect(help).toContain("Rejected items are counted separately and are not accepted");expect(help).toContain("not saved");
 reply={...read(2),metadata:{...read(2).metadata!,epochs:[["source","r1"],["other","r2"]],sources:[{node:"source",run:"r1",phase:"open",counts},{node:"other",run:"r2",phase:"open",counts:{windowItems:5,omitted:"0",rejected:"0",accepted:"5"}}]}};
 await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
 expect(line()).toEqual(["$logs: 16 accepted: 12 in window + 4 outside window · 2 rejected","$edge: 5 accepted · all 5 in window"]);
 // 21 would be an invented sum of two sources' accepted counts.
 expect(JSON.stringify(tree.toJSON())).not.toContain("21");
 await act(async()=>tree.root.findByProps({"aria-label":"Hold displayed snapshot"}).props.onClick());
 await act(async()=>{await vi.advanceTimersByTimeAsync(300);});
 expect(tree.root.findAllByProps({className:"cell-action stream-new"})).toHaveLength(0);
 expect(Object.keys(engine)).toEqual(["liveView"]);
 await act(async()=>tree.unmount());
});
it("counts accepted events since a hold for one source and resumes without executing it",async()=>{
 vi.useFakeTimers();
 let reply=read(1);reply.metadata!.sources[0]!.counts={windowItems:500,omitted:"0",rejected:"0",accepted:"500"};
 const liveView=vi.fn(async()=>reply),engine={liveView} as unknown as Engine;let tree!:ReturnType<typeof create>;
 await act(async()=>{tree=create(<LiveView engine={engine} generation="g" node={node}/>);await vi.advanceTimersByTimeAsync(100);});
 expect(JSON.stringify(tree.toJSON())).toContain("500 accepted · all 500 in window");
 const hold=tree.root.findByProps({"aria-label":"Hold displayed snapshot"});
 expect(hold.props["aria-description"]).toContain("The source keeps running");
 await act(async()=>hold.props.onClick());
 reply=read(2);reply.metadata!.sources[0]!.counts={windowItems:500,omitted:"40",rejected:"1",accepted:"540"};
 await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
 const incoming=tree.root.findByProps({className:"cell-action stream-new"});
 expect(incoming.children.join("")).toBe("↓ 40 source events");
 // The held display keeps its own counts; the newest sample is not shown until resumed.
 expect(JSON.stringify(tree.toJSON())).toContain("500 accepted · all 500 in window");
 await act(async()=>incoming.props.onClick());
 expect(JSON.stringify(tree.toJSON())).toContain("540 accepted: 500 in window + 40 outside window");
 expect(Object.keys(engine)).toEqual(["liveView"]);
 await act(async()=>tree.unmount());
});
it("should_KeepTheCellsHoldIntact_When_ASecondPresentationReadsAndHoldsTheSameStream",async()=>{
 // Arrange
 vi.useFakeTimers();const liveView=vi.fn(async()=>read(3)),engine={liveView} as unknown as Engine;let cell!:ReturnType<typeof create>,pane!:ReturnType<typeof create>;
 await act(async()=>{cell=create(<LiveView engine={engine} generation="g" node={node}/>);await vi.advanceTimersByTimeAsync(100);});
 await act(async()=>cell.root.findByProps({"aria-label":"Hold displayed snapshot"}).props.onClick());
 // Act
 await act(async()=>{pane=create(<LiveView engine={engine} generation="g" node={node} cellHold={false}/>);await vi.advanceTimersByTimeAsync(100);});
 await act(async()=>pane.root.findByProps({"aria-label":"Hold displayed snapshot"}).props.onClick());
 await act(async()=>pane.root.findByProps({"aria-label":"Resume live display"}).props.onClick());
 await act(async()=>pane.unmount());
 // Assert
 expect(liveReader(engine).cellSnapshot("n","g")?.sample.value?.data).toBe(3);
 await act(async()=>cell.unmount());
 expect(liveReader(engine).cellSnapshot("n","g")).toBeUndefined();
});
it("should_KeepShowingNewSamples_When_AReaderScrollsALiveMappedLog",async()=>{
 // Arrange
 vi.useFakeTimers();registryStore.setHome([{name:"build-log.yaml",text:BUILD_LOG}]);
 let reply=buildLog(1,lines(0,3));const engine={liveView:vi.fn(async()=>reply)} as unknown as Engine;
 const tree=await mountLive(engine);
 // Act
 scrollAway(tree);
 reply=buildLog(2,lines(1,4));await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
 // Assert
 expect(holdLabel(tree)).toBe("Hold displayed snapshot");
 expect(liveReader(engine).cellSnapshot("n","g")).toBeUndefined();
 expect(messages(tree)).toEqual(["synthetic line 1","synthetic line 2","synthetic line 3"]);
 expect(JSON.stringify(tree.toJSON())).not.toContain(NO_IDENTITY);
 await act(async()=>tree.unmount());
});
it("should_NeitherHoldNorClaimNoIdentity_When_ALiveLogSampleHasMalformedOrRepeatedKeys",async()=>{
 // Arrange
 vi.useFakeTimers();registryStore.setHome([{name:"build-log.yaml",text:BUILD_LOG}]);
 let reply=buildLog(1,[buildLine(1),{ordinal:new ExactNumber("2"),text:"no source"},buildLine(1,"a repeated key")]);
 const engine={liveView:vi.fn(async()=>reply)} as unknown as Engine;
 const tree=await mountLive(engine);
 // Act
 scrollAway(tree);
 const mapped=JSON.stringify(tree.toJSON());
 const notices=tree.root.findAll(item=>item.props.role==="status" && String(item.props.className).includes("log-notice")).map(item=>item.children.join(""));
 const event=(sequence:number)=>({container:"fixture",sequence,received_at_ns:new ExactNumber("1790955000000000000"),timestamp_ns:null,stream:"stdout",text:`docker line ${sequence}`,partial:false,lossy:false,line_truncated:false});
 reply=sample(2,{type:{kind:"list",element:{kind:"record",name:"DockerLogEvent",fields:[]}},data:[event(1),event(1),event(2)],provenance:{}});
 await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
 scrollAway(tree);
 // Assert
 expect(notices).toEqual(["1 unreadable log event","1 log event repeats a key already shown"]);
 expect(mapped).not.toContain(NO_IDENTITY);
 expect(messages(tree)).toEqual(["docker line 1","docker line 2"]);
 expect(JSON.stringify(tree.toJSON())).not.toContain(NO_IDENTITY);
 expect(holdLabel(tree)).toBe("Hold displayed snapshot");
 await act(async()=>tree.unmount());
});
it("should_FreezeAMappedLogOnManualHoldAndShowTheNewestOnResume_When_SamplesKeepArriving",async()=>{
 // Arrange
 vi.useFakeTimers();registryStore.setHome([{name:"build-log.yaml",text:BUILD_LOG}]);
 let reply=buildLog(1,lines(0,2));const engine={liveView:vi.fn(async()=>reply)} as unknown as Engine;
 const tree=await mountLive(engine);
 // Act
 await act(async()=>tree.root.findByProps({"aria-label":"Hold displayed snapshot"}).props.onClick());
 reply=buildLog(2,lines(0,3));await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
 const held=messages(tree);
 await act(async()=>tree.root.findByProps({"aria-label":"Resume live display"}).props.onClick());
 // Assert
 expect(held).toEqual(["synthetic line 0","synthetic line 1"]);
 expect(messages(tree)).toEqual(["synthetic line 0","synthetic line 1","synthetic line 2"]);
 await act(async()=>tree.unmount());
});
it("should_HoldWithReasonReading_When_AnOrdinaryUnmappedListIsScrolledAwayFromItsEdge",async()=>{
 // Arrange
 vi.useFakeTimers();registryStore.setHome([{name:"build-log.yaml",text:BUILD_LOG}]);
 let reply=sample(1,{type:{kind:"list",element:{kind:"primitive",name:"Int"}},data:[1,2,3],provenance:{}});
 const engine={liveView:vi.fn(async()=>reply)} as unknown as Engine;
 const tree=await mountLive(engine);
 const note=JSON.stringify(tree.toJSON()).includes(NO_IDENTITY);
 // Act
 scrollAway(tree);
 reply=sample(2,{type:{kind:"list",element:{kind:"primitive",name:"Int"}},data:[1,2,3,4],provenance:{}});await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
 // Assert
 expect(note).toBe(true);
 expect(holdLabel(tree)).toBe("Resume live display");
 expect(tree.root.findByProps({className:"stream-snapshot mono-dim"}).props.title).toBe("reading");
 expect(liveReader(engine).cellSnapshot("n","g")?.sample.value?.data).toEqual([1,2,3]);
 await act(async()=>tree.unmount());
});
