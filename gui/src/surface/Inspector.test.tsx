import { afterEach,expect,it,vi } from "vitest";
import { act,create,type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";
import { emptyWorkspace,type WorkspaceNode } from "../workspace";
import { newCell } from "../cells";
import { readSession } from "./session-model";
import { Inspector,snapshotLabel } from "./Inspector";
import { resultAccess } from "./result-access";
const nodes:WorkspaceNode[]=[{id:"n",name:"sample",command:"synthetic",dependsOn:[],state:"ready",handle:"h",kept:true,provenance:{},cautions:[]}];
const stored:StoredValue={type:{kind:"unknown"},data:{message:"authorized synthetic value"},provenance:{}};
const trees:ReactTestRenderer[]=[];
afterEach(()=>{act(()=>trees.splice(0).forEach(tree=>tree.unmount()));vi.useRealTimers();});
function fixture(over:Partial<WorkspaceNode>={}) {
 const api={fetch:vi.fn(async()=>stored),liveView:vi.fn(async()=>stored)};
 const workspace={...emptyWorkspace,nodes:[{...nodes[0]!,...over}]};
 const cell=readSession({workspace,cells:[{...newCell("synthetic"),id:"c",state:"answered",nodes:["n"]}],context:{workspace:"qa",connection:"connected"}}).cells[0]!;
 const props={engine:api as unknown as Engine,workspace,generation:"g",cell,selection:{cell:"c",node:"n",tab:"inspect" as const},active:true,onTab:vi.fn(),onClose:vi.fn(),onWindow:vi.fn(),overlay:false};
 return {api,props};
}
it("honors memory-only finite reads while refusing private live sampling and uncertain results",async()=>{
 const {api,props}=fixture({private:true,streamOutput:true});let tree!:ReactTestRenderer;
 await act(async()=>{tree=create(<Inspector {...props}/>);});trees.push(tree);
 expect(api.fetch).toHaveBeenCalledWith("h");expect(api.liveView).not.toHaveBeenCalled();expect(JSON.stringify(tree.toJSON())).toContain("authorized synthetic value");
 expect(resultAccess({...nodes[0]!,private:true,streamOutput:true}).live).toBe(false);
 await act(async()=>tree.update(<Inspector {...props} workspace={{...props.workspace,nodes:[{...props.workspace.nodes[0]!,doubt:{capability:"synthetic",when:"2026-10-02T00:00:00Z",safe:false}}]}}/>));
 expect(JSON.stringify(tree.toJSON())).not.toContain("authorized synthetic value");expect(api.fetch).toHaveBeenCalledTimes(1);
});
it("discards an old read when definition or generation changes",async()=>{
 const {api,props}=fixture();let finish!:(value:StoredValue)=>void;api.fetch.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;}));let tree!:ReactTestRenderer;
 await act(async()=>{tree=create(<Inspector {...props}/>);});trees.push(tree);
 await act(async()=>tree.update(<Inspector {...props} generation="g2" workspace={{...props.workspace,nodes:[{...props.workspace.nodes[0]!,command:"different",handle:undefined}]}}/>));
 await act(async()=>finish(stored));expect(JSON.stringify(tree.toJSON())).not.toContain("authorized synthetic value");
});
it("should_HeadTheInspectorWithTheNominalItemContract_When_TheRecipeIsAnnotated",async()=>{
 // Arrange
 const {api,props}=fixture();
 const recipe:StoredValue={type:{kind:"iter",element:{kind:"primitive",name:"TEXT"}},data:{itemType:"TEXT",itemContract:"SyntheticEntry"},provenance:{}};
 api.fetch.mockResolvedValue(recipe);let tree!:ReactTestRenderer;
 // Act
 await act(async()=>{tree=create(<Inspector {...props}/>);});trees.push(tree);
 // Assert
 const heading=tree.root.findByProps({className:"inspector-title"}).findByType("span");
 expect(heading.children.join("")).toBe("Iter<SyntheticEntry>");
});
it("labels snapshots with client evidence and excludes failed or incomplete outcomes",()=>{
 expect(snapshotLabel({at:"12:30",revision:4},true,true)).toContain("client sample 4 · stream stopped · the stream's last value is newer");
 expect(resultAccess({...nodes[0]!,state:"failed"}).current).toBe(false);
 expect(resultAccess({...nodes[0]!,state:"cancelled"}).current).toBe(false);
 expect(resultAccess({...nodes[0]!,state:"cancelled",stopped:{source:"n",run:"r"}}).current).toBe(true);
});
it("should_OfferLiveSamplingAndSayNewerInputWaits_When_ACalculationIsBehindItsInput",async()=>{
 // Arrange
 const behind={...nodes[0]!,state:"stale" as const,streamOutput:true,staleReason:{code:"input_behind",message:"Behind."}};
 const {props}=fixture({state:"running",updatePending:true});let tree!:ReactTestRenderer;
 // Act
 await act(async()=>{tree=create(<Inspector {...props}/>);});trees.push(tree);
 // Assert
 expect(resultAccess(behind).live).toBe(true);
 expect(resultAccess({...behind,staleReason:{code:"dependency_changed",message:"Changed."}}).live).toBe(false);
 const status=tree.root.findByProps({className:"inspector-live"}).findAllByType("span").map(span=>span.children.join(""));
 expect(status).toEqual(["updating · previous result","Newer input waiting; finishing current calculation"]);
});
it("should_KeepTheUpdatePendingStatusBesideLiveAndHeldLabels_When_AStreamDerivedCalculationRuns",async()=>{
 // Arrange
 vi.useFakeTimers();const {api,props}=fixture({state:"running",streamOutput:true,updatePending:true});
 const metadata={revision:"1",epochs:[["source","r1"]] as [string,string][],sources:[{node:"source",run:"r1",phase:"open" as const}]};
 api.liveView.mockImplementation(async()=>({value:stored,metadata}) as never);
 const {liveReader}=await import("../live-view-reader");liveReader(props.engine).holdCell("n","g",{sample:{value:stored,revision:1,metadata},at:"12:00"});
 const status=()=>tree.root.findByProps({className:"inspector-live"}).findAllByType("span").map(span=>span.children.join(""));
 let tree!:ReactTestRenderer;
 // Act
 await act(async()=>{tree=create(<Inspector {...props}/>);});trees.push(tree);
 const held=status();
 act(()=>tree.root.findAllByType("button").find(button=>button.children.includes("follow live"))!.props.onClick());
 await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
 const following=status();
 // Assert
 expect(held).toEqual(["snapshot from the cell’s hold at 12:00","Newer input waiting; finishing current calculation"]);
 expect(following).toEqual(["live · following","Newer input waiting; finishing current calculation"]);
});
it("seeds an independent inspector from the cell hold and discards every copy on withdrawal",async()=>{
 vi.useFakeTimers();const {api,props}=fixture({streamOutput:true});
 const metadata={revision:"1",epochs:[["source","r1"]] as [string,string][],sources:[{node:"source",run:"r1",phase:"open" as const}]};
 api.liveView.mockImplementation(async()=>({value:stored,metadata}) as never);
 const {liveReader,DisplayReadError}=await import("../live-view-reader");const owner=liveReader(props.engine);
 owner.holdCell("n","g",{sample:{value:stored,revision:1,metadata},at:"12:00"});
 let tree!:ReactTestRenderer;await act(async()=>{tree=create(<Inspector {...props}/>);});trees.push(tree);
 expect(JSON.stringify(tree.toJSON())).toContain("snapshot from the cell");
 await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
 act(()=>tree.root.findAllByType("button").find(button=>button.children.includes("follow live"))!.props.onClick());
 expect(owner.cellSnapshot("n","g")?.sample.value).toBe(stored);
 api.liveView.mockRejectedValue(new DisplayReadError("authority lost","withdrawn"));await act(async()=>{await vi.advanceTimersByTimeAsync(100);});
 expect(JSON.stringify(tree.toJSON())).not.toContain("authorized synthetic value");expect(owner.cellSnapshot("n","g")).toBeUndefined();
});
it("should_ExplainThatRefreshingTheInputDoesNotRecreateTheView_When_ConstructionIsComplete",async()=>{
 // Arrange
 const {props}=fixture({id:"n",name:"chart",command:":view create Metric",dependsOn:["mapped"],dependencyLifetime:"creation",constructionComplete:true});
 const workspace={...props.workspace,nodes:[...props.workspace.nodes,{id:"mapped",name:"mapped",command:"toSeries",dependsOn:[],state:"ready" as const,kept:false,provenance:{},cautions:[]}]};
 let tree!:ReactTestRenderer;
 // Act
 await act(async()=>{tree=create(<Inspector {...props} workspace={workspace}/>);});trees.push(tree);
 // Assert
 expect(tree.root.findByProps({role:"note"}).children.join("")).toBe("Created once from $mapped; refreshing it does not create this again.");
});
it("should_OmitTheCreationNote_When_TheNodeHasContinuousInputs",async()=>{
 // Arrange
 const {props}=fixture({dependencyLifetime:"continuous"});let tree!:ReactTestRenderer;
 // Act
 await act(async()=>{tree=create(<Inspector {...props}/>);});trees.push(tree);
 // Assert
 expect(tree.root.findAllByProps({role:"note"})).toHaveLength(0);
});
