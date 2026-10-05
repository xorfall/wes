import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, expect, it, vi } from "vitest";
import { ViewHost } from "./ViewHost";
import { Cell } from "./Cell";
import { readSession } from "./session-model";
import { apply, emptyWorkspace } from "../workspace";
import { newCell } from "../cells";

afterEach(() => vi.unstubAllGlobals());

it("reserves a bounded expanded stream viewport and keeps manual height out of preview",()=>{
 const onResize=vi.fn();let tree!:ReactTestRenderer;
 act(()=>{tree=create(<ViewHost stream mode="expanded" onResize={onResize}><div>sample</div></ViewHost>);});
 expect(tree.root.findByProps({"aria-label":"Data viewport"}).props.style).toBeUndefined(); // Content determines automatic height; stream tables/logs reserve rows via the host.
 expect(JSON.stringify(tree.toJSON())).not.toContain("fit output");
 act(()=>tree.update(<ViewHost stream mode="expanded" rows={30} onResize={onResize}><div>smaller sample</div></ViewHost>));
 expect(tree.root.findByProps({"aria-label":"Data viewport"}).props.style.height).toBe("30lh");
 act(()=>tree.update(<ViewHost stream mode="preview" rows={30} onResize={onResize}><div>preview</div></ViewHost>));
 expect(tree.root.findByProps({"aria-label":"Data viewport"}).props.style).toBeUndefined();act(()=>tree.unmount());
});

it("projects explicit stream metadata into a independent data hosts through state changes", () => {
  let workspace = apply(emptyWorkspace, { event: "created", dependencyLifetime: "continuous", node: "n", command: "synthetic", name: "", dependsOn: [], interactive: false, streamOutput: true });
  const client = { ...newCell("synthetic"), state: "answered" as const, nodes: ["n"] };
  for (const state of ["running", "ready", "stale", "cancelled"] as const) {
    workspace = apply(workspace, { event: "node", constructionComplete: false, node: "n", state });
    const model = readSession({ workspace, cells: [client], context: { workspace: "qa", connection: "connected" } }).cells[0]!;
    expect(model.streamOutput).toBe(true);
  }
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<Cell theme="controls" state="live" rows={[]} verdict={[]} streamOutput blocks={[
    { key: "chart", open: true, content: <div>chart</div> },
    { key: "table", open: true, content: <div>table</div> },
  ]} />); });
  expect(tree.root.findAllByType(ViewHost)).toHaveLength(2);
  expect(tree.root.findAllByType(ViewHost).every(host=>host.props.stream)).toBe(false); // Stream demand belongs to a specific result, never all results in its cell.
  act(() => tree.unmount());
});

it("resizes finite results accessibly, clamps extremes and explicitly resets to natural height",()=>{
  const onResize=vi.fn();let tree!:ReactTestRenderer;
  act(()=>{tree=create(<ViewHost mode="expanded" rows={20} onResize={onResize}><div>synthetic</div></ViewHost>);});
  const slider=()=>tree.root.findByProps({role:"slider"});
  const press=(key:string)=>act(()=>slider().props.onKeyDown({key,preventDefault(){},stopPropagation(){}}));
  press("ArrowUp");expect(onResize).toHaveBeenLastCalledWith(21);
  press("ArrowDown");expect(onResize).toHaveBeenLastCalledWith(19);
  press("End");expect(onResize).toHaveBeenLastCalledWith(40);
  press("Home");expect(onResize).toHaveBeenLastCalledWith(undefined);
  act(()=>tree.update(<ViewHost mode="expanded" rows={40} onResize={onResize}><div>synthetic</div></ViewHost>));press("ArrowUp");expect(onResize).toHaveBeenLastCalledWith(40);
  act(()=>tree.update(<ViewHost mode="expanded" rows={6} onResize={onResize}><div>synthetic</div></ViewHost>));press("ArrowDown");expect(onResize).toHaveBeenLastCalledWith(6);
  act(()=>tree.update(<ViewHost mode="expanded" onResize={onResize}><div>synthetic</div></ViewHost>));press("ArrowUp");expect(onResize).toHaveBeenLastCalledWith(15);
  act(()=>tree.update(<ViewHost mode="preview" rows={20} onResize={onResize}><div>synthetic</div></ViewHost>));expect(tree.root.findAllByProps({role:"slider"})).toHaveLength(0);
  act(()=>tree.unmount());
});

it("starts keyboard resizing from the measured natural height, keeping manual geometry out of preview",()=>{
  vi.stubGlobal("getComputedStyle",()=>({lineHeight:"21px"}));
  const onResize=vi.fn();let tree!:ReactTestRenderer;
  act(()=>{tree=create(<ViewHost mode="expanded" onResize={onResize}><div>three visible lines</div></ViewHost>,{createNodeMock:node=>node.props["aria-label"]==="Data viewport"?{getBoundingClientRect:()=>({height:63}),closest:()=>null}:null});});
  act(()=>tree.root.findByProps({role:"slider"}).props.onKeyDown({key:"ArrowUp",preventDefault(){},stopPropagation(){}}));
  expect(onResize).toHaveBeenCalledWith(6);
  act(()=>tree.update(<ViewHost mode="preview" rows={40} onResize={onResize}><div>preview</div></ViewHost>));
  expect(tree.root.findByProps({"aria-label":"Data viewport"}).props.style).toBeUndefined();
  act(()=>tree.unmount());
});

it("honors dynamic bounds and drags when its content has no scrollbar",()=>{
 vi.stubGlobal("getComputedStyle",()=>({lineHeight:"20px"}));
 const onResize=vi.fn(),listeners=new Map<string,(event:any)=>void>(),release=vi.fn();let tree!:ReactTestRenderer;
 const layout={dataset:{minRows:"8",preferredRows:"18",maxRows:"60"}};
 act(()=>{tree=create(<ViewHost mode="expanded" onResize={onResize}><div>small dynamic chart</div></ViewHost>,{createNodeMock:node=>node.props.className==="view-host-content"?{querySelector:()=>layout}:node.props["aria-label"]==="Data viewport"?{getBoundingClientRect:()=>({height:100}),scrollHeight:100,clientHeight:100}:null});});
 const slider=tree.root.findByProps({role:"slider"});expect(slider.props["aria-valuemin"]).toBe(8);expect(slider.props["aria-valuemax"]).toBe(60);
 const grip={focus:vi.fn(),setPointerCapture:vi.fn(),hasPointerCapture:()=>true,releasePointerCapture:release,addEventListener:(name:string,fn:(event:any)=>void)=>listeners.set(name,fn),removeEventListener:(name:string)=>listeners.delete(name)};
 act(()=>slider.props.onPointerDown({button:0,pointerId:7,clientY:100,currentTarget:grip,preventDefault(){},stopPropagation(){}}));
 act(()=>listeners.get("pointermove")!({pointerId:7,clientY:300}));expect(onResize).toHaveBeenLastCalledWith(15);
 act(()=>listeners.get("pointermove")!({pointerId:8,clientY:1200}));expect(onResize).toHaveBeenCalledOnce();
 act(()=>listeners.get("pointermove")!({pointerId:7,clientY:2200}));expect(onResize).toHaveBeenLastCalledWith(60);
 act(()=>listeners.get("pointerup")!({pointerId:7}));expect(release).toHaveBeenCalledWith(7);expect(listeners.size).toBe(0);act(()=>tree.unmount());
});
