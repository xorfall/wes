import {expect,it} from "vitest";
import {AssetBudget} from "./assets";
import {MessageRate} from "./message-rate";
import type {ViewAsset} from "./document";
const asset=(digest:string,javascript="",css="")=>({digest,javascript,css} as ViewAsset);
it("charges shipped and installed assets once in the same finite count and byte budget",()=>{
  const count=new AssetBudget();
  count.retain(asset("shipped"));for(let i=0;i<100;i++)count.retain(asset("shipped"));
  for(let i=0;i<63;i++)count.retain(asset(`installed-${i}`));
  expect(()=>count.retain(asset("next"))).toThrow("64");
  const bytes=new AssetBudget(),chunk="x".repeat(4*1024*1024);
  expect(()=>bytes.retain(asset("bad-css","","é".repeat(128*1024+1)))).toThrow("package size");
  for(let i=0;i<8;i++)bytes.retain(asset(String(i),chunk));
  expect(()=>bytes.retain(asset("overflow","x"))).toThrow("32 MiB");
});
it("allows continuous cursor/draft traffic alongside drawing control, but bounds both lanes",()=>{
  const rate=new MessageRate();
  for(let i=0;i<600;i++){
    rate.take("event",i*1000/60);rate.take("event",i*1000/60);
    if(i%6===0){rate.take("ack",i*1000/60);rate.take("geometry",i*1000/60);}
  }
  for(const kind of ["event","geometry"]){const flood=new MessageRate();for(let i=0;i<120;i++)flood.take(kind,0);expect(()=>flood.take(kind,0)).toThrow("rate");}
});
