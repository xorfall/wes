import { afterEach, expect, it, vi } from "vitest";
import { terminalInput } from "./terminal-input";

afterEach(() => vi.useRealTimers());

it("drains 1,000 queued mouse reports without 1,000 round trips or dropped movement", async () => {
  vi.useFakeTimers();
  const sent: string[] = [];
  let inFlight = 0, peak = 0;
  const failed = vi.fn();
  const input = terminalInput(async text => {
    sent.push(text); peak = Math.max(peak, ++inFlight);
    await new Promise(resolve => setTimeout(resolve, 25));
    inFlight--;
  }, failed);
  const events = Array.from({ length: 1000 }, (_, i) => `\x1b[<${i % 2 ? 64 : 65};40;12M`);
  const began = Date.now();
  input.push(events[0]!);
  expect(sent).toEqual([events[0]]); // First event is immediate, even with fake timers.
  for (const event of events.slice(1)) input.push(event);
  expect(sent).toHaveLength(1);
  await vi.runAllTimersAsync();
  expect(sent.join("")).toBe(events.join(""));
  expect(peak).toBe(1);
  expect(sent).toHaveLength(7); // Previously 1,000 sequential requests / 25,000 ms.
  expect(Date.now() - began).toBe(175);
  expect(failed).not.toHaveBeenCalled();
});

it("preserves Unicode paste, mouse, arrow, interrupt and keystroke order in bounded writes", async () => {
  const sent: string[] = [], acknowledgements: (() => void)[] = [];
  const input = terminalInput(text => {
    sent.push(text); return new Promise<void>(resolve => acknowledgements.push(resolve));
  }, vi.fn());
  const events = ["x", "\x1b[<64;1;1M", "\x1b[200~", "Ğ😀".repeat(9000), "\x1b[201~", "\x1b[A", "\x03", "done\r"];
  events.forEach(event => input.push(event));
  while (acknowledgements.length) { acknowledgements.shift()!(); await Promise.resolve(); }
  expect(sent.join("")).toBe(events.join(""));
  for (const text of sent) {
    expect([...text].length).toBeLessThanOrEqual(2048);
    const bytes = new TextEncoder().encode(text);
    expect(bytes.length).toBeLessThanOrEqual(8192);
    expect(new TextDecoder("utf-8", { fatal: true }).decode(bytes)).toBe(text);
  }
  input.push("idle key");
  expect(sent.at(-1)).toBe("idle key");
  acknowledgements.shift()!(); await Promise.resolve();
});

it("drops unsent input on an ambiguous failure and never retries", async () => {
  let reject!: (error: Error) => void;
  const write = vi.fn(() => new Promise<void>((_resolve, fail) => { reject = fail; }));
  const failed = vi.fn(), input = terminalInput(write, failed);
  input.push("accepted?"); input.push("must not follow");
  const error = new Error("Lost acknowledgement"); reject(error); await Promise.resolve();
  input.push("must not restart");
  expect(write).toHaveBeenCalledExactlyOnceWith("accepted?");
  expect(failed).toHaveBeenCalledExactlyOnceWith(error);
});

it.each([false, true])("disposed input ignores late replies and failures (%s)", async reject => {
  let finish!: () => void;
  const write = vi.fn(() => new Promise<void>((resolve, fail) => { finish = () => reject ? fail(new Error("late")) : resolve(); }));
  const failed = vi.fn(), input = terminalInput(write, failed);
  input.push("in flight"); input.push("queued"); input.dispose(); input.dispose(); input.push("disposed");
  finish(); await Promise.resolve();
  expect(write).toHaveBeenCalledExactlyOnceWith("in flight");
  expect(failed).not.toHaveBeenCalled();
});

it("empty events do not create writes, and synchronous write errors retire input", () => {
  const failed = vi.fn(), write = vi.fn(() => { throw new Error("unavailable"); });
  const input = terminalInput(write, failed);
  input.push(""); expect(write).not.toHaveBeenCalled();
  input.push("x"); input.push("y");
  expect(write).toHaveBeenCalledTimes(1); expect(failed).toHaveBeenCalledTimes(1);
});
