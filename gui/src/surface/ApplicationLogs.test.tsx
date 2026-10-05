import { recordExecutionFailure } from "../engine-diagnostics";
import { applicationLog } from "../application-log";
import { locatedError } from "./testing/located-error";
import { afterEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { ApplicationLog } from "../application-log";
import { ApplicationLogs } from "./ApplicationLogs";
import { Split } from "./Split";
import { oneP } from "./split-model";
let tree: ReactTestRenderer | undefined;
afterEach(() => { if (tree) act(() => tree!.unmount()); tree = undefined; vi.unstubAllGlobals(); vi.useRealTimers(); });
it("opens from the shortcut footer without changing panes", () => {
  act(() => { tree = create(<Split state={oneP({ id: "p1", title: "session" })} top={[]} prompt={[]} context={[]} />); });
  expect(tree!.root.findByProps({ className: "split-key-row" }).findByType(ApplicationLogs)).toBeTruthy();
});
it("opens a modal, shows scoped diagnostics, copies, clears and closes on Escape with focus return", async () => {
  const log = new ApplicationLog();
  const modal = { open: false, showModal: vi.fn(() => { modal.open = true; }), close: vi.fn(() => { modal.open = false; }) };
  const focus = vi.fn(), writeText = vi.fn().mockResolvedValue(undefined);
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  act(() => { tree = create(<ApplicationLogs log={log} />, { createNodeMock: node => node.type === "dialog" ? modal : node.props.className === "logs-trigger" ? { focus } : null }); });
  act(() => tree!.root.findByProps({ className: "logs-trigger" }).props.onClick());
  expect(modal.showModal).toHaveBeenCalledTimes(1);
  act(() => log.add({ level: "warning", code: "TERM_TIMEOUT", source: "Browser → terminal server", operation: "poll", workspace: "synthetic", pane: "p2", terminal: "t1", message: "Read timed out", next: "Retrying output" }));
  expect(JSON.stringify(tree!.toJSON())).toContain("synthetic");
  expect(tree!.root.findAllByType("article")).toHaveLength(1);
  const button = (label: string) => tree!.root.findAllByType("button").find(node => node.children.includes(label))!;
  await act(async () => button("Copy logs").props.onClick());
  expect(writeText).toHaveBeenCalledWith(expect.stringContaining("TERM_TIMEOUT"));
  act(() => button("Clear").props.onClick()); expect(log.snapshot()).toEqual([]);
  const event = { key: "Escape", preventDefault: vi.fn(), stopPropagation: vi.fn() };
  act(() => tree!.root.findByProps({ className: "application-logs" }).props.onKeyDown(event));
  expect(event.stopPropagation).toHaveBeenCalled(); expect(modal.close).toHaveBeenCalled(); expect(focus).toHaveBeenCalled();
});

it("shows only the latest warning/error for eight seconds, renews repeats, and retains full scoped logs", () => {
  vi.useFakeTimers();
  const log = new ApplicationLog();
  const add = (message: string, level: "error" | "warning" | "info" = "error") => log.add({
    level, code: "SYNTHETIC", source: "Engine", operation: "Run again", workspace: "synthetic", message,
    detail: "Complete cause", });
  act(() => { tree = create(<ApplicationLogs log={log} />); });
  const notices = () => tree!.root.findAllByProps({ className: "logs-notice" });
  act(() => add("First"));
  expect(notices()[0]!.children.join("")).toBe("First Complete cause");
  expect(notices()[0]!.children.join("")).not.toContain("synthetic");
  act(() => { vi.advanceTimersByTime(4000); });
  act(() => add("Second", "warning"));
  expect(notices()).toHaveLength(1);
  expect(notices()[0]!.children.join("")).toContain("Second");
  act(() => { vi.advanceTimersByTime(7000); });
  act(() => add("Second", "warning"));
  expect(log.snapshot().at(-1)?.count).toBe(2);
  act(() => { vi.advanceTimersByTime(7000); });
  expect(notices()).toHaveLength(1);
  act(() => add("Informational", "info"));
  act(() => { vi.advanceTimersByTime(1000); });
  expect(notices()).toHaveLength(0);
  expect(log.snapshot()).toHaveLength(3);
  act(() => { tree!.unmount(); tree = create(<ApplicationLogs log={log} />); });
  expect(notices()).toHaveLength(0);
  act(() => add("New"));
  expect(notices()).toHaveLength(1);
  act(() => log.clear());
  expect(notices()).toHaveLength(0);
});


it("keeps a runtime message once in the transient notice, with locations retained in the log", () => {
  applicationLog.clear();
  act(() => { tree = create(<ApplicationLogs />); });
  act(() => recordExecutionFailure({ event: "failed", node: "synthetic-node", reason: locatedError.message, error: { ...locatedError, id: "log-location-once" } }, { workspace: "synthetic" }));
  const notice = tree!.root.findByProps({ className: "logs-notice" }).children.join("");
  expect(notice.split(locatedError.message)).toHaveLength(2);
  expect(applicationLog.snapshot()[0]!.detail).toContain("line 3, column 10");
  expect(applicationLog.snapshot()[0]!.detail).not.toContain(locatedError.message);
  applicationLog.clear();
});


it("keeps exact source identities in logs but out of the transient notice", () => {
  applicationLog.clear();
  act(() => { tree = create(<ApplicationLogs />); });
  const source = "cell 11111111-2222-4333-8444-555555555555";
  const error = { ...locatedError, id: "log-source-detail", locations: locatedError.locations!.map(location => ({ ...location, source })) };
  act(() => recordExecutionFailure({ event: "failed", node: "synthetic-node", reason: error.message, error }, { workspace: "synthetic" }));
  expect(tree!.root.findByProps({ className: "logs-notice" }).children.join("")).toBe(error.message);
  expect(applicationLog.snapshot()[0]!.detail).toContain(source);
  expect(applicationLog.snapshot()[0]!.detail).toContain("called from");
  applicationLog.clear();
});
