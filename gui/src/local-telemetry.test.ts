import { afterEach, expect, it, vi } from "vitest";
import { initializeDiagnostics, diagnosticsStatus, refreshDiagnostics, controlDiagnostics, clientDiagnostic, timeClientSubmit } from "./local-telemetry";
let dispose: (() => void) | undefined;
const status = (mode = "basic", saved = "basic") => ({ schema:1, generation:"fixture",mode,saved_mode:saved,remaining_ms:5000,forced:false,dropped:0,suppressed:0,write_errors:0,recent_count:0,previous_unclean:false,writer_stopping:false,metrics:{operations:[]} });
const flush = async () => { for(let i=0;i<12;i++) await Promise.resolve(); };
function setup(mode="basic") {
  vi.useFakeTimers(); vi.stubGlobal("window",new EventTarget());
  vi.stubGlobal("document",Object.assign(new EventTarget(),{visibilityState:"visible"}));
  vi.stubGlobal("BroadcastChannel",undefined);
  const fetch=vi.fn().mockResolvedValue({ok:true,json:async()=>status(mode)});vi.stubGlobal("fetch",fetch);
  dispose=initializeDiagnostics(); return fetch;
}
afterEach(()=>{dispose?.();dispose=undefined;vi.useRealTimers();vi.unstubAllGlobals();});
it("Off schedules no client records or timer and never forwards private error text",async()=>{
  const fetch=setup("off");await flush();
  window.dispatchEvent(Object.assign(new Event("error"),{message:"SYNTHETIC_SECRET"}));clientDiagnostic("render");timeClientSubmit()("error");
  await vi.advanceTimersByTimeAsync(3000);expect(fetch).toHaveBeenCalledTimes(1);expect(vi.getTimerCount()).toBe(0);
});
it("batches only fixed classifications and finite durations without console or Error payloads",async()=>{
  const fetch=setup();await flush();
  window.dispatchEvent(Object.assign(new Event("error"),{message:"SYNTHETIC_SECRET",filename:"secret/path"}));clientDiagnostic("render");timeClientSubmit()("ok");
  await vi.advanceTimersByTimeAsync(1000);
  const request=fetch.mock.calls.find(c=>c[0]==="/diagnostics/client")!;
  const batch=JSON.parse(request[1].body);expect(batch.events.map((e:any)=>e.kind)).toEqual(["error","render","submit"]);
  expect(request[1].body).not.toContain("SYNTHETIC_SECRET");expect(request[1].body).not.toContain("secret/path");
  expect(request[1].headers["X-Wes-Session"]).toBe("fixture");
});
it("turning Off discards pending records, preserves selected mode and does not poll",async()=>{
  const fetch=setup();await flush();clientDiagnostic("error");
  fetch.mockResolvedValueOnce({ok:true,json:async()=>status("off","off")});
  await controlDiagnostics({action:"set",mode:"off"});await vi.advanceTimersByTimeAsync(60_000);
  expect(diagnosticsStatus()?.mode).toBe("off");expect(fetch.mock.calls.some(c=>c[0]==="/diagnostics/client")).toBe(false);
  expect(fetch).toHaveBeenCalledTimes(2);
});
it("capture completion refreshes once, and a refused control never reports success",async()=>{
  const fetch=setup("diagnostic");await flush();
  fetch.mockResolvedValueOnce({ok:true,json:async()=>status("basic")});await vi.advanceTimersByTimeAsync(5050);
  expect(diagnosticsStatus()?.mode).toBe("basic");expect(fetch).toHaveBeenCalledTimes(2);
  fetch.mockResolvedValueOnce({ok:false});await expect(controlDiagnostics({action:"start"})).rejects.toThrow("not confirmed");
  expect(diagnosticsStatus()?.mode).toBe("basic");
});
it("limits a burst to sixteen events and does not retry telemetry transport failures",async()=>{
  const fetch=setup();await flush();for(let i=0;i<1000;i++)clientDiagnostic("error");
  fetch.mockRejectedValueOnce(new Error("offline"));await vi.advanceTimersByTimeAsync(1000);
  expect(JSON.parse(fetch.mock.calls[1]![1].body).events).toHaveLength(16);
  await vi.advanceTimersByTimeAsync(60_000);expect(fetch).toHaveBeenCalledTimes(2);
  fetch.mockResolvedValueOnce({ok:true,json:async()=>status("off","off")});await refreshDiagnostics();
});
