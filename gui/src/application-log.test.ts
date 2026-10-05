import { expect, it, vi } from "vitest";
import { ApplicationLog, formatLogs, type LogMessage } from "./application-log";
import { TerminalDiagnostics } from "./terminal-diagnostics";
import { TerminalRequestError } from "./terminal-errors";
const entry: LogMessage = { level: "warning", code: "TEST", source: "Synthetic", operation: "poll", workspace: "lab", pane: "p2", terminal: "t1", message: "Interrupted" };
it("bounds records and fields, coalesces repeats, and notifies without retaining payloads", () => {
  const log = new ApplicationLog(2), listener = vi.fn();
  const unsubscribe = log.subscribe(listener);
  log.add(entry); const before = log.snapshot(); log.add({ ...entry });
  expect(before[0]?.count).toBe(1); expect(log.snapshot()[0]?.count).toBe(2);
  log.add({ ...entry, terminal: "t2" }); log.add({ ...entry, message: "x".repeat(5000) });
  expect(log.snapshot()).toHaveLength(2);
  expect(log.snapshot()[1]?.message).toHaveLength(2000);
  expect(formatLogs(log.snapshot())).toContain("Workspace: lab · Pane: p2");
  log.clear(); expect(log.snapshot()).toEqual([]); expect(listener).toHaveBeenCalledTimes(5);
  unsubscribe(); log.add(entry); expect(listener).toHaveBeenCalledTimes(5);
});
it("attributes typed request errors and emits only real recovery, independently across panes", () => {
  const log = new ApplicationLog(), first = new TerminalDiagnostics(log), second = new TerminalDiagnostics(log);
  const error = new TerminalRequestError("TERM_TIMEOUT", "Browser → terminal server", "editorreply", "Timed out", "Deadline expired");
  first.connection("output", error, "editor acknowledgement", { workspace: "a", pane: "p1", terminal: "one" });
  second.error("write", new Error("Unconfirmed"), { workspace: "b", pane: "p2" }, "Check effects");
  first.connection("output", error, "editor acknowledgement", { workspace: "a", pane: "p1" });
  first.connection("resize", undefined, "resize", {});
  expect(log.snapshot()).toHaveLength(2);
  expect(log.snapshot()[0]).toMatchObject({ code: "TERM_TIMEOUT", operation: "editorreply", workspace: "a", terminal: "one" });
  first.connection("output", undefined, "poll", { workspace: "a", pane: "p1" });
  first.connection("output", undefined, "poll", {});
  expect(log.snapshot()).toHaveLength(3);
  expect(log.snapshot()[2]?.code).toBe("TERM_RECOVERED");
});

it("retains bounded structured details beyond the short message limit and labels truncation", () => {
  const log = new ApplicationLog();
  const detail = "field · TYP005: constraint failed\n".repeat(200);
  log.add({ ...entry, detail });
  expect(log.snapshot()[0]?.detail).toBe(detail);
  expect(formatLogs(log.snapshot())).toContain(detail);
  log.add({ ...entry, detail: "x".repeat(40000) });
  expect(log.snapshot()[1]?.detail).toHaveLength(32768);
  expect(log.snapshot()[1]?.detail).toContain("truncated; inspect the cell");
});
