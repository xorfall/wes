import { expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import type { StoredValue } from "../../protocol";
import { parseExactJson } from "../../exact-json";
import { ValueBlock } from "./ValueBlock";

vi.mock("../Cell", async (original) => ({ ...(await original<typeof import("../Cell")>()), useBlockReport: () => () => undefined }));
const value=(data:unknown):StoredValue=>({type:{kind:"unknown"},provenance:{},data});
const text=(tree:ReactTestRenderer)=>tree.root.findAllByType("span").map(span=>span.children.filter(child=>typeof child==="string").join("")).join(" ") + tree.root.findAllByType("button").map(button=>button.children.filter(child=>typeof child==="string").join("")).join(" ");
function draw(data:unknown,mode:"preview"|"window"="preview") {let tree!:ReactTestRenderer;act(()=>{tree=create(<ValueBlock value={value(data)} cacheKey="synthetic" mode={mode}/>);});return tree;}

it("shows honest root summaries, counts and locally expandable JSON branches",()=>{
 const tree=draw({first:1,second:2,third:3,fourth:4,hidden:{secret:"synthetic"}});
 expect(text(tree)).toContain("1 not shown");expect(text(tree)).not.toContain("secret");
 act(()=>tree.root.findByProps({className:"cell-action json-more"}).props.onClick());
 act(()=>tree.root.findByProps({"aria-label":"Open /hidden"}).props.onClick());
 expect(text(tree)).toContain("secret");expect(text(tree)).toContain("synthetic");
 act(()=>tree.root.findByProps({"aria-label":"Close /hidden"}).props.onClick());expect(text(tree)).not.toContain("synthetic");
 act(()=>tree.unmount());
});
it("keeps nested JSON disclosure state when preview becomes full",()=>{
 const stored=value({branch:{message:"retained"}});let tree!:ReactTestRenderer;
 act(()=>{tree=create(<ValueBlock value={stored} cacheKey="h" bindingKey="g:n" mode="preview"/>);});
 act(()=>tree.root.findByProps({"aria-label":"Open /branch"}).props.onClick());
 act(()=>tree.update(<ValueBlock value={stored} cacheKey="h" bindingKey="g:n" mode="window"/>));
 expect(tree.root.findByProps({"aria-label":"Close /branch"}).props["aria-expanded"]).toBe(true);
 expect(text(tree)).toContain("retained");act(()=>tree.unmount());
});
it("preserves exact numbers and escaped pointers instead of exposing implementation fields",()=>{
 const tree=draw(parseExactJson('{"a/b~c":{"large":9007199254740993,"fraction":0.123456789123456789123456789}}'),"window");
 act(()=>tree.root.findByProps({"aria-label":"Open /a~1b~0c"}).props.onClick());
 expect(text(tree)).toContain("9007199254740993");expect(text(tree)).toContain("0.123456789123456789123456789");expect(tree.root.findAllByProps({className:"json-key"}).some(key=>key.children.join("")==="lexeme")).toBe(false);act(()=>tree.unmount());
});
it("opens, pages and closes nested table data with independent addresses",()=>{
 for(const mode of ["preview","window"] as const) {
  const tree=draw([{options:Array.from({length:62},(_,i)=>({schemes:[`choice-${i}`]})),sibling:["other"]}],mode);
  const options=()=>tree.root.findByProps({"aria-label":"Open /0/options"});
  act(()=>options().props.onClick());
  expect(options().props["aria-expanded"]).toBe(true);
  expect(tree.root.findByProps({"aria-label":"Open /0/sibling"}).props["aria-expanded"]).toBe(false);
  const schemes=()=>tree.root.findByProps({"aria-label":"Open /0/options/0/schemes"});
  act(()=>schemes().props.onClick());expect(text(tree)).toContain("choice-0");
  const pages=()=>tree.root.findByProps({"aria-label":"Pages of /0/options"});
  expect(pages().findAllByType("button")[0]!.props.disabled).toBe(true);
  act(()=>pages().findAllByType("button")[1]!.props.onClick());
  expect(tree.root.findByProps({"aria-label":"Open /0/options/50/schemes"})).toBeDefined();
  expect(pages().findAllByType("button")[1]!.props.disabled).toBe(true);
  act(()=>pages().findAllByType("button")[0]!.props.onClick());expect(text(tree)).toContain("choice-0");
  act(()=>options().props.onClick());expect(pages).toBeDefined();expect(tree.root.findAllByProps({"aria-label":"Pages of /0/options"})).toHaveLength(0);act(()=>tree.unmount());
 }
});
it("renders management projections without opening a management dialog",()=>{
 const tree=draw({workspace:"demo",blockers:[],cells:2},"window");expect(text(tree)).toContain("demo");expect(tree.root.findAllByProps({role:"dialog"})).toHaveLength(0);act(()=>tree.unmount());
});

const plan=(over:Record<string,unknown>={}):StoredValue=>({type:{kind:"meta",name:"ImportPlan"},provenance:{},data:{
 notice:"Restored, expired or used plans cannot be applied. Planning read no contents. Authority is usable once in this session until expiry.",
 expires:"2026-10-04T12:10:00Z",kind:"spec",alias:"orders",arguments:{file:"synthetic/orders.json"},contentsReadWhenPlanned:false,
 argumentTypes:{file:"Text"},origins:[{argument:"file",node:"cfg",port:"data",run:"r3",fields:["path"]}],...over}});
function drawValue(stored:StoredValue,mode:"preview"|"window"="window") {let tree!:ReactTestRenderer;act(()=>{tree=create(<ValueBlock value={stored} cacheKey={`synthetic:${mode}`} mode={mode}/>);});return tree;}

it("should_ShowAnImportPlanAsItsStaticProjection_When_TheValueIsAManagementPlan",()=>{
 // Arrange
 const stored=plan();
 // Act
 const tree=drawValue(stored);
 // Assert
 const said=text(tree);
 expect(said).toContain("contentsReadWhenPlanned");expect(said).toContain("false");
 expect(said).toContain("expires");expect(said).toContain("2026-10-04T12:10:00Z");
 expect(said).toContain("origins");
 expect(said).not.toMatch(/\b(consumed|still valid|live plan|ready to apply)\b/i);
 expect(tree.root.findAllByType("button").map(button=>button.children.join(""))).not.toContainEqual(expect.stringMatching(/apply/i));
 expect(tree.root.findAllByProps({role:"dialog"})).toHaveLength(0);
 act(()=>tree.unmount());
});

it.each([
 ["a scalar", "opaque"],
 ["null", null],
 ["a list", [1,2]],
])("should_RenderAnUnknownManagementValueAsData_When_ItsProjectionIs %s",(_case,data)=>{
 // Arrange
 const stored:StoredValue={type:{kind:"meta",name:"FutureManagementPlan"},provenance:{},data};
 // Act
 const tree=drawValue(stored);
 // Assert
 expect(tree.toJSON()).not.toBeNull();
 expect(tree.root.findAllByProps({className:"value-instance-block"})).toHaveLength(0);
 expect(tree.root.findAllByProps({role:"dialog"})).toHaveLength(0);
 act(()=>tree.unmount());
});

it("should_ShowThePlanRestrictionAndExpiryInPreview_When_AnImportPlanIsACellResult",()=>{
 // Arrange
 const stored=plan();
 // Act
 const tree=drawValue(stored,"preview");
 // Assert
 const said=text(tree);
 expect(said).toContain("Restored, expired or used plans cannot be applied");
 expect(said).toContain("2026-10-04T12:10:00Z");
 expect(tree.root.findAllByType("button").map(button=>button.children.join(""))).not.toContainEqual(expect.stringMatching(/apply/i));
 expect(said).not.toMatch(/\b(consumed|still valid|live plan|ready to apply)\b/i);
 act(()=>tree.unmount());
});
