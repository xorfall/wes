import {afterEach,expect,it,vi} from "vitest";
import {act,create,type ReactTestRenderer} from "react-test-renderer";
import {JsonTree,jsonSummary,typedJsonSummary} from "./JsonTree";
import type {TypeShape} from "../../protocol";
const measurement=vi.hoisted(()=>({columns:100}));
vi.mock("./measure",()=>({useColumns:()=>measurement.columns}));
const trees:ReactTestRenderer[]=[];
afterEach(()=>{act(()=>trees.splice(0).forEach(tree=>tree.unmount()));measurement.columns=100;vi.unstubAllGlobals();});
it("returns to the real root when a drilled narrow tree becomes wide",()=>{
 measurement.columns=30;
 const data={rootOnly:"root marker",branch:{"a/b":{leaf:"nested marker"}}};let tree!:ReactTestRenderer;
 act(()=>{tree=create(<JsonTree data={data}/>);});trees.push(tree);
 act(()=>tree.root.findByProps({"aria-label":"Open /branch"}).props.onClick());
 expect(JSON.stringify(tree.toJSON())).not.toContain("root marker");
 expect(tree.root.findByProps({"aria-label":"Open /branch/a~1b"})).toBeTruthy();
 measurement.columns=100;act(()=>tree.update(<JsonTree data={data}/>));
 expect(JSON.stringify(tree.toJSON())).toContain("root marker");
 act(()=>tree.root.findByProps({"aria-label":"Open /branch"}).props.onClick());
 expect(tree.root.findByProps({"aria-label":"Open /branch/a~1b"})).toBeTruthy();
});
it("reports a rejected clipboard copy without losing the selected pointer",async()=>{
 const writeText=vi.fn().mockRejectedValue(new Error("Permission denied"));vi.stubGlobal("navigator",{clipboard:{writeText}});
 let tree!:ReactTestRenderer;act(()=>{tree=create(<JsonTree data={{"a/b":1}}/>);});trees.push(tree);
 act(()=>tree.root.findByProps({className:"json-row"}).props.onClick());
 await act(async()=>tree.root.findAllByType("button").find(button=>button.children.join("")==="copy pointer")!.props.onClick());
 expect(writeText).toHaveBeenCalledWith("/a~1b");expect(JSON.stringify(tree.toJSON())).toContain("Copy failed · Permission denied");
});

it("keeps the initial short preview natural and bounds details explicitly opened by the reader",()=>{
 const data={branch:{leaf:1},two:2,three:3,four:4,five:5};let tree!:ReactTestRenderer;
 act(()=>{tree=create(<JsonTree data={data} mode="preview"/>);});trees.push(tree);
 const grown=()=>tree.root.findAll(node=>typeof node.props.className==="string" && node.props.className.split(" ").includes("json-preview-grown"));
 expect(grown()).toHaveLength(0);
 expect(JSON.stringify(tree.toJSON())).not.toContain('"leaf"');
 act(()=>tree.root.findByProps({"aria-label":"Open /branch"}).props.onClick());
 expect(grown()).toHaveLength(1);expect(JSON.stringify(tree.toJSON())).toContain('"leaf"');
 act(()=>tree.root.findByProps({"aria-label":"Close /branch"}).props.onClick());
 expect(grown()).toHaveLength(0);
 act(()=>tree.root.findAllByType("button").find(button=>button.children.join("").startsWith("show 1 more"))!.props.onClick());
 expect(grown()).toHaveLength(1);expect(JSON.stringify(tree.toJSON())).toContain('"five"');
});

/* Synthetic temporal values with fractional precision a browser Date would lose. */
const primitive=(name:string):TypeShape=>({kind:"primitive",name});
const INSTANT="2031-07-08T09:10:11.123456789Z",LATER="2031-07-08T09:10:12.000000001Z";
const DURATION="PT0.000000250S",INTERVAL=`${INSTANT}/${LATER}`,LOOKALIKE="2031-07-08T09:10:11.5Z";
const temporalRecord:TypeShape={kind:"record",name:"SyntheticWindow",fields:[
 {name:"at",type:primitive("INSTANT")},{name:"span",type:primitive("DURATION")},{name:"window",type:primitive("INTERVAL")},
 {name:"note",type:primitive("TEXT")},{name:"maybe",type:{kind:"option",element:primitive("INSTANT")}},
 {name:"marks",type:{kind:"list",element:primitive("INSTANT")}},
]};
const temporalData={at:INSTANT,span:DURATION,window:INTERVAL,note:LOOKALIKE,maybe:{kind:"some",value:LATER},marks:[LATER]};
const textOf=(node:import("react-test-renderer").ReactTestInstance):string=>node.children.map(child=>typeof child==="string" ? child : textOf(child)).join("");
const valuesOf=(tree:ReactTestRenderer)=>tree.root.findAll(node=>node.type==="span" && typeof node.props.className==="string" && node.props.className.split(" ").includes("json-value")).map(textOf);
const factsOf=(tree:ReactTestRenderer)=>tree.root.findAll(node=>node.type==="p" && node.props.className==="json-facts").map(textOf);

it("should_ShowDeclaredTemporalTextExactly_When_NestedInRecordsOptionsAndLists",()=>{
 // Arrange
 let tree!:ReactTestRenderer;
 // Act
 act(()=>{tree=create(<JsonTree data={temporalData} type={temporalRecord}/>);});trees.push(tree);
 act(()=>tree.root.findByProps({"aria-label":"Open /marks"}).props.onClick());
 // Assert
 const values=valuesOf(tree);
 expect(values).toEqual(expect.arrayContaining([INSTANT,DURATION,INTERVAL,JSON.stringify(LOOKALIKE),LATER]));
 expect(values.filter(value=>value===LATER)).toHaveLength(2);
 expect(values).not.toContain(JSON.stringify(INSTANT));
});

it("should_ShowDeclaredTemporalRootExactly_When_CollapsedOptionalOrDrilled",()=>{
 // Arrange
 let tree!:ReactTestRenderer;
 const optional:TypeShape={kind:"option",element:primitive("DURATION")};
 // Act
 act(()=>{tree=create(<JsonTree data={INTERVAL} type={primitive("INTERVAL")}/>);});trees.push(tree);
 const root=factsOf(tree);
 act(()=>tree.update(<JsonTree data={{kind:"some",value:DURATION}} type={optional} collapsed/>));
 const collapsed=factsOf(tree);
 act(()=>tree.update(<JsonTree data={LOOKALIKE} type={primitive("TEXT")}/>));
 const text=factsOf(tree);
 measurement.columns=30;
 act(()=>tree.update(<JsonTree data={{outer:temporalData}} type={{kind:"record",name:"",fields:[{name:"outer",type:temporalRecord}]}}/>));
 act(()=>tree.root.findByProps({"aria-label":"Open /outer"}).props.onClick());
 // Assert
 expect(root).toEqual([INTERVAL]);
 expect(collapsed).toEqual([DURATION]);
 expect(text).toEqual([JSON.stringify(LOOKALIKE)]);
 expect(valuesOf(tree)).toEqual(expect.arrayContaining([INSTANT,DURATION,JSON.stringify(LOOKALIKE)]));
});

it("should_KeepGenericSummaries_When_NoTemporalTypeIsDeclared",()=>{
 // Arrange
 let tree!:ReactTestRenderer;
 // Act
 act(()=>{tree=create(<JsonTree data={{at:INSTANT}}/>);});trees.push(tree);
 // Assert
 expect(valuesOf(tree)).toEqual([JSON.stringify(INSTANT)]);
 expect(jsonSummary(INSTANT)).toBe(JSON.stringify(INSTANT));
 expect(typedJsonSummary(INSTANT,primitive("TEXT"))).toBe(JSON.stringify(INSTANT));
 expect(typedJsonSummary(INSTANT,{kind:"unknown"})).toBe(JSON.stringify(INSTANT));
 // A numeric wire instant is not canonical text: the tree keeps the exact number.
 expect(typedJsonSummary(1700000000000,primitive("INSTANT"))).toBe("1700000000000");
});
