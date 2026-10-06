import { TerminalUnavailable } from "./terminal-errors";
import { afterEach, expect, it, vi } from "vitest";
import { terminalOutput, type TerminalFrame } from "./terminal-output";
import type { UiRequest } from "./assistant-ui";
function frame(text = "", start = 0, closed = false): TerminalFrame {
  return { start, next: start + text.length, data: btoa(text), closed, exit: closed ? 0 : null, problem: null };
}
function sink() {
  return { poll: vi.fn<(cursor: number, signal: AbortSignal) => Promise<TerminalFrame>>(), write: vi.fn<(bytes: Uint8Array) => Promise<void>>().mockResolvedValue(undefined), trimmed: vi.fn(), ui: vi.fn().mockResolvedValue(undefined), ended: vi.fn(), problem: vi.fn(), connection: vi.fn(), unavailable: vi.fn() };
}
afterEach(() => vi.useRealTimers());
it("drains backlog without a timer but never queues another frame before xterm acknowledges", async () => {
  vi.useFakeTimers(); const s = sink(); const controller = new AbortController();
  let parsed!: () => void;
  s.poll.mockResolvedValueOnce(frame("one")).mockResolvedValueOnce(frame("two", 3)).mockResolvedValueOnce(frame("", 6, true));
  s.write.mockImplementationOnce(() => new Promise(resolve => { parsed = resolve; }));
  const running = terminalOutput(s, controller.signal);
  await Promise.resolve(); await Promise.resolve();
  expect(s.poll).toHaveBeenCalledTimes(1);
  expect(vi.getTimerCount()).toBe(0);
  parsed(); await running;
  expect(s.poll.mock.calls.map(c => c[0])).toEqual([0, 3, 6]);
  expect(s.write.mock.calls.map(c => new TextDecoder().decode(c[0]))).toEqual(["one", "two"]);
  expect(s.ended).toHaveBeenCalledWith(0); expect(s.trimmed).not.toHaveBeenCalled();
});
it("retries reads at the consumed cursor after a lost UI acknowledgement without replaying bytes", async () => {
  vi.useFakeTimers(); const s = sink(); const controller = new AbortController();
  const read: UiRequest = { id: "read", operation: { kind: "draft_read" } };
  s.poll.mockResolvedValueOnce({ ...frame("x"), ui: read }).mockResolvedValueOnce({ ...frame("", 1, true), ui: read });
  s.ui.mockRejectedValueOnce(new Error("reply lost"));
  const running = terminalOutput(s, controller.signal);
  await vi.advanceTimersByTimeAsync(250); await running;
  expect(s.poll.mock.calls.map(c => c[0])).toEqual([0, 1]);
  expect(s.write).toHaveBeenCalledTimes(1); expect(s.ui).toHaveBeenCalledTimes(2);
  expect(s.ui).toHaveBeenLastCalledWith(read);
  expect(s.connection).toHaveBeenCalledWith(expect.any(Error), "UI acknowledgement");
});
it("reports trimming and drains final output before ending", async () => {
  const s = sink();
  s.poll.mockResolvedValueOnce(frame("tail", 100, true)).mockResolvedValueOnce(frame("", 104, true));
  await terminalOutput(s, new AbortController().signal);
  expect(s.trimmed).toHaveBeenCalledTimes(1); expect(s.write).toHaveBeenCalledTimes(1);
  expect(s.poll.mock.calls.map(c => c[0])).toEqual([0, 104]); expect(s.ended).toHaveBeenCalledTimes(1);
});
it("abort cancels a pending read and does not schedule retries or render a late frame", async () => {
  vi.useFakeTimers(); const s = sink(); const controller = new AbortController();
  s.poll.mockImplementation((_cursor, signal) => new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(new Error("aborted")))));
  const running = terminalOutput(s, controller.signal); controller.abort(); await running;
  expect(s.connection).not.toHaveBeenCalled(); expect(s.problem).not.toHaveBeenCalled(); expect(s.write).not.toHaveBeenCalled(); expect(vi.getTimerCount()).toBe(0);
  const late = sink(); let resolve!: (frame: TerminalFrame) => void;
  late.poll.mockImplementation(() => new Promise(r => { resolve = r; }));
  const close = new AbortController(); const waiting = terminalOutput(late, close.signal);
  close.abort(); resolve(frame("late")); await waiting; expect(late.write).not.toHaveBeenCalled();
});

it("delivers pane commands and does not process late commands after cancellation", async () => {
  const s = { ...sink(), command: vi.fn().mockResolvedValue(undefined) };
  const command = { id: "split", text: "/rsplit" };
  s.poll.mockResolvedValueOnce({ ...frame("", 0, true), command });
  await terminalOutput(s, new AbortController().signal);
  expect(s.command).toHaveBeenCalledWith(command);
  const controller = new AbortController(); controller.abort();
  await terminalOutput(s, controller.signal);
  expect(s.command).toHaveBeenCalledTimes(1);
});

it("stops polling unavailable authority instead of retrying or replaying output", async () => {
  vi.useFakeTimers(); const s = sink();
  s.poll.mockResolvedValueOnce(frame("old prompt")).mockRejectedValueOnce(new TerminalUnavailable());
  await terminalOutput(s, new AbortController().signal);
  await vi.advanceTimersByTimeAsync(5000);
  expect(s.poll).toHaveBeenCalledTimes(2);
  expect(s.write).toHaveBeenCalledTimes(1);
  expect(s.unavailable).toHaveBeenCalledWith(new TerminalUnavailable().message);
  expect(s.problem).not.toHaveBeenCalled();
  expect(s.ended).not.toHaveBeenCalled();
  expect(vi.getTimerCount()).toBe(0);
});

it("passes split Turkish UTF-8 scalars to the persistent xterm decoder without lossy frame decoding", async () => {
  const bytes = new TextEncoder().encode("ĞğİıŞşÇçÖöÜü 100%\r\n");
  const s = sink();
  for (let index = 0; index < bytes.length; index++) {
    s.poll.mockResolvedValueOnce({ ...frame(), start: index, next: index + 1, data: btoa(String.fromCharCode(bytes[index]!)) });
  }
  s.poll.mockResolvedValueOnce(frame("", bytes.length, true));
  const decoder = new TextDecoder("utf-8", { fatal: true }); let actual = "";
  s.write.mockImplementation(async chunk => { actual += decoder.decode(chunk, { stream: true }); });
  await terminalOutput(s, new AbortController().signal);
  actual += decoder.decode();
  expect(actual).toBe("ĞğİıŞşÇçÖöÜü 100%\r\n");
  expect(s.trimmed).not.toHaveBeenCalled();
  expect(s.problem).not.toHaveBeenCalled();
});

it("reports a transient read failure and recovery, preserving the consumed cursor", async () => {
  vi.useFakeTimers(); const s = sink();
  const failure = new Error("interrupted");
  s.poll.mockResolvedValueOnce(frame("one")).mockRejectedValueOnce(failure).mockResolvedValueOnce(frame("", 3, true));
  const running = terminalOutput(s, new AbortController().signal);
  await vi.advanceTimersByTimeAsync(250); await running;
  expect(s.poll.mock.calls.map(c => c[0])).toEqual([0, 3, 3]);
  expect(s.connection.mock.calls).toEqual([[], [failure, "poll"], []]);
  expect(s.write).toHaveBeenCalledTimes(1);
  expect(s.problem).not.toHaveBeenCalled();
});
