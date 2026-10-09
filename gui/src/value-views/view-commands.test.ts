import {describe,it,expect,vi} from "vitest";
import {CommandBridge,type CommandReview} from "./view-commands";
import type {ViewBinding} from "./view-bindings";
import {FrameCommands,ViewCommandError,commandRequest} from "@wes/view-sdk";
describe("prepare-only View commands",()=>{
  it("bounds pending requests and rejects changes without replay",async()=>{
    const send=vi.fn(),commands=new FrameCommands(send,()=>3);
    const first=commands.prepare("publish",{body:'text\"\n:drop $other'});
    await expect(commands.prepare("publish",{})).rejects.toEqual(new ViewCommandError("busy"));
    expect(send.mock.calls[0]![0]).toMatchObject({kind:"command-prepare",epoch:3});
    commands.cancel();await expect(first).rejects.toEqual(new ViewCommandError("changed"));
    commands.settle({request:1,ok:true});expect(send).toHaveBeenCalledTimes(1);
    expect(commandRequest("provider path",{})).toBe(false);
    expect(commandRequest("publish",{value:9007199254740992})).toBe(false);
    expect(commandRequest("publish",{value:null})).toBe(false);
  });
  it("waits for native review and never composes at prepare",async()=>{
    const send=vi.fn(),review=vi.fn(),adopt=vi.fn(),captured={generation:"g",context:undefined};
    const prepare=vi.fn().mockResolvedValue('@view{proof} publish body:"reviewed"');
    const binding={engine:{captureComposition:()=>captured,prepareViewCommand:prepare},generation:"g"} as unknown as ViewBinding;
    const bridge=new CommandBridge(send,review);
    bridge.handle({kind:"command-prepare",request:1,epoch:0,template:"publish",arguments:{body:"reviewed"}},binding,{reviewed:adopt,compose:vi.fn(),taken:new Set()});
    await Promise.resolve();expect(adopt).not.toHaveBeenCalled();expect(send).not.toHaveBeenCalled();
    const native=review.mock.calls[0]![0] as CommandReview;
    native.adopt();expect(adopt).toHaveBeenCalledWith(native.source,captured);expect(send).toHaveBeenCalledWith({kind:"command-reply",request:1,ok:true});
  });
  it("withdraws review and ignores late responses",async()=>{
    let resolve!:(s:string)=>void;const promise=new Promise<string>(r=>resolve=r),send=vi.fn(),review=vi.fn(),adopt=vi.fn();
    const binding={engine:{captureComposition:()=>({generation:"g"}),prepareViewCommand:()=>promise}} as unknown as ViewBinding;
    const bridge=new CommandBridge(send,review);
    bridge.handle({kind:"command-prepare",request:1,epoch:0,template:"publish",arguments:{}},binding,{reviewed:adopt,compose:vi.fn(),taken:new Set()});
    bridge.reset();resolve("late");await Promise.resolve();
    expect(review).toHaveBeenCalledWith(undefined);expect(review).toHaveBeenCalledTimes(1);expect(adopt).not.toHaveBeenCalled();
    expect(send).toHaveBeenCalledWith({kind:"command-reply",request:1,ok:false,error:"changed"});bridge.close();
  });
});
