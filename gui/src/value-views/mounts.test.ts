import {afterEach,expect,it,vi} from "vitest";
import {ViewMounts} from "./mounts";
import {max_active_view_roots} from "./limits";
afterEach(()=>vi.useRealTimers());
it("shares one lease, renews it, and closes only after the final local mount",async()=>{
  vi.useFakeTimers();
  const action=vi.fn(async(_n:string,_i:string,_g:string,verb:string)=>verb==="open"?"token":undefined);
  const mounts=new ViewMounts(action),a=mounts.mount("node","id","generation"),b=mounts.mount("node","id","generation");
  expect(await a.ready).toBe("token");expect(await b.ready).toBe("token");
  expect(action).toHaveBeenCalledTimes(1);a.close();
  await vi.advanceTimersByTimeAsync(1000);expect(action.mock.calls.at(-1)?.[3]).toBe("touch");
  b.close();expect(action.mock.calls.at(-1)?.[3]).toBe("close");
  const count=action.mock.calls.length;await vi.advanceTimersByTimeAsync(5000);expect(action).toHaveBeenCalledTimes(count);
});
it("cleans up an opening lease after unmount and never silently reopens an expired lease",async()=>{
  vi.useFakeTimers();let resolve!:(token:string)=>void;
  const action=vi.fn((_n:string,_i:string,_g:string,verb:string):Promise<string|undefined>=>verb==="open"?new Promise(r=>{resolve=r;}):Promise.resolve(undefined));
  const mounts=new ViewMounts(action),a=mounts.mount("n","i","g");
  const rejected=expect(a.ready).rejects.toThrow("closed");a.close();resolve("late");await rejected;
  expect(action.mock.calls.at(-1)?.[3]).toBe("close");
  const failed=vi.fn(async(_n:string,_i:string,_g:string,verb:string)=>{if(verb==="touch")throw new Error("expired");return "token";});
  const other=new ViewMounts(failed),b=other.mount("n","i","g");await b.ready;
  await vi.advanceTimersByTimeAsync(5000);expect(failed.mock.calls.filter(c=>c[3]==="open")).toHaveLength(1);
  expect(failed.mock.calls.filter(c=>c[3]==="touch")).toHaveLength(1);b.close();
});

it("queues visible roots, shares waiting duplicates and waits for close acknowledgement before admission",async()=>{
  vi.useFakeTimers();
  let finishClose!:()=>void;
  const action=vi.fn(async(node:string,_i:string,_g:string,verb:string)=>{
    if(verb==="close"&&node==="n0")await new Promise<void>(resolve=>{finishClose=resolve;});
    return verb==="open"?`${node}-token`:undefined;
  });
  const mounts=new ViewMounts(action);
  const active=Array.from({length:max_active_view_roots()},(_,n)=>mounts.mount(`n${n}`,"identity","generation"));
  await Promise.all(active.map(mount=>mount.ready));
  const waiting=mounts.mount("next","identity","generation"),duplicate=mounts.mount("next","identity","generation");
  const cancelled=mounts.mount("cancelled","identity","generation");
  const rejected=expect(cancelled.ready).rejects.toThrow("closed");cancelled.close();await rejected;
  expect(waiting.waiting).toBe(true);expect(duplicate.ready).toBe(waiting.ready);
  active[0]!.close();
  expect(action.mock.calls.filter(c=>c[3]==="open")).toHaveLength(max_active_view_roots());
  finishClose();expect(await waiting.ready).toBe("next-token");expect(await duplicate.ready).toBe("next-token");
  expect(action.mock.calls.filter(c=>c[3]==="open")).toHaveLength(max_active_view_roots()+1);
  expect(action.mock.calls.some(c=>c[0]==="cancelled")).toBe(false);
  waiting.close();expect(action.mock.calls.some(c=>c[0]==="next"&&c[3]==="close")).toBe(false);
  duplicate.close();active.slice(1).forEach(mount=>mount.close());
});

it("keeps cancelled pending opens charged until their late lease has been closed",async()=>{
  vi.useFakeTimers();
  let finishOpen!:(token:string)=>void,finishClose!:()=>void;
  const action=vi.fn(async(node:string,_i:string,_g:string,verb:string)=>{
    if(node==="pending"&&verb==="open")return new Promise<string>(r=>{finishOpen=r;});
    if(node==="pending"&&verb==="close")await new Promise<void>(r=>{finishClose=r;});
    return verb==="open"?"token":undefined;
  });
  const mounts=new ViewMounts(action),pending=mounts.mount("pending","i","g");
  const others=Array.from({length:max_active_view_roots()-1},(_,n)=>mounts.mount(String(n),"i","g"));
  await Promise.all(others.map(mount=>mount.ready));
  const waiting=mounts.mount("waiting","i","g");
  const rejected=expect(pending.ready).rejects.toThrow("closed");pending.close();finishOpen("late");await rejected;
  expect(action.mock.calls.some(c=>c[0]==="waiting")).toBe(false);
  finishClose();await waiting.ready;
  expect(action.mock.calls.filter(c=>c[0]==="waiting"&&c[3]==="open")).toHaveLength(1);
  waiting.close();others.forEach(mount=>mount.close());
});

it("notifies all displays of a failed renewal and does not reopen for a new duplicate",async()=>{
  vi.useFakeTimers();
  const action=vi.fn(async(_n:string,_i:string,_g:string,verb:string)=>{
    if(verb==="touch")throw new Error("display closed");return verb==="open"?"token":undefined;
  });
  const mounts=new ViewMounts(action),first=vi.fn(),second=vi.fn();
  const a=mounts.mount("n","i","g",first);await a.ready;
  await vi.advanceTimersByTimeAsync(1000);
  const b=mounts.mount("n","i","g",second);await b.ready;
  expect(first).toHaveBeenCalledOnce();expect(second).toHaveBeenCalledOnce();
  await vi.advanceTimersByTimeAsync(5000);
  expect(action.mock.calls.filter(c=>c[3]==="open")).toHaveLength(1);
  expect(action.mock.calls.filter(c=>c[3]==="touch")).toHaveLength(1);
  a.close();b.close();
});
