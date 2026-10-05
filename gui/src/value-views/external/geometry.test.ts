import {afterEach,expect,it,vi} from "vitest";
import {slotClip} from "../../../../packages/view-sdk/geometry";
afterEach(()=>vi.unstubAllGlobals());
it("clips scrolled member overlays to the parent viewport, including wholly hidden tracks",()=>{
  vi.stubGlobal("window",{innerWidth:1000,innerHeight:700});
  vi.stubGlobal("getComputedStyle",()=>({overflowX:"hidden",overflowY:"auto"}));
  const parent={parentElement:null,clientLeft:1,clientTop:1,clientWidth:400,clientHeight:300,getBoundingClientRect:()=>({x:10,y:100})};
  const element={parentElement:parent} as unknown as HTMLElement;
  expect(slotClip(element,{x:11,y:50,width:400,height:200})).toEqual({x:11,y:101,width:400,height:149});
  expect(slotClip(element,{x:11,y:450,width:400,height:200}).height).toBe(0);
});
