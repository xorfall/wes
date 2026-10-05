import {expect,it} from "vitest";
import {frameHeight,layoutTier} from "./layout";
import type {ViewLayout} from "../../../packages/view-sdk/contract";
const tier={min:{columns:32,rows:4},preferred:{columns:80,rows:7},max:{columns:320,rows:120}};
const layout:ViewLayout={preview:{...tier,max:{columns:320,rows:8}},expanded:tier,window:tier};
it("honors declared maximum and measured leading without a viewport-dependent cap or artificial child minimum",()=>{
 expect(frameHeight(layout,"window",2000,20)).toBe(2000);
 expect(frameHeight(layout,"window",5000,20)).toBe(2400);
 expect(frameHeight(layout,"expanded",5000,20)).toBe(2400);
 expect(frameHeight(layout,"preview",24,20)).toBe(24);
 expect(frameHeight(layout,"window",5000,NaN)).toBe(2520);
 expect(layoutTier(undefined,"expanded").preferred.rows).toBe(14);
});
it("fits the complete selected preview instead of clipping its chrome to an allocation hint",()=>{
 expect(frameHeight(layout,"preview",243,19.5)).toBe(243);
 expect(frameHeight(layout,"preview",167,19.5)).toBe(167);
 expect(frameHeight(layout,"preview",287,19.5)).toBe(287);
 expect(frameHeight(layout,"preview",25000,20)).toBe(20000);
 for(const height of [-1,NaN,Infinity])expect(frameHeight(layout,"preview",height,20)).toBe(0);
});
it('accepts tier placement policies and rejects malformed requests instead of passing them to consumers',()=>{
 const placed={...tier,placement:{width:'preferred' as const,align:'center' as const}};
 expect(layoutTier({...layout,expanded:placed},'expanded')).toBe(placed);
 for(const placement of [{width:'fixed',align:'center'},{width:'fill',align:'left'},{width:'fill'}])
   expect(layoutTier({...layout,expanded:{...tier,placement}} as ViewLayout,'expanded')).not.toEqual({...tier,placement});
});
