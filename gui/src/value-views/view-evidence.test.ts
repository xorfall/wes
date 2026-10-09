import {expect,it,vi} from "vitest";
import {FrameEvidence,ViewEvidenceError} from "@wes/view-sdk";
import {EvidenceBridge} from "./view-evidence";
import type {ViewBinding} from "./view-bindings";
it("limits evidence reads and cancels old replies without replay",async()=>{
  const send=vi.fn(),sdk=new FrameEvidence(send,()=>4);
  const first=sdk.read("members",0),second=sdk.read("members",1);
  await expect(sdk.read("members",2)).rejects.toEqual(new ViewEvidenceError("busy"));
  await expect(sdk.read("../other")).rejects.toEqual(new ViewEvidenceError("invalid"));
  sdk.cancel();await expect(first).rejects.toEqual(new ViewEvidenceError("changed"));await expect(second).rejects.toEqual(new ViewEvidenceError("changed"));
  sdk.settle({request:1,ok:true,result:{available:true}});expect(send).toHaveBeenCalledTimes(2);
});
it("supplies only the host binding and discards late response after revocation",async()=>{
  let resolve!:(value:unknown)=>void;const pending=new Promise(r=>resolve=r),send=vi.fn(),read=vi.fn(()=>pending);
  const binding={authorityEpoch:"realm",engine:{readViewEvidence:read}} as unknown as ViewBinding;
  const bridge=new EvidenceBridge(send);
  bridge.handle({kind:"evidence-read",request:1,epoch:0,slot:"members",ordinal:0},binding);
  expect(read).toHaveBeenCalledWith(binding,"members",0,expect.any(AbortSignal));
  bridge.reset();resolve({available:true});await Promise.resolve();
  expect(send).toHaveBeenCalledExactlyOnceWith({kind:"evidence-reply",request:1,ok:false,error:"changed"});
  bridge.handle({kind:"evidence-read",request:2,epoch:1,slot:"members",ordinal:0,handle:"other"},binding);
  expect(read).toHaveBeenCalledOnce();
});
