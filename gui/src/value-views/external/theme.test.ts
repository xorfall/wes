import {afterEach,expect,it,vi} from "vitest";
import {readFileSync,readdirSync} from "node:fs";
import {roleStyles,themeTokenNames,viewTheme} from "@wes/view-sdk/theme";
import {currentTheme,followTheme} from "./theme";
import {variables,role} from "../../surface/cascade.test-support";

afterEach(()=>{vi.useRealTimers();vi.unstubAllGlobals();});

it("shipped renderer styling uses only variables supplied by the public theme bridge",()=>{
  const root=new URL("../../../../views/",import.meta.url);
  for(const entry of readdirSync(root,{recursive:true})){
    if(typeof entry!=="string"||!/[.](css|tsx)$/.test(entry))continue;
    const source=readFileSync(new URL(entry,root),"utf8");
    for(const match of source.matchAll(/var\((--[\w-]+)/g))expect(themeTokenNames,`${entry}: ${match[1]}`).toContain(match[1]);
  }
});

it("shares public role bindings with Surface and discovers their variables without exporting private roles",()=>{
  const tokens=variables({palette:"paper",density:"normal"});
  for(const name of themeTokenNames)expect(tokens.has(name),name).toBe(true);
  for(const [name,item] of Object.entries(viewTheme.roles)){
    expect(item.usage).not.toBe("");
    expect(roleStyles()).toContain(`.${name}{`);
    const resolved=role(name,{palette:"paper",density:"normal"});
    for(const [property,value] of Object.entries(item.style))
      expect(resolved.get(property)).toBe(value.replace(/var\((--[\w-]+)\)/g,(_,token:string)=>tokens.get(token)!));
  }
  expect(roleStyles()).not.toContain("frame-focus");
  const privateCss=readFileSync(new URL("../../surface/roles.css",import.meta.url),"utf8");
  for(const name of Object.keys(viewTheme.roles))expect(privateCss).not.toContain(`.${name}`);
});

it("follows each client palette, density and font change, coalesces mutations and releases its subscription",()=>{
  vi.useFakeTimers();
  let mutated=()=>{};
  const observed:unknown[]=[];
  const disconnect=vi.fn();
  vi.stubGlobal("MutationObserver",class{
    constructor(callback:()=>void){mutated=callback;}
    observe(...args:unknown[]){observed.push(args);}
    disconnect=disconnect;
  });
  vi.stubGlobal("requestAnimationFrame",(callback:()=>void)=>setTimeout(callback,1));
  vi.stubGlobal("cancelAnimationFrame",clearTimeout);
  let values=variables({palette:"paper",density:"normal"});
  vi.stubGlobal("getComputedStyle",()=>({getPropertyValue:(name:string)=>values.get(name)??""}));
  const parent={parentElement:null},element={parentElement:parent} as unknown as Element;
  const changed=vi.fn(),close=followTheme(element,changed);
  expect(observed).toHaveLength(2);
  expect(changed.mock.calls[0]![0]).toContain("--surface:#F4F2EC");
  for(const palette of ["ink","white","paper"] as const){
    values=variables({palette,density:"dense"});mutated();mutated();vi.advanceTimersByTime(1);
    const css=changed.mock.lastCall![0];
    expect(css).toContain(`--surface:${values.get("--surface")}`);
    expect(css).toContain("--space-md:12px");
    expect(css).toContain("--type-mono-leading:1.4");
  }
  const before=changed.mock.calls.length;
  values.set("--type-mono-family",'"Menlo", monospace');
  values.set("--type-sans-family",'"Avenir Next", sans-serif');
  mutated();vi.advanceTimersByTime(1);
  expect(changed).toHaveBeenCalledTimes(before+1);
  expect(changed.mock.lastCall![0]).toContain('"Avenir Next", sans-serif');
  mutated();vi.advanceTimersByTime(1);expect(changed).toHaveBeenCalledTimes(before+1);
  mutated();close();vi.advanceTimersByTime(1);
  expect(disconnect).toHaveBeenCalledOnce();expect(changed).toHaveBeenCalledTimes(before+1);
});


it("keeps the client palette when a covered session has unresolved inherited tokens",()=>{
  const tokens=variables({palette:"ink",density:"normal"});
  const client={} as Element;
  const element={closest:()=>client} as unknown as Element;
  vi.stubGlobal("getComputedStyle",(target:Element)=>({getPropertyValue:(name:string)=>target===client ? tokens.get(name)??"" : ""}));
  expect(currentTheme(element)).toContain(`--surface:${tokens.get("--surface")}`);
  expect(currentTheme(element)).toContain(`--on-surface:${tokens.get("--on-surface")}`);
});
