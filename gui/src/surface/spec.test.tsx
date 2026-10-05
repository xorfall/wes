import { SpecSourceEditor } from "./SpecSourceEditor";
import { afterEach, expect, it, vi } from "vitest";
import { act,create,type ReactTestInstance,type ReactTestRenderer } from "react-test-renderer";
import { SpecScreen } from "./screens/Spec";
import { read } from "./commands";
import { importSpecCommand } from "../api-library";
vi.mock("./SpecSourceEditor",()=>({SpecSourceEditor:({source,onChange}:{source:string;onChange:(s:string)=>void})=><textarea aria-label="test-source" value={source} onChange={e=>onChange(e.target.value)}/>}));
let tree:ReactTestRenderer|undefined;
afterEach(()=>{if(tree)act(()=>tree!.unmount());tree=undefined;vi.unstubAllGlobals();});
it("routes spec panes and rejects removed model settings",()=>{
 expect(read("/spec split")).toEqual({kind:"screen",screen:"spec",inPane:true});
 expect(read('/settings describe model "synthetic-model"').kind).toBe("trouble");
 expect(read('/settings describe key set secret').kind).toBe("trouble");
 expect(()=>importSpecCommand('/tmp/spec.json','a','https://user:secret@example.test',false)).toThrow();
 expect(importSpecCommand('/tmp/a"b.json','inventory','https://example.test',true)).toBe(':import spec file:"/tmp/a\\"b.json" as:inventory endpoint:"https://example.test" replace:true');
});
it("reviews real revisions, blocks dirty navigation and imports only explicitly",async()=>{
 const p={key:{service:"inventory",apiVersion:"v1",scope:"all"},revision:"a".repeat(64),accepted:false,origin:"fixture"};
 const descriptor={version:1,provider:"inventory",operations:[],types:{}};const source=JSON.stringify(descriptor);
 const fetch=vi.fn(async(_url,init)=>{const req=JSON.parse(init.body);return {ok:true,json:async()=>req.action==="list"?{packages:[p]}:{package:p,descriptor,source,descriptorPath:"/tmp/fixture.json"}};});vi.stubGlobal("fetch",fetch);
 const close=vi.fn();const submit=vi.fn(()=>"cell");
 await act(async()=>{tree=create(<SpecScreen top={[]} onClose={close} onSubmit={submit}/>);});
 const button=(text:string)=>tree!.root.findAllByType("button").find(b=>b.children.join("").includes(text))!;
 await act(async()=>{tree!.root.findByProps({"aria-label":"Open inventory API r1"}).props.onClick();});expect(submit).not.toHaveBeenCalled();
 act(()=>button("source").props.onClick());act(()=>tree!.root.findByType("textarea").props.onChange({target:{value:"{}"}}));
 act(()=>tree!.root.findByProps({"aria-label":"/spec"}).props.onKeyDown({key:"Escape",preventDefault(){},stopPropagation(){}}));
 expect(close).not.toHaveBeenCalled();expect(tree!.root.findByProps({role:"alertdialog"})).toBeDefined();
 act(()=>button("keep editing").props.onClick());expect(submit).not.toHaveBeenCalled();
 expect(tree!.root.findAllByProps({"aria-label":"API library"})).toHaveLength(0);
 expect(tree!.root.findAllByProps({"aria-label":"Import OpenAPI"})).toHaveLength(0);
 act(()=>button("Back to library").props.onClick());
 expect(tree!.root.findByProps({role:"alertdialog"})).toBeDefined();
 act(()=>button("discard").props.onClick());
 expect(tree!.root.findByProps({"aria-label":"API library"})).toBeDefined();
 expect(tree!.root.findByProps({"aria-label":"Import OpenAPI"})).toBeDefined();
 expect(tree!.root.findAllByType("textarea")).toHaveLength(0);
 expect(close).not.toHaveBeenCalled();
});

it("shows auth by its recorded basis, keeps operation indices through the filter and reads field provenance of the saved revision",async()=>{
 const p={key:{service:"inventory",apiVersion:"v1",scope:"all"},revision:"d".repeat(64),accepted:false,origin:"fixture"};
 const op=(name:string,auth:{scheme:string;secret?:string}[])=>({path:[name],method:"GET",route:`/${name}`,parameters:[{name:"id",location:"path",type:"Text",required:true}],responses:{"200":"Item","201":null},auth});
 const digest=`sha256:${"e".repeat(64)}`;
 const descriptor={version:1,provider:"inventory",operations:[op("listItems",[]),op("getItem",[]),op("adminItems",[{scheme:"bearer",secret:"admin_token"}])],
  types:{Item:{base:"Record",fields:{id:{type:"Text",optional:false}}}},
  source:{sha256:"e".repeat(64),format:"model-draft/openapi",location:"https://docs.synthetic.invalid/api",provenance:{version:1,status:"current",entries:[
   {target:"#/operations/0/auth",source:digest,pointer:"#/paths/~1listItems/get/security",lines:[{start:40,end:41}],basis:"documented",reason:"security: [] is stated"},
   {target:"#/operations/1/auth",source:digest,pointer:"#/paths/~1getItem/get/security",lines:[],basis:"unknown",reason:"the page never mentions authentication"},
   {target:"#/operations/1/parameters/0/required",source:digest,pointer:"#/paths/~1getItem/get/parameters/0/required",lines:[{start:52,end:52}],basis:"example",reason:"only an example request carries id"},
   {target:"#/operations/1/responses/201",source:digest,pointer:"#/paths/~1getItem/get/responses/201",lines:[{start:60,end:61}],basis:"inferred",reason:"a success status without a body is described in prose"},
   {target:"#/types/Item/fields/id/type",source:digest,pointer:"#/components/schemas/Item/properties/id",lines:[{start:7,end:7}],basis:"documented",reason:"id: string"}]}}};
 vi.stubGlobal("fetch",vi.fn(async(_url,init)=>{const req=JSON.parse(init.body);return {ok:true,json:async()=>req.action==="list"?{packages:[p]}:{package:p,descriptor,source:JSON.stringify(descriptor),descriptorPath:"/tmp/fixture.json"}};}));
 await act(async()=>{tree=create(<SpecScreen top={[]} onClose={vi.fn()} onSubmit={vi.fn(()=>"cell")}/>);});
 const button=(text:string)=>tree!.root.findAllByType("button").find(b=>b.children.join("")===text)!;
 await act(async()=>{tree!.root.findByProps({"aria-label":"Open inventory API r1"}).props.onClick();});
 const status=()=>JSON.stringify(tree!.toJSON());
 for (const name of ["listItems","getItem","adminItems"]) act(()=>tree!.root.findByProps({"aria-label":`Expand operation ${name}`}).props.onClick());
 const shown=()=>JSON.stringify(tree!.toJSON());
 expect(shown()).toContain(`saved revision ${"d".repeat(12)}`);
 for (const label of ["documented · no auth","unknown · no credentials attached","admin_token · no provenance"]) expect(shown()).toContain(label);
 act(()=>tree!.root.findByProps({"aria-label":"Filter spec operations"}).props.onChange({target:{value:"getItem"}}));
 expect(tree!.root.findAllByProps({"aria-label":"Collapse operation listItems"})).toHaveLength(0);
 act(()=>tree!.root.findByProps({"aria-label":"Auth provenance of getItem"}).props.onClick());
 const targetLine=()=>tree!.root.findAllByProps({className:"spec-provenance-target"}).at(-1)!.findByProps({className:"mono-ref"}).children.join("");
 expect(targetLine()).toBe("#/operations/1/auth");
 expect(JSON.stringify(tree!.toJSON())).toContain("the page never mentions authentication");
 expect(tree!.root.findAllByType("a")).toHaveLength(0);
 // parameter and response cells pick their own targets, by the saved descriptor's indices and escaped tokens
 act(()=>tree!.root.findByProps({"aria-label":"Provenance of parameter id of getItem"}).props.onClick());
 expect(targetLine()).toBe("#/operations/1/parameters/0");
 expect(JSON.stringify(tree!.toJSON())).toContain("only an example request carries id");
 act(()=>tree!.root.findByProps({"aria-label":"Provenance of response 201 of getItem"}).props.onClick());
 expect(targetLine()).toBe("#/operations/1/responses/201");
 expect(JSON.stringify(tree!.toJSON())).toContain("a success status without a body is described in prose");
 act(()=>tree!.root.findByProps({"aria-label":"Provenance of response 200 of getItem"}).props.onClick());
 expect(targetLine()).toBe("#/operations/1/responses/200");
 expect(JSON.stringify(tree!.toJSON())).toContain("no provenance recorded for this target");

 act(()=>button("schema").props.onClick());
 expect(JSON.stringify(tree!.toJSON())).toContain(`saved revision ${"d".repeat(12)}`);
 act(()=>tree!.root.findByProps({"aria-label":"Expand type Item"}).props.onClick());
 act(()=>button("id").props.onClick());
 expect(JSON.stringify(tree!.toJSON())).toContain("#/components/schemas/Item/properties/id");
 expect(JSON.stringify(tree!.toJSON())).toContain("https://docs.synthetic.invalid/api");
 act(()=>button("source").props.onClick());act(()=>tree!.root.findByType("textarea").props.onChange({target:{value:"{}"}}));
 act(()=>button("schema").props.onClick());
 expect(status()).toContain("not the unsaved edits");
 expect(tree!.root.findByProps({"aria-label":"Collapse type Item"}).props["aria-expanded"]).toBe(true);
 act(()=>button("operations").props.onClick());
 expect(status()).toContain("not the unsaved edits");
});
it("imports the selected captured path only after endpoint and explicit submit",async()=>{
 const p={key:{service:"inventory",apiVersion:"v1",scope:"all"},revision:"b".repeat(64),accepted:false,origin:"fixture"};
 const descriptor={version:1,provider:"inventory",operations:[],types:{}};
 const actions:string[]=[];
 vi.stubGlobal("fetch",vi.fn(async(_url,init)=>{const req=JSON.parse(init.body);actions.push(req.action);return {ok:true,json:async()=>req.action==="list"?{packages:[p]}:{package:p,descriptor,source:JSON.stringify(descriptor),descriptorPath:"/tmp/captured.json"}};}));
 const close=vi.fn();const submit=vi.fn(()=>"cell");
 await act(async()=>{tree=create(<SpecScreen top={[]} onClose={close} onSubmit={submit}/>);});
 const button=(text:string)=>tree!.root.findAllByType("button").find(b=>b.children.join("")===text)!;
 await act(async()=>{tree!.root.findByProps({"aria-label":"Open inventory API r1"}).props.onClick();});
 act(()=>button("import").props.onClick());
 const form=tree!.root.findByProps({className:"spec-import-form"});
 await act(async()=>form.props.onSubmit({preventDefault(){}}));expect(submit).not.toHaveBeenCalled();
 act(()=>tree!.root.findByProps({"aria-label":"Import endpoint"}).props.onChange({target:{value:"https://synthetic.invalid/v1"}}));
 await act(async()=>form.props.onSubmit({preventDefault(){}}));
 expect(submit).toHaveBeenCalledExactlyOnceWith(':import spec file:"/tmp/captured.json" as:inventory endpoint:"https://synthetic.invalid/v1" replace:false');
 expect(close).toHaveBeenCalledOnce();expect(actions).toEqual(["list","inspect"]);
});

it("loads one consistent artifact snapshot without competing for the backend lock",async()=>{
 const p={key:{service:"inventory",apiVersion:"v1",scope:"all"},revision:"a".repeat(64),accepted:false,origin:"fixture"};
 const draft={key:{service:"items",apiVersion:"describe",scope:"fixture"},revision:"b".repeat(64),accepted:false,origin:"fixture",valid:false,sourceDigest:null,descriptorRevision:null};
 let active=false;const actions:string[]=[];
 vi.stubGlobal("fetch",vi.fn(async(_url,init)=>{
  const {action}=JSON.parse(init.body);actions.push(action);
  if(active)return {ok:false,text:async()=>"API library is busy in another operation; retry when it finishes"};
  active=true;await Promise.resolve();active=false;
  return {ok:true,json:async()=>({packages:[p],drafts:[draft]})};
 }));
 await act(async()=>{tree=create(<SpecScreen top={[]} onClose={vi.fn()} onSubmit={vi.fn()}/>);});
 expect(tree!.root.findByProps({"aria-label":"Open inventory API r1"})).toBeDefined();
 expect(tree!.root.findByProps({"aria-label":"Open items draft r1"})).toBeDefined();
 expect(JSON.stringify(tree!.toJSON())).toContain("needs attention");
 await act(async()=>{tree!.root.findByProps({"aria-label":"Refresh library"}).props.onClick();});
 expect(actions).toEqual(["list","list"]);
 expect(JSON.stringify(tree!.toJSON())).not.toContain("API library is busy");
});

it.each(["describe", "import"])("keeps %s submission failures visible and waits for acknowledgement before closing", async mode => {
 const p={key:{service:"inventory",apiVersion:"v1",scope:"all"},revision:"b".repeat(64),accepted:false,origin:"fixture"};
 const descriptor={version:1,provider:"inventory",operations:[],types:{}};
 vi.stubGlobal("fetch",vi.fn(async(_url,init)=>({ok:true,json:async()=>JSON.parse(init.body).action==="list"?{packages:[p],drafts:[]}:{package:p,descriptor,source:JSON.stringify(descriptor),descriptorPath:"/tmp/captured.json"}})));
 let resolve!:(value:string)=>void;let reject!:(error:Error)=>void;
 const submit=vi.fn(()=>new Promise<string>((yes,no)=>{resolve=yes;reject=no;}));const close=vi.fn();
 await act(async()=>{tree=create(<SpecScreen top={[]} onClose={close} onSubmit={submit}/>);});
 if(mode==="import") {
  await act(async()=>tree!.root.findByProps({"aria-label":"Open inventory API r1"}).props.onClick());
  act(()=>tree!.root.findAllByType("button").find(b=>b.children.join("")==="import")!.props.onClick());
  act(()=>tree!.root.findByProps({"aria-label":"Import endpoint"}).props.onChange({target:{value:"https://synthetic.invalid"}}));
 } else {
  act(()=>tree!.root.findByProps({"aria-label":"OpenAPI source"}).props.onChange({target:{value:"https://synthetic.invalid/docs"}}));
  act(()=>tree!.root.findByProps({"aria-label":"Describe provider"}).props.onChange({target:{value:"inventory"}}));
 }
 const form=()=>mode==="import"?tree!.root.findByProps({className:"spec-import-form"}):tree!.root.findAllByType("form")[0]!;
 act(()=>form().props.onSubmit({preventDefault(){}}));
 expect(close).not.toHaveBeenCalled();
 expect(form().findAllByType("button").at(-1)!.props.disabled).toBe(true);
 await act(async()=>reject(new Error("Synthetic send failed")));
 expect(JSON.stringify(tree!.toJSON())).toContain("Synthetic send failed");expect(close).not.toHaveBeenCalled();
 act(()=>form().props.onSubmit({preventDefault(){}}));
 await act(async()=>resolve("accepted-cell"));
 expect(close).toHaveBeenCalledOnce();
});

it("describes a URL or a local file from one location field and refreshes when the window returns",async()=>{
 const actions:string[]=[];
 vi.stubGlobal("fetch",vi.fn(async(_url,init)=>{actions.push(JSON.parse(init.body).action);return {ok:true,json:async()=>({packages:[],drafts:[]})};}));
 const submit=vi.fn(()=>undefined);
 const win=new EventTarget();vi.stubGlobal("window",win);vi.stubGlobal("document",Object.assign(new EventTarget(),{visibilityState:"visible"}));
 await act(async()=>{tree=create(<SpecScreen top={[]} onClose={vi.fn()} onSubmit={submit}/>);});
 expect(tree!.root.findAllByProps({"aria-label":"OpenAPI source kind"})).toHaveLength(0);
 const describe=async(location:string)=>{
  act(()=>tree!.root.findByProps({"aria-label":"OpenAPI source"}).props.onChange({target:{value:location}}));
  act(()=>tree!.root.findByProps({"aria-label":"Describe provider"}).props.onChange({target:{value:"inventory"}}));
  await act(async()=>tree!.root.findAllByType("form")[0]!.props.onSubmit({preventDefault(){}}));
 };
 await describe("https://docs.synthetic.invalid/guide");
 await describe("/tmp/synthetic/openapi.json");
 expect(submit.mock.calls).toEqual([[':describe url:"https://docs.synthetic.invalid/guide" provider:inventory'],[':describe file:"/tmp/synthetic/openapi.json" provider:inventory']]);
 await act(async()=>{win.dispatchEvent(new Event("focus"));});
 expect(actions).toEqual(["list","list"]);
});

it("discovers workspace captures without publishing library entries or submitting commands",async()=>{
 const binding={workspace:"synthetic-sensor",generation:"g1"};
 const spec={environment:"lab",alias:"prices",origin:"/synthetic/spec.json",revision:"c".repeat(64),bytes:80};
 const descriptor={version:1,provider:"original",types:{},operations:[{path:["latest"],method:"GET",route:"/latest",parameters:[],responses:{"200":"Unknown"},auth:[]}]};
 const calls:{url:string;init:any}[]=[];
 vi.stubGlobal("fetch",vi.fn(async(url:string,init:any)=>{
   calls.push({url,init});
   return {ok:true,json:async()=>url==="/api-library"?{packages:[],drafts:[]}:url.includes("alias=")?{...binding,spec,source:JSON.stringify(descriptor)}:{...binding,specs:[spec]}};
 }));
 const submit=vi.fn();
 await act(async()=>{tree=create(<SpecScreen top={[]} binding={binding} onClose={vi.fn()} onSubmit={submit}/>);});
 expect(tree!.root.findByProps({"aria-label":"Workspace APIs"})).toBeDefined();
 expect(calls.filter(c=>c.url.startsWith("/workspace-specs"))).toHaveLength(1);
 expect(calls[1]!.init.headers["X-Wes-Workspace"]).toBe("synthetic-sensor");
 expect(calls[1]!.init.headers["X-Wes-Session"]).toBe("g1");
 await act(async()=>{tree!.root.findByProps({"aria-label":"Open imported prices in lab"}).props.onClick();});
 expect(tree!.root.findAllByProps({"aria-label":"API library"})).toHaveLength(0);
 expect(tree!.root.findAllByProps({"aria-label":"Import OpenAPI"})).toHaveLength(0);
 expect(JSON.stringify(tree!.toJSON())).toContain("/latest");
 // The heading names what was captured: environment, digest revision (not an ordinal), origin and size.
 const textOf=(n:ReactTestInstance|string):string=>typeof n==="string"?n:n.children.map(textOf).join("");
 const heading=()=>tree!.root.findByProps({className:"spec-snapshot-card"});const said=textOf(heading());
 expect(said).toContain("in lab");expect(said).toContain("cccccccc…");expect(said).toContain("captured from /synthetic/spec.json · 80 bytes");
 expect(said).toContain("This is what synthetic-sensor calls as prices in lab.");expect(said).not.toMatch(/\br\d+\b|endpoint/i);
 expect(heading().findByProps({"aria-description":`sha256:${"c".repeat(64)}`})).toBeDefined();
 act(()=>heading().findAllByType("button").find(b=>textOf(b).includes("Captured details"))!.props.onClick());
 expect(textOf(heading())).toContain(`sha256 ${"c".repeat(64)}`);
 act(()=>tree!.root.findAllByType("button").find(b=>b.children.join("")==="source")!.props.onClick());
 // The actual editor receives readOnly, including its fallback textarea.
 expect(tree!.root.findByType(SpecSourceEditor).props.readOnly).toBe(true);
 expect(tree!.root.findByType(SpecSourceEditor).props.source).toBe(JSON.stringify(descriptor));
 expect(submit).not.toHaveBeenCalled();
 expect(calls.filter(c=>c.url==="/api-library").map(c=>JSON.parse(c.init.body).action)).toEqual(["list"]);
 act(()=>tree!.root.findAllByType("button").find(b=>b.children.join("").includes("Back to library"))!.props.onClick());
 expect(tree!.root.findByProps({"aria-label":"Open imported prices in lab"})).toBeDefined();
});

const txt=(n:ReactTestInstance|string):string=>typeof n==="string"?n:n.children.map(txt).join("");
const buttonText=(text:string)=>tree!.root.findAllByType("button").find(b=>txt(b)===text)!;
const byLabel=(label:string)=>tree!.root.findByProps({"aria-label":label});

it("lists workspace imports by environment and alias, expands every row and opens the exact capture",async()=>{
 const binding={workspace:"synthetic-sensor",generation:"g1"};
 const lab={environment:"lab",alias:"prices",origin:"/synthetic/specs/prices/v1/openapi.json",revision:"a".repeat(64),bytes:43008};
 const prod={...lab,environment:"prod",origin:"",revision:"b".repeat(64),bytes:80};
 const draft={key:{service:"inventory",apiVersion:"v1",scope:"all"},revision:"c".repeat(64),accepted:false,origin:"fixture",valid:true,sourceDigest:null,descriptorRevision:null};
 const requests:string[]=[];
 vi.stubGlobal("fetch",vi.fn(async(url:string,init:any)=>{requests.push(url==="/api-library"?JSON.parse(init.body).action:url);
  return {ok:true,json:async()=>url==="/api-library"?{packages:[],drafts:[draft]}:url.includes("alias=")?{...binding,spec:prod,source:"{}"}:{...binding,specs:[lab,prod]}};}));
 await act(async()=>{tree=create(<SpecScreen top={[]} binding={binding} onClose={vi.fn()} onSubmit={vi.fn()}/>);});
 const table=byLabel("Imports in synthetic-sensor");
 const rows=table.findAll(n=>n.props.role==="row"&&n.findAll(c=>c.props.role==="cell").length>1);
 // The revision is the digest's start, never an ordinal; the whole digest and source are accessible descriptions.
 expect(rows.map(r=>r.findAll(c=>c.props.role==="cell").map(txt))).toEqual([
  ["▸","prices…/v1/openapi.json","lab","aaaaaaaa","42 KB","…/v1/openapi.json","open"],
  ["▸","pricesnot recorded","prod","bbbbbbbb","80 bytes","not recorded","open"]]);
 expect(rows[0]!.findAll(n=>n.props["aria-description"]===`sha256:${"a".repeat(64)}`)).toHaveLength(1);
 expect(txt(byLabel("Workspace APIs").findByType("header"))).toContain("Workspace APIs·2");
 act(()=>buttonText("expand all").props.onClick());
 for (const label of ["Collapse prices in lab","Collapse prices in prod","Collapse inventory version v1"]) expect(byLabel(label).props["aria-expanded"]).toBe(true);
 for (const cell of table.findAllByProps({className:"spec-home-detail"})) expect(cell.props).toMatchObject({role:"cell","aria-colspan":7});
 const details=table.findAllByProps({className:"spec-home-detail"}).map(txt);
 expect(details[0]).toContain(lab.origin);expect(details[0]).toContain(`sha256:${"a".repeat(64)}`);
 expect(details[1]).toContain(`sha256:${"b".repeat(64)}`);
 expect(details[0]).toContain("It doesn’t call the API");
 act(()=>buttonText("collapse all").props.onClick());
 expect(tree!.root.findAllByProps({className:"spec-home-detail"})).toHaveLength(0);
 expect(requests).toEqual(["list","/workspace-specs?"]); // expanding reads nothing and opens nothing
 await act(async()=>byLabel("Open imported prices in prod").props.onClick());
 expect(requests.at(-1)).toBe(`/workspace-specs?environment=prod&alias=prices&revision=${"b".repeat(64)}`);
 expect(requests).not.toContain("inspect");
 expect(byLabel("Imported API snapshot")).toBeDefined();
});

it("keeps each section's read its own: a failed library read is neither empty nor hides workspace imports",async()=>{
 const binding={workspace:"lab",generation:"g1"};
 const spec={environment:"default",alias:"demo",origin:"spec.json",revision:"d".repeat(64),bytes:30};
 let libraryFails=true;
 vi.stubGlobal("fetch",vi.fn(async(url:string)=>url==="/api-library"
  ?libraryFails?{ok:false,text:async()=>"Synthetic library outage"}:{ok:true,json:async()=>({packages:[],drafts:[]})}
  :{ok:true,json:async()=>({...binding,specs:[spec]})}));
 await act(async()=>{tree=create(<SpecScreen top={[]} binding={binding} onClose={vi.fn()} onSubmit={vi.fn()}/>);});
 const library=()=>byLabel("API library");
 expect(txt(library().findByProps({role:"alert"}))).toContain("Couldn’t read the library.");
 expect(txt(library())).toContain("Synthetic library outage");
 expect(txt(library())).toContain("Library·—");
 expect(txt(library())).not.toContain("No APIs yet");
 expect(byLabel("Open imported demo in default")).toBeDefined();
 expect(tree!.root.findAllByType("form")[0]!.props.hidden).toBe(true); // an unread library is not an empty one
 libraryFails=false;
 await act(async()=>library().findAllByType("button").find(b=>txt(b)==="try again")!.props.onClick());
 expect(txt(library())).toContain("No APIs yet. Import OpenAPI below to create an editable draft.");
 expect(tree!.root.findAllByType("form")[0]!.props.hidden).toBe(false);
 expect(byLabel("Import OpenAPI").props.className).toBe("spec-home-card option spec-home-card-accent");
 expect(tree!.root.findAllByProps({role:"status"})).toHaveLength(0);
});

it("validates the describe name, keeps typed values across collapse and sends only the existing command",async()=>{
 vi.stubGlobal("fetch",vi.fn(async()=>({ok:true,json:async()=>({packages:[],drafts:[]})})));
 const submit=vi.fn(()=>undefined);
 await act(async()=>{tree=create(<SpecScreen top={[]} onClose={vi.fn()} onSubmit={submit}/>);});
 const form=()=>tree!.root.findAllByType("form")[0]!;
 act(()=>byLabel("OpenAPI source").props.onChange({target:{value:"./specs/acme-openapi.yaml"}}));
 act(()=>byLabel("Describe provider").props.onChange({target:{value:"acme-api"}}));
 expect(byLabel("Describe provider").props["aria-invalid"]).toBe(true);
 expect(txt(form())).toContain("Use letters, digits and _; start with a letter or _.");
 expect(buttonText("create draft").props.disabled).toBe(true);
 await act(async()=>form().props.onSubmit({preventDefault(){}}));
 expect(submit).not.toHaveBeenCalled();
 act(()=>byLabel("Describe provider").props.onChange({target:{value:"acme"}}));
 expect(byLabel("Describe provider").props["aria-invalid"]).toBe(false);
 act(()=>byLabel("Collapse OpenAPI import").props.onClick());
 expect(form().props.hidden).toBe(true);
 expect(txt(byLabel("Import OpenAPI").findByType("header"))).toContain("· unsent: ./specs/acme-openapi.yaml · acme");
 act(()=>byLabel("Expand OpenAPI import").props.onClick());
 expect(byLabel("OpenAPI source").props.value).toBe("./specs/acme-openapi.yaml");
 expect(byLabel("Describe provider").props.value).toBe("acme");
 await act(async()=>form().props.onSubmit({preventDefault(){}}));
 expect(submit).toHaveBeenCalledExactlyOnceWith(':describe file:"./specs/acme-openapi.yaml" provider:acme');
 expect(tree!.root.findAllByProps({role:"status"})).toHaveLength(0); // nothing claims success before the session publishes a draft
});

it("reports stale workspace captures without displaying mismatched source",async()=>{
 const binding={workspace:"lab",generation:"g1"};
 const spec={environment:"default",alias:"demo",origin:"spec.json",revision:"d".repeat(64),bytes:30};
 vi.stubGlobal("fetch",vi.fn(async(url:string)=>({ok:true,json:async()=>url==="/api-library"?{packages:[],drafts:[]}:url.includes("alias=")?{workspace:"another",generation:"g2",spec,source:"{}"}:{...binding,specs:[spec]}})));
 await act(async()=>{tree=create(<SpecScreen top={[]} binding={binding} onClose={vi.fn()} onSubmit={vi.fn()}/>);});
 await act(async()=>{tree!.root.findByProps({"aria-label":"Open imported demo in default"}).props.onClick();});
 expect(tree!.root.findByProps({role:"status"}).children.join("")).toContain("Workspace changed");
 expect(tree!.root.findAllByProps({"aria-label":"Imported API snapshot"})).toHaveLength(0);
});
