import { afterEach, expect, it, vi } from "vitest";
import { DisplayReadError, LiveViewReader, sourceScope, type DisplayRead } from "./live-view-reader";
import type { StoredValue } from "./protocol";
const value:DisplayRead = {value:{type:{kind:"unknown"},data:1} as StoredValue,metadata:{revision:"1",epochs:[["source","r1"]],sources:[]}};
afterEach(() => vi.useRealTimers());
it("does no work without demand and bounds concurrency, cadence and cancellation", async () => {
  vi.useFakeTimers();
  const signals: AbortSignal[] = [];
  const resolve: ((value: DisplayRead) => void)[] = [];
  const fetch = vi.fn((_node: string, _generation: string, signal: AbortSignal) => { signals.push(signal); return new Promise<DisplayRead>(yes => resolve.push(yes)); });
  const reader = new LiveViewReader(fetch);
  await vi.advanceTimersByTimeAsync(1000); expect(fetch).not.toHaveBeenCalled();
  const changed = vi.fn();
  const close = ["a", "b", "c"].map(node => reader.watch(node, "g", changed));
  await vi.advanceTimersByTimeAsync(1000); expect(fetch).toHaveBeenCalledTimes(2);
  resolve[0]!(value); resolve[1]!(value);
  await vi.advanceTimersByTimeAsync(100);
  expect(fetch).toHaveBeenCalledTimes(4);
  expect(fetch.mock.calls[2]![0]).toBe("c");
  close.forEach(close => close()); expect(signals.slice(2).every(signal => signal.aborted)).toBe(true);
  resolve.slice(2).forEach(resolve => resolve(value));
  await vi.advanceTimersByTimeAsync(1000); expect(changed).toHaveBeenCalledTimes(2);
  expect(fetch).toHaveBeenCalledTimes(4);
});
it("stops a slow view once instead of retrying and flooding warnings", async () => {
  vi.useFakeTimers();
  const fetch = vi.fn((_n: string, _g: string, signal: AbortSignal) => new Promise<DisplayRead>((_, reject) => signal.addEventListener("abort", () => reject(new Error("aborted")))));
  const reader = new LiveViewReader(fetch); const changed = vi.fn();
  const close = reader.watch("a", "g", changed);
  await vi.advanceTimersByTimeAsync(10000);
  expect(fetch).toHaveBeenCalledOnce(); expect(changed).toHaveBeenCalledOnce();
  expect(changed.mock.calls[0]![0].problem).toContain("2-second budget"); close();
});

it("deduplicates revisions, retains a fitting held sample on budget errors and withdraws on authority loss",async()=>{
  vi.useFakeTimers();let reply:DisplayRead|Error=value;
  const fetch=vi.fn(async()=>{if(reply instanceof Error)throw reply;return reply;});
  const reader=new LiveViewReader(fetch),changed=vi.fn();let close=reader.watch("n","g",changed);
  await vi.advanceTimersByTimeAsync(1000);expect(changed).toHaveBeenCalledOnce();
  reader.holdCell("n","g",{sample:changed.mock.calls[0]![0],at:"12:00"});
  reply=new DisplayReadError("too large","budget",{...value.metadata,revision:"2"});
  await vi.advanceTimersByTimeAsync(100);expect(changed.mock.lastCall![0].value).toBe(value.value);expect(reader.cellSnapshot("n","g")?.sample.value).toBe(value.value);
  close();reply=new DisplayReadError("permission withdrawn","withdrawn");close=reader.watch("n","g",changed);
  await vi.advanceTimersByTimeAsync(100);expect(changed.mock.lastCall![0].value).toBeUndefined();expect(reader.cellSnapshot("n","g")).toBeUndefined();close();
});
it("new source epochs clear a cell snapshot even if its value revision repeats",async()=>{
 vi.useFakeTimers();let reply=value;const reader=new LiveViewReader(async()=>reply),changed=vi.fn();const close=reader.watch("n","g",changed);
 await vi.advanceTimersByTimeAsync(100);reader.holdCell("n","g",{sample:changed.mock.lastCall![0],at:"12:00"});
 reply={...value,metadata:{...value.metadata,epochs:[["source","r2"]]}};await vi.advanceTimersByTimeAsync(100);
 expect(changed).toHaveBeenCalledTimes(2);expect(reader.cellSnapshot("n","g")).toBeUndefined();close();
});
it("derives one source's scope labels from its wire counts without summing or estimating", () => {
  expect(sourceScope({ windowItems: 500, omitted: "1240", rejected: "3", accepted: "1740" })).toEqual({ text: "1740 accepted: 500 in window + 1240 outside window", rejected: "3 rejected" });
  expect(sourceScope({ windowItems: 7, omitted: "0", rejected: "0", accepted: "7" })).toEqual({ text: "7 accepted · all 7 in window" });
  const huge = "340282366920938463463374607431768211455";
  expect(sourceScope({ windowItems: 1, omitted: "340282366920938463463374607431768211454", rejected: huge, accepted: huge }).rejected).toBe(`${huge} rejected`);
});
