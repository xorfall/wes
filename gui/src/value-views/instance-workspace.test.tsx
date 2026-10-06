import {localPackage} from "./package.test-support";
import metric from "../../../views/metric/View";
import {afterEach,expect,it,vi} from "vitest";
import {act,create} from "react-test-renderer";
import {InstanceView} from "./InstanceView";
import {valueViewModules} from "./registry";
import type {FrameSample,ViewFrame} from "./instances";
import type {Engine} from "../engine";
import type {StoredValue} from "../protocol";
import {frameRenderObservation,type RenderObservation} from "../view-render-status";

vi.mock("../view-render-status",async importOriginal=>{
  const actual=await importOriginal<typeof import("../view-render-status")>();
  return {...actual,frameRenderObservation:vi.fn(actual.frameRenderObservation)};
});
afterEach(()=>{vi.mocked(frameRenderObservation).mockClear();});

function card():ViewFrame{return {root:"card",instances:[
  {id:"card",instance:"card-identity",query:null,inputReference:{kind:"unlinked"},inputDelivery:"finite",observing:false,inputRevision:"0",inputProblem:null,inputCautions:[],linkedInputs:[],revision:"0",
    definition:"metric",digest:valueViewModules.named("metric")!.definition!.digest,members:{},
    input:{type:{kind:"record",name:"Sample",fields:[{name:"view",type:{kind:"primitive",name:"TEXT"}},{name:"value",type:{kind:"primitive",name:"INT"}}]},data:{view:"metric",value:1},provenance:{}}},
]};}
/** An unbound engine whose startup workspace is announced later by its session, not named "default". */
function unboundEngine(){
  let workspace:string|undefined;
  const listeners=new Set<()=>void>();
  const engine={viewWorkspaceName:()=>workspace,onViewWorkspace:(listener:()=>void)=>{listeners.add(listener);return ()=>{listeners.delete(listener);};},
    viewGeneration:()=>"session",watchViewFrame:(_id:string,_identity:string,listener:(sample:FrameSample)=>void)=>{listener({frame:card()});return ()=>{};}} as unknown as Engine;
  return {engine,listeners,announce:(name:string|undefined)=>{workspace=name;listeners.forEach(listener=>listener());}};
}
const value:StoredValue={type:{kind:"meta",name:"ViewInstance"},data:{id:"card",instance:"card-identity"},provenance:{}};
const observations=()=>vi.mocked(frameRenderObservation).mock.results.map(result=>result.value as RenderObservation);

it("should_ObserveCanvasesUnderAnnouncedWorkspace_When_UnboundEngineServesNonDefaultWorkspace",()=>{
  // Arrange
  localPackage("metric",metric);
  const {engine,listeners,announce}=unboundEngine();
  let tree!:ReturnType<typeof create>;
  // Act
  act(()=>{tree=create(<InstanceView value={value} engine={engine} mode="window"/>);});
  // Assert: no receipt scope while the identity is unknown
  expect(frameRenderObservation).not.toHaveBeenCalled();
  act(()=>announce("research"));
  expect(vi.mocked(frameRenderObservation).mock.calls.at(-1)?.[1]).toBe("research");
  expect(observations().at(-1)!.scope("view/card","card-identity")).toEqual({workspace:"research",generation:"session",node:"card",instance:"card-identity"});
  act(()=>announce("lab"));
  expect(observations().at(-1)!.scope("view/card","card-identity")?.workspace).toBe("lab");
  const calls=vi.mocked(frameRenderObservation).mock.calls.length;
  act(()=>announce(undefined));
  expect(vi.mocked(frameRenderObservation).mock.calls.length).toBe(calls);
  expect(vi.mocked(frameRenderObservation).mock.calls.some(call=>call[1]==="default")).toBe(false);
  act(()=>tree.unmount());
  expect(listeners.size).toBe(0);
});
