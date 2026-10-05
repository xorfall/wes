import {expect,it} from "vitest";
import {renderToStaticMarkup} from "react-dom/server";
import {valueViewModules} from "./registry";
import {decodeContract} from "./definition";
import metric from "../../../views/metric/View";
import histogram from "../../../views/histogram/View";
import choice from "../../../views/choice/View";
import dashboard from "../../../views/dashboard/View";
import {definition} from "../../../views/metric/contract";
import {parseExactJson} from "../exact-json";
import type {Input} from "../../../views/metric/contract";
import type {Input as HistogramInput} from "../../../views/histogram/contract";
const context={mode:"window" as const,instance:null};
const valid:Input={view:"metric",value:1250,label:"Latency",unit:"ms",status:"healthy"};
// @ts-expect-error Undeclared fields cannot become renderer inputs.
const invalid:Input={view:"metric",seconds:1};void invalid;

it("ships domain Views through compiled artifact adapters, with no retired default definitions",()=>{
  const names=valueViewModules.get().flatMap(v=>v.definition?[v.definition.name]:[]).sort();
  expect(names).toEqual(["Choice","Dashboard","Histogram","Metric","Timeline","TimelineGroup"]);
  expect(valueViewModules.named("http-response")).toBeUndefined();
  expect(valueViewModules.named("http")!.definition).toBeUndefined();
  for(const module of valueViewModules.get().filter(v=>v.definition)) {
    expect(module.definition!.artifact).toMatch(/^[a-f0-9]{64}$/);
    expect(module.id).toBe(`${module.definition!.id}--${module.definition!.artifact}`);
    for(const tier of Object.values(module.definition!.layout!)) {
      expect(tier.placement).toBeDefined();
      expect(tier.min.columns).toBeLessThanOrEqual(tier.preferred.columns);
      expect(tier.preferred.columns).toBeLessThanOrEqual(tier.max.columns);
    }
  }
  expect(valueViewModules.named("metric")!.definition!.digest).toBe(definition.digest);
  for(const id of ["time-series","drawing","duration-card","range-summary"])expect(valueViewModules.named(id)).toBeUndefined();
});
it("keeps compact status Views near preferred width while plots and compositions fill their slot",()=>{
  for(const name of ["metric","choice"]){
    const layout=valueViewModules.named(name)!.definition!.layout!;
    expect(layout.expanded.placement?.width).toBe("preferred");
    expect(layout.window.preferred.columns).toBeLessThanOrEqual(40);
    expect(layout.expanded.preferred.rows).toBeLessThanOrEqual(6);
  }
  for(const name of ["histogram","timeline","timeline-group","dashboard"])
    expect(valueViewModules.named(name)!.definition!.layout!.expanded.placement).toEqual({width:"fill",align:"start"});
});
it("bounds a composed Dashboard preview and discloses the remaining members without dropping them in expanded/window",()=>{
  const props={input:{view:"dashboard" as const,title:"Operations"},state:undefined as never,revision:0,emit:()=>{},slots:{members:["first","second","third","fourth"].map(name=><p key={name}>{name}</p>)}};
  const preview=renderToStaticMarkup(<dashboard.Component {...props} context={{...context,mode:"preview"}}/>);
  expect(preview).toContain("first");expect(preview).toContain("second");expect(preview).not.toContain("third");expect(preview).toContain("2 more views");
  for(const mode of ["expanded","window"] as const){
    const html=renderToStaticMarkup(<dashboard.Component {...props} context={{...context,mode}}/>);
    expect(html).toContain("third");expect(html).toContain("fourth");expect(html).not.toContain("more views");
  }
});
it("matches declared metric input structurally and preserves exact values in the SDK renderer",()=>{
  const type={kind:"record" as const,name:"Sample",fields:[{name:"view",type:{kind:"primitive" as const,name:"TEXT"}},{name:"value",type:{kind:"primitive" as const,name:"INT"}}]};
  const value=parseExactJson('{"view":"metric","value":9007199254740993}') as Input;
  expect(valueViewModules.named("metric")!.matches(type,value)).toBe(true);
  expect(valueViewModules.named("metric")!.matches({kind:"unknown"},value)).toBe(false);
  expect(valueViewModules.named("metric")!.matches(type,{view:"metric",value:"wrong"})).toBe(false);
  const html=renderToStaticMarkup(<metric.Component input={value} state={undefined as never} revision={0} emit={()=>{}} slots={{}} context={context}/>);
  expect(html).toContain("9007199254740993");expect(html).not.toContain("9007199254740992");
});
it("rejects malformed metrics before an author receives them and escapes labels",()=>{
  for(const input of [{view:"metric",value:"1"},{view:"other",value:1},{view:"metric"}])
    expect(()=>decodeContract(definition,definition.input,input)).toThrow();
  const html=renderToStaticMarkup(<metric.Component input={{...valid,label:"<script>"}} state={undefined as never} revision={0} emit={()=>{}} slots={{}} context={context}/>);
  expect(html).toContain("&lt;script&gt;");expect(html).toContain("status-ok");
});
it("uses Choice state and outputs from the public SDK and leaves unavailable selections visible",()=>{
  const input={view:"choice" as const,title:"Feed",options:[{value:"demo",label:"Demo"}]};
  const state=choice.initial!(input),next=choice.reduce!(state,{value:"other"});
  expect(choice.outputs!(next)).toEqual({value:"other"});
  const html=renderToStaticMarkup(<choice.Component input={input} state={next} revision={1} emit={()=>{}} slots={{}} context={context}/>);
  expect(html).toContain("Selection unavailable");
  expect(()=>renderToStaticMarkup(<choice.Component input={{...input,options:[...input.options,...input.options]}} state={state} revision={0} emit={()=>{}} slots={{}} context={context}/>)).toThrow(/distinct/);
});
it("draws bounded histograms and retains exact labels and counts when drawing is unsafe",()=>{
  const input=parseExactJson('{"view":"histogram","bins":[{"lower":9007199254740993,"upper":9007199254740994,"count":9007199254740993}],"total":9007199254740993}') as HistogramInput;
  const html=renderToStaticMarkup(<histogram.Component input={input} state={undefined as never} revision={0} emit={()=>{}} slots={{}} context={context}/>);
  expect(html).toContain("9007199254740993");expect(html).toContain("exact drawing range");expect(html).not.toContain("<svg");
  const empty=renderToStaticMarkup(<histogram.Component input={{view:"histogram",bins:[],total:0}} state={undefined as never} revision={0} emit={()=>{}} slots={{}} context={context}/>);
  expect(empty).toContain("No samples");
});
