import { afterEach, expect, it, vi } from "vitest";
import { ResultReader, ResultReadError } from "./result-reader";

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });
const value = { type: { kind: "unknown" }, provenance: {}, data: "synthetic" };
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
const busy = () => new Response(JSON.stringify({ error: { code: "VALUE_READ_BUSY", message: "Backend says readers are busy; result not known missing.", retryable: true, retryAfterMs: 150, context: { operation: "read-value", handle: "h" } } }), { status: 503 });

it("admits only two complete response bodies and coalesces concurrent handle reads", async () => {
  const complete: (() => void)[] = [];
  let active = 0, peak = 0;
  const fetch = vi.fn(async (_url: string, _options?: RequestInit) => {
    active++; peak = Math.max(peak, active);
    return { ok: true, text: () => new Promise(resolve => complete.push(() => { active--; resolve(JSON.stringify(value)); })) };
  });
  vi.stubGlobal("window", { fetch });
  const reader = new ResultReader(() => "g1", () => 15000);
  const first = reader.read("same handle");
  expect(reader.read("same handle")).toBe(first);
  const reads = [first, ...Array.from({ length: 5 }, (_, i) => reader.read(`h${i}`))];
  await flush();
  expect(fetch).toHaveBeenCalledTimes(2); // Returning headers must not release admission.
  for (let i = 0; i < 6; i++) { complete.shift()!(); await flush(); }
  await expect(Promise.all(reads)).resolves.toEqual(Array(6).fill(value));
  expect(peak).toBe(2);
  expect(fetch.mock.calls[0]?.[0]).toBe("/values/same%20handle");
  const fresh = reader.read("same handle"); await flush(); complete.shift()!();
  await fresh; expect(fetch).toHaveBeenCalledTimes(7); // No persistent/private value cache.
});

it("retries only busy data reads, with finite backoff and an honest exhausted message", async () => {
  vi.useFakeTimers();
  const fetch = vi.fn().mockResolvedValueOnce(busy())
    .mockResolvedValueOnce(busy())
    .mockResolvedValueOnce(new Response(JSON.stringify(value)));
  vi.stubGlobal("window", { fetch });
  const reader = new ResultReader(() => "g", () => 15000);
  const result = reader.read("h"); await flush();
  expect(fetch).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(150); expect(fetch).toHaveBeenCalledTimes(2);
  await vi.advanceTimersByTimeAsync(400); await expect(result).resolves.toEqual(value);
  expect(fetch).toHaveBeenCalledTimes(3);
  fetch.mockImplementation(async () => busy());
  const refused = reader.read("busy").catch(error => error.message);
  await vi.runAllTimersAsync();
  expect(await refused).toBe("VALUE_READ_BUSY: Backend says readers are busy; result not known missing.");
  expect(fetch).toHaveBeenCalledTimes(7);
  expect(fetch.mock.calls.every(call => String(call[0]).startsWith("/values/"))).toBe(true);
});

it("keeps backend context and does not guess that an unexplained 503 means missing or retryable", async () => {
  const missing = { code: "VALUE_UNAVAILABLE", message: "The archive no longer contains this handle.", retryable: false, context: { operation: "read-value", handle: "missing" } };
  const fetch = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({ error: missing }), { status: 404 }))
    .mockResolvedValueOnce(new Response(null, { status: 503 }));
  vi.stubGlobal("window", { fetch });
  const reader = new ResultReader(() => "g", () => 15000);
  const error = await reader.read("missing").catch(error => error);
  expect(error).toBeInstanceOf(ResultReadError);
  expect(error.problem).toEqual(missing);
  expect(error.message).toBe(`VALUE_UNAVAILABLE: ${missing.message}`);
  await expect(reader.read("legacy-busy")).rejects.toThrow("server supplied no structured explanation");
  expect(fetch).toHaveBeenCalledTimes(2);
});

it.each([404, 410, 403, 500])("does not retry HTTP %i and releases admission after errors", async status => {
  const fetch = vi.fn().mockResolvedValueOnce(new Response(null, { status }))
    .mockResolvedValueOnce(new Response(JSON.stringify(value)));
  vi.stubGlobal("window", { fetch });
  const reader = new ResultReader(() => "g", () => 15000);
  await expect(reader.read("unavailable")).rejects.toThrow(String(status));
  expect(fetch).toHaveBeenCalledTimes(1);
  await expect(reader.read("available")).resolves.toEqual(value);
});

it("discards old bodies and queued reads when the workspace generation changes", async () => {
  let generation = "old";
  const bodies: ((value: unknown) => void)[] = [];
  const fetch = vi.fn(async () => ({ ok: true, text: () => new Promise<string>(resolve => bodies.push(value=>resolve(JSON.stringify(value)))) }));
  vi.stubGlobal("window", { fetch });
  const reader = new ResultReader(() => generation, () => 15000);
  const old = [reader.read("one"), reader.read("two"), reader.read("queued")];
  const settled = Promise.allSettled(old);
  await flush(); generation = "new";
  bodies.shift()!(value); bodies.shift()!(value); await flush();
  expect((await settled).every(result => result.status === "rejected" && /Workspace changed/.test(result.reason.message))).toBe(true);
  expect(fetch).toHaveBeenCalledTimes(2);
  const next = reader.read("one"); await flush(); bodies.shift()!(value);
  await expect(next).resolves.toEqual(value);
});

it("aborts stalled reads after the deadline without claiming the value was deleted", async () => {
  vi.useFakeTimers();
  const fetch = vi.fn((_url: string, options: RequestInit) => new Promise((_resolve, reject) => {
    options.signal!.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
  }));
  vi.stubGlobal("window", { fetch });
  const reader = new ResultReader(() => "g", () => 100);
  const result = reader.read("slow").catch(error => error.message);
  await vi.advanceTimersByTimeAsync(100);
  expect(await result).toContain("does not mean the command failed or the result was deleted");
  expect(fetch).toHaveBeenCalledOnce();
});
