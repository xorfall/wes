import { afterEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { EnvScreen, type Environment } from "./screens/Env";
import { authenticationRequest, type AuthenticationReport } from "../environment-authentication";
import { applicationLog } from "../application-log";
let tree:ReactTestRenderer|undefined;
afterEach(()=>{act(()=>tree?.unmount());tree=undefined;vi.unstubAllGlobals();vi.useRealTimers();});
const binding={workspace:"auth-fixture",generation:"g1"};
function report():AuthenticationReport {return {...binding,providers:[{environment:"default",revision:"r1",provider:"demo",enabled:true,grantSeconds:0,credentials:[],operations:[{operation:["latest"],state:"selection-required",selected:null,options:[{schemes:["key","secret"],credentialSlots:["key","secret"],methods:["header","header"]},{schemes:["basic"],credentialSlots:["basic.username","basic.password"],methods:["basic"]}]}]}]};}
const environments=(execution=true):Environment[]=>[{name:"default",revision:"r1",execution,providers:[{name:"demo",writes:false,credentials:[]},{name:"sh",writes:true,credentials:[]}]}];
const screen=(execution=true)=><EnvScreen top={[]} environments={environments(execution)} chosen="default" authentication={binding}/>;
const text=()=>JSON.stringify(tree!.toJSON());
const button=(label:string)=>tree!.root.findAllByType("button").find(b=>b.children.join("")===label)!;
/** Mounts the screen and opens demo's row, where its setup lives. */
async function mount(execution=true){
 await act(async()=>{tree=create(screen(execution));});
 act(()=>tree!.root.findByProps({"aria-label":"Show demo setup"}).props.onClick());
}
const textOf=(node:unknown):string=>typeof node==="string"?node:(node as {children:unknown[]}).children.map(textOf).join("");
const option=(label:string)=>tree!.root.findByProps({"aria-label":"demo latest authentication method"}).findAllByType("button").find(b=>textOf(b).includes(label))!;
const fixedKey=(state:AuthenticationReport,present:boolean)=>{const p=state.providers[0]!;p.credentials=[{slot:"key",reference:"ref",present}];p.operations=[{operation:["latest"],state:"fixed",selected:null,options:[{schemes:[],credentialSlots:["key"],methods:["header"]}]}];return p;};

it("selects methods, sends masked credentials outside source, grants and revokes with exact binding",async()=>{
 let state=report();const requests:any[]=[];
 vi.stubGlobal("fetch",vi.fn(async(url,init)=>{
   expect(url).toBe("/environment-authentication");expect(init.headers["X-Wes-Workspace"]).toBe(binding.workspace);expect(init.headers["X-Wes-Session"]).toBe(binding.generation);
   if(init.method==="GET")return {ok:true,json:async()=>structuredClone(state)};
   const body=JSON.parse(init.body);requests.push(body);const p=state.providers[0]!;
   if(body.action==="configure"){p.revision="r2";p.operations[0]!.selected=["key","secret"];p.operations[0]!.state="selected";p.credentials=[{slot:"key",reference:"ref1",present:false},{slot:"secret",reference:"ref2",present:false}];}
   if(body.action==="supply")p.credentials.find(c=>c.slot===body.slot)!.present=true;
   if(body.action==="grant")p.grantSeconds=299;
   if(body.action==="revoke")p.grantSeconds=0;
   return {ok:true,json:async()=>({...binding,applied:true})};
 }));
 await mount();
 expect(tree!.root.findAllByType("select")).toHaveLength(0);
 act(()=>option("key + secret").props.onClick());
 expect(option("key + secret").props["aria-pressed"]).toBe(true);
 expect(option("basic").props["aria-pressed"]).toBe(false);
 await act(async()=>{button("save authentication").props.onClick();});
 expect(requests[0]).toMatchObject({action:"configure",environment:"default",revision:"r1",provider:"demo",auth:{latest:["key","secret"]}});
 expect(button("allow for 5 minutes").props.disabled).toBe(true);
 for(const slot of ["key","secret"]){
   const input=tree!.root.findByProps({"aria-label":`${slot} credential`});expect(input.props.type).toBe("password");
   act(()=>input.props.onChange({target:{value:`private-${slot}`}}));
   await act(async()=>{input.parent!.parent!.props.onSubmit({preventDefault(){}});});
   expect(tree!.root.findByProps({"aria-label":`${slot} credential`}).props.value).toBe("");
 }
 expect(button("allow for 5 minutes").props.disabled).toBe(false);
 await act(async()=>{button("allow for 5 minutes").props.onClick();});
 expect(text()).toContain("remaining");
 await act(async()=>{button("revoke access").props.onClick();});
 expect(requests.at(-1)).toEqual({environment:"default",revision:"r2",provider:"demo",action:"revoke"});
 expect(requests.every(r=>r.action!=="submit")).toBe(true);
 expect(text()).not.toContain("private-key");
});

it("takes an unsaved method choice back when the pressed option is chosen again",async()=>{
 vi.stubGlobal("fetch",vi.fn(async()=>({ok:true,json:async()=>structuredClone(report())})));
 await mount();
 act(()=>option("basic").props.onClick());
 expect(text()).toContain("unsaved choice");
 act(()=>option("basic").props.onClick());
 expect(option("basic").props["aria-pressed"]).toBe(false);
 expect(text()).not.toContain("unsaved choice");
 expect(button("save authentication").props.disabled).toBe(true);
});

it("clears credential input even on failure and never echoes a hostile response to Logs",async()=>{
 let state=report();fixedKey(state,false);
 const log=vi.spyOn(applicationLog,"add");
 vi.stubGlobal("fetch",vi.fn(async(_url,init)=>init.method==="GET"?{ok:true,json:async()=>state}:{ok:false,text:async()=>"private-submitted-value"}));
 await mount();
 const input=tree!.root.findByProps({"aria-label":"key credential"});act(()=>input.props.onChange({target:{value:"private-submitted-value"}}));
 await act(async()=>input.parent!.parent!.props.onSubmit({preventDefault(){}}));
 expect(input.props.value).toBe("");expect(text()).not.toContain("private-submitted-value");expect(JSON.stringify(log.mock.calls)).not.toContain("private-submitted-value");
 expect(tree!.root.findByProps({role:"status"}).children.join("")).toContain("could not be confirmed");log.mockRestore();
});

it("rejects a response from another workspace",async()=>{
 vi.stubGlobal("fetch",vi.fn(async()=>({ok:true,json:async()=>({...report(),workspace:"other"})})));
 await expect(authenticationRequest(binding)).rejects.toThrow("Workspace changed");
});

it("keeps environment execution and where it runs outside the provider setup",async()=>{
 const state=report();
 vi.stubGlobal("fetch",vi.fn(async()=>({ok:true,json:async()=>structuredClone(state)})));
 state.providers[0]!.enabled=false;
 await mount();
 const steps=()=>tree!.root.findAll(n=>typeof n.props["aria-label"]==="string"&&/^\d \w/.test(n.props["aria-label"])&&typeof n.type==="string"&&String(n.props.className).startsWith("env-step")).map(n=>n.props["aria-label"]);
 // Nothing is chosen yet: only the method is current; execution belongs to the environment, runs on to the table.
 expect(steps()).toEqual(["1 Method, current","2 Credentials, waiting"]);
 expect(tree!.root.findAllByType("button").some(b=>b.children.join("")==="enable environment")).toBe(false);
});

it("lists every provider once and opens setup only in an authenticated provider's own row",async()=>{
 vi.stubGlobal("fetch",vi.fn(async()=>({ok:true,json:async()=>structuredClone(report())})));
 await act(async()=>{tree=create(screen());});
 const headers=tree!.root.findAllByProps({role:"columnheader"}).map(h=>h.children.join(""));
 expect(headers).toEqual(["provider","kind","runs on → contacts","credentials","access"]);
 const cells=tree!.root.findAllByProps({role:"cell"}).map(textOf);
 expect(cells.filter(c=>c.endsWith("demo"))).toHaveLength(1);
 expect(cells.filter(c=>c==="sh")).toHaveLength(1);
 expect(text()).toContain("choose a method");
 expect(text()).toContain("not reported");
 expect(tree!.root.findAllByProps({"aria-label":"Show sh setup"})).toHaveLength(0);
 expect(tree!.root.findAllByProps({"aria-label":"demo authentication"})).toHaveLength(0);
 act(()=>tree!.root.findByProps({"aria-label":"Show demo setup"}).props.onClick());
 const detail=tree!.root.findByProps({className:"value-table-detail"});
 expect(detail.findAllByProps({"aria-label":"demo authentication"})).toHaveLength(1);
});

it("shows only the failure when the setup cannot be read",async()=>{
 vi.stubGlobal("fetch",vi.fn(async()=>({ok:false,text:async()=>"Authentication is unavailable"})));
 await act(async()=>{tree=create(screen());});
 expect(text()).toContain("Authentication is unavailable");
 expect(text()).not.toContain("Loading");
 // The providers the environment declares stay listed.
 expect(tree!.root.findAllByProps({role:"cell"}).some(c=>textOf(c).includes("demo"))).toBe(true);
});

it("refresh never invents access between timer ticks or supplies credentials", async () => {
  vi.useFakeTimers();
  const state = report();
  fixedKey(state, false);
  const fetch = vi.fn(async () => ({ ok: true, json: async () => structuredClone(state) }));
  vi.stubGlobal("fetch", fetch);
  await mount();
  for (let i = 0; i < 3; i++) {
    await act(async () => { vi.advanceTimersByTime(250); });
    await act(async () => { await tree!.root.findByProps({ "aria-label": "Refresh setup" }).props.onClick(); });
    expect(tree!.root.findByProps({ "aria-label": "3 Access, waiting" })).toBeDefined();
    expect(text()).not.toContain("granted ·");
    expect(button("revoke access").props.disabled).toBe(true);
    expect(button("allow for 5 minutes").props.disabled).toBe(true);
  }
  expect(fetch).toHaveBeenCalledTimes(4);
  for (const call of fetch.mock.calls as unknown as [string, RequestInit][]) {
    expect(call[1].method).toBe("GET");
    expect(call[1].body).toBeUndefined();
  }
});

it("refresh cannot increase reported grant duration or resurrect an expired grant", async () => {
  vi.useFakeTimers();
  const state = report();
  const provider = fixedKey(state, true);
  provider.grantSeconds = 2;
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => structuredClone(state) })));
  await mount();
  await act(async () => { vi.advanceTimersByTime(250); });
  await act(async () => { await tree!.root.findByProps({ "aria-label": "Refresh setup" }).props.onClick(); });
  expect(text()).toContain("granted · 2s remaining");
  expect(text()).not.toContain("3s remaining");
  await act(async () => { vi.advanceTimersByTime(3000); });
  expect(text()).not.toContain("granted ·");
  provider.grantSeconds = 0;
  await act(async () => { await tree!.root.findByProps({ "aria-label": "Refresh setup" }).props.onClick(); });
  expect(tree!.root.findByProps({ "aria-label": "3 Access, current" })).toBeDefined();
  expect(text()).not.toContain("granted ·");
});

it("refreshes access readiness when the environment execution state changes", async () => {
  const state = report();
  const provider = fixedKey(state, true);
  provider.enabled = false;
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => structuredClone(state) })));
  await mount(false);
  expect(button("allow for 5 minutes").props.disabled).toBe(true);
  expect(text()).toContain("Enable execution on the environment card");
  provider.enabled = true;
  await act(async () => { tree!.update(screen(true)); });
  expect(button("allow for 5 minutes").props.disabled).toBe(false);
  expect(text()).not.toContain("Enable execution on the environment card");
  expect(text()).not.toContain("granted ·");
});

it("keeps device persistence opt-in and displays the server's saved status", async () => {
 const state=report(), p=fixedKey(state,true); p.persistenceSupported=true;
 p.credentials=[{slot:"key",reference:"r",present:true,saved:true}];
 const requests:any[]=[];
 vi.stubGlobal("fetch",vi.fn(async(_url,init)=>{ if(init.method==="GET")return {ok:true,json:async()=>structuredClone(state)}; requests.push(JSON.parse(init.body)); return {ok:true,json:async()=>({...binding,applied:true})}; }));
 await mount();
 expect(text()).toContain("saved on device");
 expect(text()).toContain("Keys are session-only unless you choose Remember on this device.");
 const checkbox=tree!.root.findByProps({type:"checkbox"}); expect(checkbox.props.checked).toBe(false);
 act(()=>checkbox.props.onChange({target:{checked:true}}));
 const input=tree!.root.findByProps({"aria-label":"key credential"}); act(()=>input.props.onChange({target:{value:"synthetic"}}));
 await act(async()=>input.parent!.parent!.props.onSubmit({preventDefault(){}}));
 expect(requests[0]).toMatchObject({action:"supply",remember:true,value:"synthetic"});
 expect(input.props.value).toBe("");
 expect(text()).not.toContain("synthetic");
});

it("counts the report's credential presence on the card and marks missing ones in words", async () => {
 const state=report(); fixedKey(state,false);
 vi.stubGlobal("fetch",vi.fn(async()=>({ok:true,json:async()=>structuredClone(state)})));
 await act(async()=>{tree=create(<EnvScreen top={[]} environments={environments()} chosen="" authentication={binding}/>);});
 expect(text()).toContain("credentials 0/1");
 const chip=tree!.root.findByProps({"aria-label":"demo, credentials missing"});
 expect(chip.props.className).toBe("env-chip");
 expect(tree!.root.findByProps({"aria-label":"sh, can change real systems"})).toBeDefined();
});

it("discards a late action response after moving to another workspace generation", async()=>{
 const {useEnvironmentAuthentication}=await import('./EnvironmentAuthentication');
 let state!:ReturnType<typeof useEnvironmentAuthentication>;
 function Probe({owner}:{owner:typeof binding}){state=useEnvironmentAuthentication(owner,'r1');return <span>{state.report?.workspace??'loading'}</span>;}
 let finish!:(response:unknown)=>void;
 const fetch=vi.fn(async(_url,init)=>{
  const owner={workspace:decodeURIComponent(init.headers['X-Wes-Workspace']),generation:init.headers['X-Wes-Session']};
  if(init.method==='POST')return new Promise(resolve=>{finish=resolve;});
  return {ok:true,json:async()=>({...report(),...owner})};
 });vi.stubGlobal('fetch',fetch);
 await act(async()=>{tree=create(<Probe owner={binding}/>);});
 let pending!:Promise<void>;
 act(()=>{pending=state.change({action:'revoke',environment:'default',revision:'r1',provider:'demo'});});
 await act(async()=>{tree!.update(<Probe owner={{workspace:'neighbor',generation:'g2'}}/>);});
 expect(state.report?.workspace).toBe('neighbor');
 const before=fetch.mock.calls.length;
 await act(async()=>{finish({ok:true,json:async()=>({...binding,applied:true})});await pending;});
 expect(state.report?.workspace).toBe('neighbor');expect(fetch.mock.calls).toHaveLength(before);
});

/** The env host plus a synthetic vault; every request is recorded in order. */
function vaultHost(state:AuthenticationReport,vault:{kind:string;state?:string}){
 const calls:{url:string;body?:any}[]=[];
 vi.stubGlobal("fetch",vi.fn(async(url:string,init:any)=>{
   const body=init.body?JSON.parse(init.body):undefined;calls.push({url,body});
   if(url==="/credential-vault"){
     if(body?.action==="create"||body?.action==="unlock")vault={kind:"vault",state:"unlocked"};
     return {ok:true,json:async()=>structuredClone(vault)};
   }
   if(init.method==="GET")return {ok:true,json:async()=>structuredClone(state)};
   return {ok:true,json:async()=>({...binding,applied:true})};
 }));
 return calls;
}

it("should_create_the_vault_in_the_credential_form_before_remembering_a_value",async()=>{
 // Arrange
 const state=report();fixedKey(state,false).persistenceSupported=true;
 const calls=vaultHost(state,{kind:"vault",state:"absent"});
 await mount();
 act(()=>tree!.root.findByProps({type:"checkbox"}).props.onChange({target:{checked:true}}));
 act(()=>tree!.root.findByProps({"aria-label":"key credential"}).props.onChange({target:{value:"synthetic-remembered"}}));
 const supply=()=>button("supply");

 // Act
 const withoutPassword=supply().props.disabled;
 act(()=>tree!.root.findByProps({"aria-label":"Vault password"}).props.onChange({target:{value:"synthetic password"}}));
 act(()=>tree!.root.findByProps({"aria-label":"Repeat vault password"}).props.onChange({target:{value:"synthetic password"}}));
 await act(async()=>tree!.root.findByProps({"aria-label":"key credential"}).parent!.parent!.props.onSubmit({preventDefault(){}}));

 // Assert
 expect(withoutPassword).toBe(true);
 const posts=calls.filter(c=>c.body).map(c=>({url:c.url,action:c.body.action}));
 expect(posts).toEqual([{url:"/credential-vault",action:"create"},{url:"/environment-authentication",action:"supply"}]);
 expect(calls.find(c=>c.body?.action==="supply")!.body).toMatchObject({remember:true,value:"synthetic-remembered"});
 expect(tree!.root.findAllByProps({"aria-label":"Vault password"})).toHaveLength(0);
 expect(text()).not.toContain("synthetic password");
});

it("should_unlock_a_locked_vault_where_its_credentials_are_needed_and_reread_the_setup",async()=>{
 // Arrange
 const state=report();fixedKey(state,false).persistenceSupported=true;
 const calls=vaultHost(state,{kind:"vault",state:"locked"});
 await mount();
 const locked=text().includes("Remembered credentials are locked.");
 const reads=calls.filter(c=>c.url==="/environment-authentication"&&!c.body).length;

 // Act
 const form=tree!.root.findByProps({"aria-label":"Unlock credential vault"});
 act(()=>form.findByProps({"aria-label":"Vault password"}).props.onChange({target:{value:"synthetic password"}}));
 await act(async()=>tree!.root.findByProps({"aria-label":"Unlock credential vault"}).props.onSubmit({preventDefault(){}}));

 // Assert
 expect(locked).toBe(true);
 expect(calls.filter(c=>c.body).map(c=>c.body)).toEqual([{action:"unlock",password:"synthetic password"}]);
 expect(calls.filter(c=>c.url==="/environment-authentication"&&!c.body).length).toBe(reads+1);
 expect(text()).not.toContain("Remembered credentials are locked.");
});

it("should_not_read_a_vault_when_no_provider_can_remember_credentials",async()=>{
 // Arrange
 const state=report();fixedKey(state,false);
 const calls=vaultHost(state,{kind:"vault",state:"locked"});

 // Act
 await mount();

 // Assert
 expect(calls.some(c=>c.url==="/credential-vault")).toBe(false);
 expect(text()).not.toContain("Remembered credentials are locked.");
});
