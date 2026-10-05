import {afterEach,expect,it,vi} from "vitest";
import {InteractionController} from "./interaction";
import {localTimeline} from "./interaction.test-support";
import {attachSharedState,type SharedState,type SharedCommit,type StateTransport} from "./shared-state";
import type {FrameSample} from "./instances";
const module=localTimeline;
const viewport={start:"2026-01-01T00:00:00Z",end:"2026-01-01T00:10:00Z"};
function state():SharedState{return {owner:"id1",identity:"instance",definitionRevision:"0",revision:"1",definition:module.definition!.id,artifact:module.definition!.artifact,digest:module.definition!.digest,
  fields:{viewport,selection:null,selectedItem:null},outputs:{selection:null,selectedItem:null}};}
function host(initial=state()) {
  let current=initial;
  const listeners=new Set<(sample:FrameSample<SharedState>)=>void>();
  const commit=vi.fn(async(edit:SharedCommit)=>{
    const conflict=edit.revision!==current.revision;
    if(!conflict)current={...current,fields:edit.fields,outputs:edit.outputs,revision:String(BigInt(current.revision)+1n)};
    return {state:current,conflict};
  });
  const transport:StateTransport={watch:listener=>{listeners.add(listener);listener({frame:current});return ()=>{listeners.delete(listener);};},commit};
  return {transport,commit,publish:()=>listeners.forEach(l=>l({frame:current})),current:()=>current};
}
function controller(){return new InteractionController(module.interaction!,{range:viewport});}
afterEach(()=>vi.useRealTimers());
it("shares committed selections while cursor/draft stay local and source model is untouched",async()=>{
  vi.useFakeTimers();const h=host(),a=controller(),b=controller();
  const closeA=attachSharedState(module,a,h.transport),closeB=attachSharedState(module,b,h.transport);
  a.emit({kind:"cursor",at:viewport.start});a.emit({kind:"selection-preview",range:viewport});
  await vi.advanceTimersByTimeAsync(100);expect(h.commit).not.toHaveBeenCalled();
  expect((b.committed() as any).cursor).toBeNull();expect((b.committed() as any).draft).toBeNull();
  a.emit({kind:"selection",range:viewport});await vi.advanceTimersByTimeAsync(50);h.publish();
  expect((b.committed() as any).selection).toEqual(viewport);
  expect((b.committed() as any).cursor).toBeNull();
  expect(h.current().outputs.selection).toBe(`${viewport.start}/${viewport.end}`);
  expect(h.commit).toHaveBeenCalledTimes(1);closeA();closeB();
});
it("coalesces state only, rejects conflicting commits without replay, and ignores late closed replies",async()=>{
  vi.useFakeTimers();const h=host(),a=controller(),b=controller();
  const closeA=attachSharedState(module,a,h.transport),closeB=attachSharedState(module,b,h.transport);
  a.emit({kind:"selection",range:viewport});b.emit({kind:"viewport",range:{...viewport,end:"2026-01-01T00:20:00Z"}});
  await vi.advanceTimersByTimeAsync(50);
  expect(h.commit).toHaveBeenCalledTimes(2);
  expect((b.committed() as any).selection).toEqual(viewport);
  expect(b.snapshot().error).toContain("another view");
  h.publish();await vi.advanceTimersByTimeAsync(1000);expect(h.commit).toHaveBeenCalledTimes(2);
  closeA();closeB();
  let reply!:(r:{state:SharedState;conflict:boolean})=>void;
  const slow={...h.transport,commit:vi.fn(()=>new Promise<{state:SharedState;conflict:boolean}>(r=>reply=r))};
  const c=controller(),close=attachSharedState(module,c,slow);
  c.emit({kind:"selection",range:null});await vi.advanceTimersByTimeAsync(50);close();
  reply({state:{...state(),fields:{...state().fields,selection:viewport}},conflict:false});
  await vi.advanceTimersByTimeAsync(1);expect((c.committed() as any).selection).toBeNull();
});
it("first mount initializes once, rejects protocol mismatch and never initializes an existing selection",async()=>{
  vi.useFakeTimers();const h=host({...state(),revision:"0",fields:{},outputs:{}}),a=controller();
  const close=attachSharedState(module,a,h.transport);await vi.advanceTimersByTimeAsync(1);
  expect(h.commit).toHaveBeenCalledTimes(1);close();
  const b=controller(),next=attachSharedState(module,b,h.transport);await vi.advanceTimersByTimeAsync(1);
  expect(h.commit).toHaveBeenCalledTimes(1);next();
  const invalid=host({...state(),digest:"different"}),c=controller(),stop=attachSharedState(module,c,invalid.transport);
  expect(c.snapshot().error).toContain("contract");stop();
});
it("delivers repeated events in order while initialization, cursor and remote adoption emit none",async()=>{
  vi.useFakeTimers();const h=host(),a=controller(),b=controller();
  const closeA=attachSharedState(module,a,h.transport),closeB=attachSharedState(module,b,h.transport);
  const item={source:"requests",series:"latency",id:"sample",at:viewport.start,value:"10"};
  expect(a.emit({kind:"item",item})).toBe(true);
  expect(a.emit({kind:"item",item})).toBe(true);
  a.emit({kind:"cursor",at:viewport.end});
  await vi.advanceTimersByTimeAsync(100);
  expect(h.commit).toHaveBeenCalledTimes(2);
  expect(h.commit.mock.calls.map(([edit])=>edit.events)).toEqual([[{port:"picked",value:item}],[{port:"picked",value:item}]]);
  h.publish();await vi.advanceTimersByTimeAsync(100);
  expect(h.commit).toHaveBeenCalledTimes(2);
  expect((b.committed() as any).selectedItem).toEqual(item);
  closeA();closeB();
});
it("bounds event delivery, rejects overflow visibly and never retries events after an unconfirmed write",async()=>{
  vi.useFakeTimers();const h=host(),a=controller();
  let reject!:(error:Error)=>void;
  const commit=vi.fn(()=>new Promise<{state:SharedState;conflict:boolean}>((_resolve,r)=>reject=r));
  const close=attachSharedState(module,a,{...h.transport,commit});
  const event={kind:"item",item:{source:"requests",series:"latency",id:"sample",at:viewport.start,value:"10"}};
  expect(a.emit(event)).toBe(true);await vi.advanceTimersByTimeAsync(1);
  for(let i=0;i<16;i++)expect(a.emit(event)).toBe(true);
  expect(a.emit(event)).toBe(false);expect(a.snapshot().error).toContain("queue is full");
  reject(new Error("connection lost"));await vi.advanceTimersByTimeAsync(100);
  h.publish();await vi.advanceTimersByTimeAsync(1000);
  expect(commit).toHaveBeenCalledOnce();expect(a.snapshot().error).toContain("not replayed");
  expect(a.emit(event)).toBe(false);close();
});
it("only enables result capture for confirmed state, never during pending or unconfirmed updates",async()=>{
  vi.useFakeTimers();const h=host(),a=controller(),settled=vi.fn();
  let reject!:(e:Error)=>void;
  const commit=vi.fn(()=>new Promise<{state:SharedState;conflict:boolean}>((_r,no)=>{reject=no;}));
  const close=attachSharedState(module,a,{...h.transport,commit,settled});
  expect(settled).toHaveBeenLastCalledWith(true);
  a.emit({kind:"selection-preview",range:viewport});expect(settled).toHaveBeenLastCalledWith(true);
  a.emit({kind:"selection",range:viewport});expect(settled).toHaveBeenLastCalledWith(false);
  await vi.advanceTimersByTimeAsync(50);expect(settled).toHaveBeenLastCalledWith(false);
  reject(new Error("unknown outcome"));await vi.advanceTimersByTimeAsync(1);
  expect(settled).toHaveBeenLastCalledWith(false);expect(commit).toHaveBeenCalledOnce();close();
});
