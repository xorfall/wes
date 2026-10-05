import { act, create } from "react-test-renderer";
import { afterEach, expect, it, vi } from "vitest";
import { LiveView } from "./LiveView";
import { DisplayReadError, liveReader, type DisplayRead } from "../live-view-reader";
import type { Engine } from "../engine";
import type { WorkspaceNode } from "../workspace";
const node={id:"n",command:"events watch",state:"ready",streamOutput:true} as WorkspaceNode;
const read=(n:number,run="r1"):DisplayRead=>({value:{type:{kind:"primitive",name:"Int"},data:n,provenance:{}},metadata:{revision:String(n),epochs:[["source",run]],sources:[{node:"source",run,phase:"open"}]}});
afterEach(()=>vi.useRealTimers());
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
