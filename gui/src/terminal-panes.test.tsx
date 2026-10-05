import { terminalPane } from "./surface/split-model";
import { applicationLog } from "./application-log";
import { TerminalUnavailable } from "./terminal-errors";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "./engine";
import type { ITheme } from "@xterm/xterm";
import { monoFamily } from "./settings";
import { ShellTerminal } from "./ShellTerminal";
import { Split } from "./surface/Split";
import { focus, oneP, SESSION_PANE, splitPane } from "./surface/split-model";
const { terminals, output, directoryHandlers, dataHandlers, fits, resized, resizeHandlers } = vi.hoisted(() => ({ terminals: [] as { focus: ReturnType<typeof vi.fn>; options: { theme?: ITheme; fontFamily?: string; linkHandler?: { allowNonHttpProtocols?: boolean } }; links: { dispose: ReturnType<typeof vi.fn> }[] }[], output: vi.fn(), directoryHandlers: [] as ((value: string) => boolean)[], dataHandlers: [] as ((value: string) => void)[], fits: [] as ReturnType<typeof vi.fn>[], resized: [] as (() => void)[], resizeHandlers: [] as ((size: { cols: number; rows: number }) => void)[] }));
vi.mock("@xterm/xterm", () => ({ Terminal: class {
  focus = vi.fn(); cols = 80; rows = 24; options: { theme?: ITheme; fontFamily?: string }; links: { dispose: ReturnType<typeof vi.fn> }[] = [];
  registerLinkProvider() { const registration = { dispose: vi.fn() }; this.links.push(registration); return registration; }
  parser = { registerOscHandler: (_: number, fn: (value: string) => boolean) => { directoryHandlers.push(fn); return { dispose() {} }; } };
  constructor(options: { theme?: ITheme; fontFamily?: string }) { this.options = options; terminals.push(this); }
  loadAddon() {} open() {} dispose() {} reset() {} writeln() {}
  onData(handler: (value: string) => void) { dataHandlers.push(handler); return { dispose() {} }; } onResize(handler: (size: { cols: number; rows: number }) => void) { resizeHandlers.push(handler); return { dispose() {} }; }
} }));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: class { fit = vi.fn(); constructor() { fits.push(this.fit); } } }));
vi.mock("./terminal-wheel", () => ({ normalizeTerminalWheel: () => () => {} }));
vi.mock("./terminal-renderer", () => ({ accelerateTerminal: () => () => {} }));
vi.mock("./terminal-output", () => ({ terminalOutput: output }));
let tree: ReactTestRenderer | undefined;
beforeEach(() => {
  applicationLog.clear(); resizeHandlers.length = 0;
  terminals.length = 0; directoryHandlers.length = 0; dataHandlers.length = 0; fits.length = 0; resized.length = 0; output.mockResolvedValue(undefined);
  vi.stubGlobal("document", { documentElement: { getAttribute: () => "light" } });
  vi.stubGlobal("getComputedStyle", () => ({ getPropertyValue: () => "" }));
  vi.stubGlobal("ResizeObserver", class { constructor(callback: () => void) { resized.push(callback); } observe() {} disconnect() {} });
  vi.stubGlobal("MutationObserver", class { observe() {} disconnect() {} });
});
afterEach(() => { if (tree) act(() => tree!.unmount()); tree = undefined; vi.unstubAllGlobals(); });
const mount = async (engine: Engine, focused = false, onDirectory = vi.fn()) => act(async () => {
  tree = create(<ShellTerminal engine={engine} active focused={focused} autoStart closeOnUnmount generation="g" cwd="/synthetic/project" onDirectory={onDirectory} />,
    { createNodeMock: node => node.props.className === "terminal-emulator" ? { clientWidth: 400, clientHeight: 300 } : null });
});
it("starts one fresh shell, restores its directory, never steals ordinary split focus, and closes it", async () => {
  const terminal = vi.fn().mockResolvedValue({ id: "owned", cwd: "/synthetic/project" }); const engine = { terminal } as unknown as Engine;
  const onDirectory = vi.fn(); await mount(engine, false, onDirectory);
  expect(terminal.mock.calls.map(([r]) => r.action)).toEqual(["start", "resize"]);
  expect(terminal.mock.calls[0]![0]).toEqual({ action: "start", cwd: "/synthetic/project" });
  expect(terminals[0]!.focus).not.toHaveBeenCalled();
  expect(onDirectory).toHaveBeenCalledWith("/synthetic/project");
  await act(async () => tree!.update(<ShellTerminal engine={engine} active focused autoStart closeOnUnmount generation="g" />));
  expect(terminals[0]!.focus).toHaveBeenCalled();
  expect(terminal.mock.calls.filter(([r]) => r.action === "start")).toHaveLength(1);
  await act(async () => tree!.unmount()); tree = undefined;
  expect(terminal).toHaveBeenLastCalledWith({ action: "close", id: "owned" });
});
it("detects web links for Cmd-click and releases them when the terminal closes", async () => {
  await mount({ terminal: vi.fn().mockResolvedValue({ id: "owned" }) } as unknown as Engine);
  expect(terminals[0]!.links).toHaveLength(1);
  expect(terminals[0]!.options.linkHandler?.allowNonHttpProtocols).toBe(false);
  await act(async () => tree!.unmount()); tree = undefined;
  expect(terminals[0]!.links[0]!.dispose).toHaveBeenCalledTimes(1);
});
it("closes a shell whose start reply arrives after its pane was closed", async () => {
  let finish!: (v: {id: string}) => void;
  const terminal = vi.fn().mockImplementationOnce(() => new Promise(resolve => { finish = resolve; })).mockResolvedValue({});
  await mount({ terminal } as unknown as Engine);
  await act(async () => tree!.unmount()); tree = undefined;
  await act(async () => finish({ id: "late" }));
  expect(terminal.mock.calls.map(([r]) => r)).toEqual([{ action: "start", cwd: "/synthetic/project" }, { action: "close", id: "late" }]);
});
it("records OSC 7 directory changes and ignores malformed sequences", async () => {
  const onDirectory = vi.fn(); await mount({ terminal: vi.fn().mockResolvedValue({ id: "owned" }) } as unknown as Engine, false, onDirectory);
  directoryHandlers[0]!("file://localhost/synthetic/new%20directory");
  expect(onDirectory).toHaveBeenLastCalledWith("/synthetic/new directory");
  directoryHandlers[0]!("not a URL"); directoryHandlers[0]!("https://example.com/path"); directoryHandlers[0]!("file:///bad%00path");
  expect(onDirectory).toHaveBeenCalledTimes(1);
});
it("does not replay commands or restart an ended terminal when focus changes", async () => {
  const terminal = vi.fn().mockResolvedValue({ id: "owned" }); const engine = { terminal } as unknown as Engine;
  await mount(engine);
  await act(async () => output.mock.calls.at(-1)![0].ended(0));
  await act(async () => tree!.update(<ShellTerminal engine={engine} active focused autoStart closeOnUnmount generation="g" />));
  expect(terminal.mock.calls.filter(([r]) => r.action === "start")).toHaveLength(1);
  expect(terminal.mock.calls.filter(([r]) => r.action === "write")).toHaveLength(0);
});

it("keeps the running terminal free of chrome and claims control commands once, returning validation failures", async () => {
  const terminal = vi.fn().mockImplementation(async (r: { action: string }) => r.action === "start" ? { id: "owned" } : { claimed: true });
  const onCommand = vi.fn();
  await act(async () => { tree = create(<ShellTerminal engine={{ terminal } as unknown as Engine} active autoStart generation="g" onCommand={onCommand} />,
    { createNodeMock: node => node.props.className === "terminal-emulator" ? { clientWidth: 400, clientHeight: 300 } : null }); });
  expect(tree!.root.findAllByProps({ className: "terminal-toolbar" })).toHaveLength(0);
  expect(JSON.stringify(tree!.toJSON())).not.toContain("End terminal");
  expect(tree!.root.findAllByType("button").map(button => button.children.join(""))).toEqual([]);
  const command = output.mock.calls.at(-1)![0].command;
  await act(async () => command({ id: "once", text: "/split" }));
  expect(onCommand).toHaveBeenCalledWith("/split", undefined);
  expect(terminal).toHaveBeenLastCalledWith({ action: "commandreply", id: "owned", request: "once", error: null }, expect.any(AbortSignal));
  terminal.mockResolvedValueOnce({ claimed: false });
  await command({ id: "once", text: "/split" });
  expect(onCommand).toHaveBeenCalledTimes(1);
  onCommand.mockImplementation(() => { throw new Error("The pane budget is full"); });
  await command({ id: "rejected", text: "/split" });
  expect(terminal).toHaveBeenLastCalledWith({ action: "commandreply", id: "owned", request: "rejected", error: "The pane budget is full" }, expect.any(AbortSignal));
});
it("offers recovery after an automatic start fails", async () => {
  await mount({ terminal: vi.fn().mockRejectedValue(new Error("Unavailable")) } as unknown as Engine);
  expect(applicationLog.snapshot().at(-1)).toMatchObject({ operation: "start", detail: "Unavailable" });
  expect(tree!.root.findAllByProps({ role: "alert" })).toHaveLength(0);
  expect(tree!.root.findByProps({ "aria-label": "Start terminal" }).children).toContain("Start terminal");
});

it("offers a fresh start after a lost input acknowledgement without replaying the write", async () => {
  const terminal = vi.fn().mockImplementation(async (r: { action: string }) => {
    if (r.action === "write") throw new Error("Connection lost");
    return { id: "owned" };
  });
  await mount({ terminal } as unknown as Engine);
  await act(async () => dataHandlers[0]!("one command"));
  expect(tree!.root.findByProps({ "aria-label": "Start terminal" }).children).toContain("Start terminal");
  await act(async () => tree!.root.findByProps({ "aria-label": "Start terminal" }).props.onClick());
  expect(terminal.mock.calls.filter(([r]) => r.action === "write")).toHaveLength(1);
  expect(terminal.mock.calls.map(([r]) => r.action)).toEqual(["start", "resize", "write", "close", "start", "resize"]);
});

it("focus changes never fit or resize the PTY; only real visible host changes do", async () => {
  const terminal = vi.fn().mockResolvedValue({ id: "owned" }); const engine = { terminal } as unknown as Engine;
  const host = { clientWidth: 400, clientHeight: 300 };
  const draw = (focused: boolean, active = true) => <ShellTerminal engine={engine} active={active} focused={focused} autoStart closeOnUnmount generation="g" />;
  await act(async () => { tree = create(draw(false), { createNodeMock: node => node.props.className === "terminal-emulator" ? host : null }); });
  const fitted = fits[0]!; fitted.mockClear(); terminal.mockClear();
  for (const focus of [true, false, true, false]) await act(async () => tree!.update(draw(focus)));
  expect(fitted).not.toHaveBeenCalled();
  expect(terminal).not.toHaveBeenCalled();
  expect(terminals).toHaveLength(1);
  expect(terminals[0]!.focus).toHaveBeenCalledTimes(2);
  act(() => resized[0]!()); // Same-size observer deliveries (including focus/layout notifications).
  expect(fitted).not.toHaveBeenCalled();
  host.clientWidth = 320; act(() => resized[0]!());
  expect(fitted).toHaveBeenCalledTimes(1); // Resize still works when unfocused.
  act(() => resized[0]!()); expect(fitted).toHaveBeenCalledTimes(1);
  host.clientHeight = 240; act(() => resized[0]!()); expect(fitted).toHaveBeenCalledTimes(2);
  host.clientWidth = 0; host.clientHeight = 0; act(() => resized[0]!());
  expect(fitted).toHaveBeenCalledTimes(2); // Hidden workspaces must not collapse the terminal to tiny rows/columns.
  host.clientWidth = 320; host.clientHeight = 240; act(() => resized[0]!());
  expect(fitted).toHaveBeenCalledTimes(3); // Same-size reveal is not mistaken for a duplicate.
  await act(async () => tree!.update(draw(false, false)));
  await act(async () => tree!.update(draw(false, true)));
  expect(fitted).toHaveBeenCalledTimes(4);
  const count = fitted.mock.calls.length;
  await act(async () => tree!.unmount()); tree = undefined;
  act(() => resized[0]!()); expect(fitted).toHaveBeenCalledTimes(count);
});
it("waits for a measurable host when an automatic terminal starts hidden", async () => {
  const host = { clientWidth: 0, clientHeight: 0 };
  await act(async () => { tree = create(<ShellTerminal engine={{ terminal: vi.fn().mockResolvedValue({ id: "hidden" }) } as unknown as Engine} active={false} focused={false} autoStart closeOnUnmount generation="g" />,
    { createNodeMock: node => node.props.className === "terminal-emulator" ? host : null }); });
  expect(fits[0]).not.toHaveBeenCalled();
  host.clientWidth = 400; host.clientHeight = 300;
  act(() => resized[0]!()); expect(fits[0]).toHaveBeenCalledTimes(1);
});

it("fits an expanded terminal and hidden/revealed siblings without recreating or closing the shell", async () => {
  const terminal = vi.fn().mockResolvedValue({ id: "owned" });
  const engine = { terminal } as unknown as Engine;
  const host = { clientWidth: 400, clientHeight: 300 };
  let state = splitPane(oneP(SESSION_PANE), "down", terminalPane("p2"), true);
  const draw = () => <Split state={state} top={[]} prompt={[]} context={[]}
    content={pane => pane.terminal ? <ShellTerminal engine={engine} history={pane.history} active focused={state.focused === pane.id}
      autoStart closeOnUnmount generation="synthetic" /> : <textarea defaultValue="kept draft" />} />;
  await act(async () => { tree = create(draw(), { createNodeMock: node => node.props.className === "terminal-emulator" ? host : null }); });
  const fitted = fits[0]!; fitted.mockClear(); terminal.mockClear();
  const press = (key: string, held = {}) => act(() => tree!.root.findByProps({ className: "split" }).props.onKeyDownCapture({
    key, ...held, preventDefault() {}, stopPropagation() {},
  }));
  press("f", { metaKey: true, shiftKey: true });
  host.clientWidth = 800; host.clientHeight = 600; act(() => resized[0]!());
  expect(fitted).toHaveBeenCalledTimes(1);
  press("Escape");
  host.clientWidth = 400; host.clientHeight = 300; act(() => resized[0]!());
  expect(fitted).toHaveBeenCalledTimes(2);
  state = focus(state, "p1"); await act(async () => tree!.update(draw()));
  press("f", { metaKey: true, shiftKey: true });
  expect(tree!.root.findByProps({ "data-pane-id": "p2" }).props.hidden).toBe(true);
  host.clientWidth = 0; host.clientHeight = 0; act(() => resized[0]!());
  expect(fitted).toHaveBeenCalledTimes(2);
  press("Escape");
  host.clientWidth = 400; host.clientHeight = 300; act(() => resized[0]!());
  expect(fitted).toHaveBeenCalledTimes(3);
  expect(terminals).toHaveLength(1);
  expect(terminal).not.toHaveBeenCalled();
  await act(async () => dataHandlers[0]!("still connected"));
  expect(terminal).toHaveBeenLastCalledWith({ action: "write", id: "owned", text: "still connected" });
});

it("retires an unavailable terminal and starts fresh only on request, ignoring stale callbacks", async () => {
  let starts = 0;
  const terminal = vi.fn().mockImplementation(async (r: { action: string }) => r.action === "start" ? { id: `shell-${++starts}` } : {});
  const engine = { terminal } as unknown as Engine;
  await mount(engine);
  const previous = output.mock.calls.at(-1)![0];
  await act(async () => previous.unavailable(new TerminalUnavailable().message));
  expect(applicationLog.snapshot().at(-1)).toMatchObject({ code: "TERM_UNAVAILABLE", terminal: "shell-1" });
  await act(async () => dataHandlers[0]!("do not submit"));
  await act(async () => tree!.update(<ShellTerminal engine={engine} active focused autoStart closeOnUnmount generation="g" />));
  expect(starts).toBe(1);
  await act(async () => tree!.root.findByProps({ "aria-label": "Start terminal" }).props.onClick());
  expect(starts).toBe(2);
  expect(terminal.mock.calls.map(([r]) => r.action)).toEqual(["start", "resize", "start", "resize"]);
  await act(async () => { previous.unavailable("late"); previous.ended(0); previous.problem("late problem"); previous.connection(new Error("late connection"), "poll"); });
  expect(tree!.root.findAllByType("button").map(button => button.children.join(""))).toEqual([]);
  expect(tree!.root.findAllByProps({ role: "alert" })).toHaveLength(0);
  await act(async () => dataHandlers[0]!("fresh input"));
  expect(terminal).toHaveBeenLastCalledWith({ action: "write", id: "shell-2", text: "fresh input" });
});

it.each([true, false])("restart tolerates already unavailable close only: %s", async unavailable => {
  let starts = 0;
  const terminal = vi.fn().mockImplementation(async (r: { action: string }) => {
    if (r.action === "start") return { id: `shell-${++starts}` };
    if (r.action === "write") throw new Error("Lost reply");
    if (r.action === "close") throw unavailable ? new TerminalUnavailable() : new Error("Close failed");
    return {};
  });
  await mount({ terminal } as unknown as Engine);
  await act(async () => dataHandlers[0]!("uncertain input"));
  await act(async () => tree!.root.findByProps({ "aria-label": "Start terminal" }).props.onClick());
  expect(starts).toBe(unavailable ? 2 : 1);
  expect(terminal.mock.calls.filter(([r]) => r.action === "write")).toHaveLength(1);
  if (!unavailable) expect(applicationLog.snapshot().at(-1)?.detail).toBe("Close failed");
});

it("a late failed write cannot retire the replacement terminal", async () => {
  let reject!: (error: Error) => void, starts = 0;
  const terminal = vi.fn().mockImplementation(async (r: { action: string }) => {
    if (r.action === "start") return { id: `shell-${++starts}` };
    if (r.action === "write") return new Promise((_resolve, fail) => { reject = fail; });
    return {};
  });
  await mount({ terminal } as unknown as Engine);
  await act(async () => dataHandlers[0]!("pending input"));
  await act(async () => output.mock.calls.at(-1)![0].unavailable(new TerminalUnavailable().message));
  await act(async () => tree!.root.findByProps({ "aria-label": "Start terminal" }).props.onClick());
  await act(async () => reject(new TerminalUnavailable()));
  expect(starts).toBe(2);
  expect(tree!.root.findAllByType("button").map(button => button.children.join(""))).toEqual([]);
  expect(tree!.root.findAllByProps({ role: "alert" })).toHaveLength(0);
});

it("coalesces xterm onData bursts behind one in-flight write", async () => {
  const acknowledgements: (() => void)[] = [];
  const terminal = vi.fn().mockImplementation(async (r: { action: string }) => {
    if (r.action === "write") return new Promise<void>(resolve => acknowledgements.push(resolve));
    return { id: "owned" };
  });
  await mount({ terminal } as unknown as Engine);
  await act(async () => { for (let i = 0; i < 100; i++) dataHandlers[0]!("\x1b[<64;40;12M"); dataHandlers[0]!("Ğ😀\r"); });
  const writes = () => terminal.mock.calls.map(([r]) => r).filter(r => r.action === "write");
  expect(writes()).toEqual([{ action: "write", id: "owned", text: "\x1b[<64;40;12M" }]);
  await act(async () => acknowledgements.shift()!());
  expect(writes()).toHaveLength(2);
  expect(writes()[1].text).toBe("\x1b[<64;40;12M".repeat(99) + "Ğ😀\r");
  await act(async () => acknowledgements.shift()!());
});

it.each(["ended", "unavailable", "generation", "unmount"])("discards queued input on %s and does not wait on the old owner", async retirement => {
  const acknowledgements: (() => void)[] = [];
  let starts = 0;
  const terminal = vi.fn().mockImplementation(async (r: { action: string }) => {
    if (r.action === "start") return { id: `shell-${++starts}` };
    if (r.action === "write") return new Promise<void>(resolve => acknowledgements.push(resolve));
    return {};
  });
  const engine = { terminal } as unknown as Engine;
  await mount(engine);
  await act(async () => { dataHandlers[0]!("in flight"); dataHandlers[0]!("discard queued"); });
  await act(async () => {
    if (retirement === "ended") output.mock.calls.at(-1)![0].ended(0);
    if (retirement === "unavailable") output.mock.calls.at(-1)![0].unavailable("Expired");
    if (retirement === "generation") tree!.update(<ShellTerminal engine={engine} active autoStart closeOnUnmount generation="new" />);
    if (retirement === "unmount") { tree!.unmount(); tree = undefined; }
  });
  if (tree) {
    await act(async () => tree!.root.findByProps({ "aria-label": "Start terminal" }).props.onClick());
    await act(async () => dataHandlers[0]!("new input"));
    // A hung old write must not stall the fresh terminal.
    expect(terminal).toHaveBeenLastCalledWith({ action: "write", id: "shell-2", text: "new input" });
  }
  await act(async () => { for (const acknowledge of acknowledgements) acknowledge(); });
  expect(terminal.mock.calls.map(([r]) => r).filter(r => r.action === "write").map(r => r.text))
    .toEqual(tree ? ["in flight", "new input"] : ["in flight"]);
});


it("follows the containing surface palette independently of the root without restarting or refocusing", async () => {
  let palette = "white";
  const surface = { getAttribute: () => palette };
  const host = { clientWidth: 400, clientHeight: 300, closest: () => surface };
  const colors: Record<string, Record<string, string>> = {
    white: { "--ground": "#ffffff", "--terminal": "#ffffff", "--mono-ink": "#1b1d1f", "--mono-ref": "#6a4ba8" },
    paper: { "--ground": "#f4f2ec", "--terminal": "#faf7f0", "--mono-ink": "#1b1d1f", "--mono-ref": "#6a4ba8" },
    ink: { "--ground": "#14161a", "--terminal": "#0d0f12", "--mono-ink": "#d8dbe0", "--mono-ref": "#ae8fd8" },
  };
  vi.stubGlobal("document", { documentElement: {} });
  vi.stubGlobal("getComputedStyle", (element: unknown) => {
    expect(element).toBe(host);
    return { getPropertyValue: (name: string) => colors[palette]![name] ?? "" };
  });
  const observe = vi.fn(), disconnect = vi.fn(); let refresh!: () => void;
  vi.stubGlobal("MutationObserver", class {
    constructor(callback: () => void) { refresh = callback; }
    observe = observe; disconnect = disconnect;
  });
  const terminal = vi.fn().mockResolvedValue({ id: "palette-owned" });
  await act(async () => { tree = create(<ShellTerminal engine={{ terminal } as unknown as Engine} generation="palette-generation" active focused={false} autoStart closeOnUnmount />,
    { createNodeMock: node => node.props.className === "terminal-emulator" ? host : null }); });
  const instance = terminals[0]!;
  expect(observe).toHaveBeenCalledWith(surface, { attributes: true, attributeFilter: ["data-palette", "style"] });
  expect(instance.options.theme).toMatchObject({ background: "#ffffff", foreground: "#1b1d1f", cursor: "#6a4ba8", cursorAccent: "#ffffff", blue: "#0969da" });
  for (const next of ["ink", "paper", "white"]) {
    palette = next;
    act(() => refresh());
    expect(instance.options.theme).toMatchObject({ background: colors[next]!["--terminal"], foreground: colors[next]!["--mono-ink"], cursor: colors[next]!["--mono-ref"],
      blue: next === "ink" ? "#58a6ff" : "#0969da" });
  }
  expect(terminals).toHaveLength(1);
  expect(terminal.mock.calls.filter(([r]) => r.action === "start")).toHaveLength(1);
  expect(terminal.mock.calls.filter(([r]) => r.action === "write")).toHaveLength(0);
  expect(instance.focus).not.toHaveBeenCalled();
  await act(async () => tree!.unmount()); tree = undefined;
  expect(disconnect).toHaveBeenCalledOnce();
});

it("uses the paper palette while Surface CSS tokens are unavailable", async () => {
  vi.stubGlobal("getComputedStyle", () => ({ getPropertyValue: () => "" }));
  await mount({ terminal: vi.fn().mockResolvedValue({ id: "paper-fallback" }) } as unknown as Engine);
  expect(terminals[0]!.options.theme).toMatchObject({ background: "#FAF7F0", foreground: "#1B1D1F", cursor: "#6A4BA8", blue: "#0969da" });
});

it("loads Turkish glyphs and follows the Surface font without replacing the terminal or fitting after disposal", async () => {
  let family = monoFamily("PT Mono"), refresh!: () => void, loaded!: () => void;
  const surface = { getAttribute: () => "paper" }, host = { clientWidth: 400, clientHeight: 300, closest: () => surface };
  const pending: (() => void)[] = [];
  const load = vi.fn(() => new Promise<void>(resolve => pending.push(resolve)));
  const remove = vi.fn();
  vi.stubGlobal("document", { fonts: { load, addEventListener: (_: string, handler: () => void) => { loaded = handler; }, removeEventListener: remove } });
  vi.stubGlobal("getComputedStyle", () => ({ getPropertyValue: (name: string) => name === "--type-mono-family" ? family : "" }));
  vi.stubGlobal("MutationObserver", class { constructor(callback: () => void) { refresh = callback; } observe() {} disconnect() {} });
  const terminal = vi.fn().mockResolvedValue({ id: "font-owned" });
  await act(async () => { tree = create(<ShellTerminal engine={{ terminal } as unknown as Engine} active focused={false} autoStart generation="font-generation" />,
    { createNodeMock: node => node.props.className === "terminal-emulator" ? host : null }); });
  expect(terminals[0]!.options.fontFamily).toBe(monoFamily("PT Mono"));
  expect(load).toHaveBeenLastCalledWith(`14px ${family}`, "ĞğİıŞşÇçÖöÜü");
  fits[0]!.mockClear();
  await act(async () => pending[0]!());
  expect(fits[0]).toHaveBeenCalledOnce();
  family = monoFamily("SF Mono");
  act(() => refresh());
  expect(terminals[0]!.options.fontFamily).toBe(family);
  expect(load).toHaveBeenLastCalledWith(`14px ${family}`, "ĞğİıŞşÇçÖöÜü");
  fits[0]!.mockClear(); act(() => loaded());
  expect(fits[0]).toHaveBeenCalledOnce();
  expect(terminals).toHaveLength(1);
  expect(terminal.mock.calls.filter(([r]) => r.action === "start")).toHaveLength(1);
  expect(terminal.mock.calls.filter(([r]) => r.action === "write")).toHaveLength(0);
  expect(terminals[0]!.focus).not.toHaveBeenCalled();
  act(() => tree!.unmount()); tree = undefined;
  fits[0]!.mockClear(); await act(async () => pending[1]!());
  loaded(); refresh();
  expect(fits[0]).not.toHaveBeenCalled();
  expect(remove).toHaveBeenCalledWith("loadingdone", loaded);
});

it("flushes durable history identity before starting, reuses it on restart, and never forgets on teardown", async () => {
  const history = "d640ba35-630c-4c89-96f0-dfe2f946799b";
  let release!: () => void, starts = 0;
  const beforeStart = vi.fn().mockImplementationOnce(() => new Promise<void>(resolve => { release = resolve; })).mockResolvedValue(undefined);
  const terminal = vi.fn().mockImplementation(async (request: { action: string }) => request.action === "start" ? { id: `process-${++starts}` } : {});
  const engine = { terminal } as unknown as Engine;
  const render = (active = true) => <ShellTerminal engine={engine} active={active} autoStart closeOnUnmount generation="g" history={history} beforeStart={beforeStart} />;
  await act(async () => { tree = create(render(), { createNodeMock: node => node.props.className === "terminal-emulator" ? { clientWidth: 400, clientHeight: 300 } : null }); });
  expect(beforeStart).toHaveBeenCalledOnce(); expect(terminal).not.toHaveBeenCalled();
  await act(async () => release());
  expect(terminal).toHaveBeenCalledWith({ action: "start", history });
  await act(async () => tree!.update(render(false)));
  expect(terminal.mock.calls.filter(([request]) => request.action === "close" || request.action === "forget")).toHaveLength(0);
  await act(async () => output.mock.calls.at(-1)![0].ended(0));
  await act(async () => tree!.root.findByProps({ "aria-label": "Start terminal" }).props.onClick());
  expect(terminal.mock.calls.filter(([request]) => request.action === "start").map(([request]) => request.history)).toEqual([history, history]);
  await act(async () => tree!.unmount()); tree = undefined;
  expect(terminal).toHaveBeenLastCalledWith({ action: "close", id: "process-2" });
  expect(terminal.mock.calls.some(([request]) => request.action === "forget")).toBe(false);
});
it("does not start after preference failure, pane teardown or generation change during the flush", async () => {
  let release!: () => void;
  const beforeStart = vi.fn().mockRejectedValueOnce(new Error("preferences not saved"))
    .mockImplementationOnce(() => new Promise<void>(resolve => { release = resolve; }));
  const terminal = vi.fn(), engine = { terminal } as unknown as Engine;
  const render = (generation: string) => <ShellTerminal engine={engine} active autoStart closeOnUnmount generation={generation}
    history="d640ba35-630c-4c89-96f0-dfe2f946799b" beforeStart={beforeStart} />;
  await act(async () => { tree = create(render("g1"), { createNodeMock: node => node.props.className === "terminal-emulator" ? { clientWidth: 400, clientHeight: 300 } : null }); });
  expect(applicationLog.snapshot().at(-1)?.detail).toBe("preferences not saved");
  expect(terminal).not.toHaveBeenCalled();
  await act(async () => tree!.root.findByProps({ "aria-label": "Start terminal" }).props.onClick());
  await act(async () => tree!.update(render("g2")));
  await act(async () => release());
  expect(terminal).not.toHaveBeenCalled();
  expect(tree!.root.findByProps({ "aria-label": "Start terminal" }).props.disabled).toBe(false);
});
it("awaits transactional pane commands and returns their failure instead of a premature success", async () => {
  let fail!: (error: Error) => void;
  const onCommand = vi.fn(() => new Promise<void>((_, reject) => { fail = reject; }));
  const terminal = vi.fn().mockImplementation(async (request: { action: string }) => request.action === "start" ? { id: "owned" } : { claimed: true });
  await act(async () => { tree = create(<ShellTerminal engine={{ terminal } as unknown as Engine} active autoStart generation="g" onCommand={onCommand} />,
    { createNodeMock: node => node.props.className === "terminal-emulator" ? { clientWidth: 400, clientHeight: 300 } : null }); });
  let pending!: Promise<void>;
  await act(async () => { pending = output.mock.calls.at(-1)![0].command({ id: "close-request", text: "/close" }); });
  expect(terminal.mock.calls.some(([request]) => request.action === "commandreply")).toBe(false);
  await act(async () => { fail(new Error("history could not be forgotten")); await pending; });
  expect(terminal).toHaveBeenLastCalledWith({ action: "commandreply", id: "owned", request: "close-request", error: "history could not be forgotten" }, expect.any(AbortSignal));
});
it("a stale restart cleanup reply cannot retire the replacement process after a workspace change", async () => {
  let finish!: () => void, starts = 0;
  const terminal = vi.fn().mockImplementation(async (request: { action: string }) => {
    if (request.action === "start") return { id: `shell-${++starts}` };
    if (request.action === "close") return new Promise<void>(resolve => { finish = resolve; });
    return {};
  });
  const engine = { terminal } as unknown as Engine;
  const render = (generation: string) => <ShellTerminal engine={engine} active generation={generation} history="d640ba35-630c-4c89-96f0-dfe2f946799b" />;
  await act(async () => { tree = create(render("g1"), { createNodeMock: node => node.props.className === "terminal-emulator" ? { clientWidth: 400, clientHeight: 300 } : null }); });
  const button = (label: string) => tree!.root.findAllByType("button").find(node => node.children.includes(label))!;
  await act(async () => button("Start terminal").props.onClick());
  await act(async () => output.mock.calls.at(-1)![0].ended(0));
  await act(async () => button("Start terminal").props.onClick());
  await act(async () => tree!.update(render("g2")));
  await act(async () => button("Start terminal").props.onClick());
  await act(async () => finish());
  await act(async () => dataHandlers[0]!("fresh input"));
  expect(terminal).toHaveBeenLastCalledWith({ action: "write", id: "shell-2", text: "fresh input" });
});


it("logs one output outage and its recovery while preserving uncertain input and server errors", async () => {
  const terminal = vi.fn().mockImplementation(async (r: { action: string }) => {
    if (r.action === "write") throw new Error("acknowledgement lost");
    return { id: "owned" };
  });
  await mount({ terminal, binding: "agent-lab" } as unknown as Engine);
  const sink = output.mock.calls.at(-1)![0];
  await act(async () => {
    sink.connection(new Error("read lost"), "poll");
    sink.connection(new Error("read lost"), "poll");
    sink.problem("PTY reader failed");
    dataHandlers[0]!("secret command");
  });
  await act(async () => sink.connection());
  expect(applicationLog.snapshot().map(r => r.operation)).toEqual(["poll", "server report", "write", "poll"]);
  expect(applicationLog.snapshot().at(-1)).toMatchObject({ code: "TERM_RECOVERED", workspace: "agent-lab", terminal: "owned", generation: "g" });
  expect(JSON.stringify(applicationLog.snapshot())).not.toContain("secret command");
  expect(tree!.root.findAllByProps({ role: "alert" })).toHaveLength(0);
  expect(tree!.root.findByProps({ "aria-label": "Start terminal" }).children).toContain("Start terminal");
  expect(terminal.mock.calls.filter(([r]) => r.action === "write")).toHaveLength(1);
  await act(async () => tree!.unmount()); tree = undefined;
  expect(applicationLog.snapshot()).toHaveLength(4);
});

it("a successful read does not claim recovery of a failed resize", async () => {
  let resizes = 0;
  const terminal = vi.fn().mockImplementation(async (r: { action: string }) => {
    if (r.action === "resize" && ++resizes === 1) throw new Error("size lost");
    return { id: "owned" };
  });
  await mount({ terminal } as unknown as Engine);
  await act(async () => output.mock.calls.at(-1)![0].connection());
  expect(applicationLog.snapshot().map(r => r.code)).toEqual(["TERM_OPERATION_FAILED"]);
  await act(async () => resizeHandlers[0]!({ cols: 100, rows: 30 }));
  expect(applicationLog.snapshot().at(-1)).toMatchObject({ code: "TERM_RECOVERED", operation: "resize" });
  expect(terminal.mock.calls.filter(([r]) => r.action === "start")).toHaveLength(1);
});

it("remembers a captured target without automatically opening it and ignores remote cwd sequences", async () => {
  const target = { environment: "qa", revision: `sha256:${"a".repeat(64)}`, target: "server" };
  const terminal = vi.fn().mockImplementation(async (request: { action: string }) => request.action === "start" ? { id: "remote", workspace_tools: false } : {});
  const onDirectory = vi.fn();
  await act(async () => { tree = create(<ShellTerminal engine={{terminal} as unknown as Engine} active autoStart closeOnUnmount generation="g" target={target} cwd="/host/saved" onDirectory={onDirectory} />,
    {createNodeMock: node => node.props.className === "terminal-emulator" ? {clientWidth:400,clientHeight:300} : null}); });
  expect(terminal).not.toHaveBeenCalled();
  await act(async () => tree!.root.findAllByType("button").find(button => button.props.children === "Start terminal")!.props.onClick());
  expect(terminal.mock.calls[0]![0]).toEqual({action:"start",target});
  directoryHandlers[0]!("file://remote/remote/path");
  expect(onDirectory).not.toHaveBeenCalled();
  expect(JSON.stringify(tree!.toJSON())).toContain("qa / server");
});

it("starts an explicitly requested target once and keeps its endpoint metadata across focus changes", async () => {
  const destination = `Docker container ${"a".repeat(64)}`;
  const target = { environment:"qa",revision:`sha256:${"b".repeat(64)}`,target:"api" };
  const terminal = vi.fn().mockImplementation(async (r: {action:string}) => r.action === "start" ? {id:"container",workspace_tools:false,destination} : {});
  const engine = {terminal} as unknown as Engine;
  const draw = (focused: boolean) => <ShellTerminal engine={engine} active autoStart allowTargetStart focused={focused} generation="g" target={target} />;
  await act(async () => { tree=create(draw(false),
    {createNodeMock: node => node.props.className === "terminal-emulator" ? {clientWidth:400,clientHeight:300} : null}); });
  expect(terminal.mock.calls[0]![0]).toEqual({action:"start",target});
  expect(tree!.root.findAllByType("select")).toHaveLength(0);
  expect(tree!.root.findAllByType("button").map(button => button.children.join(""))).toEqual([]);
  expect(tree!.root.findByProps({ "aria-label": "Shell terminal" }).props["aria-description"]).toContain(destination);
  await act(async () => tree!.update(draw(true)));
  expect(terminal.mock.calls.filter(([r])=>r.action==="start")).toHaveLength(1);
  expect(tree!.root.findByProps({ "aria-label": "Shell terminal" }).props["aria-description"]).toContain(destination);
});

it("forwards the originating actor environment with a pane command", async () => {
  const terminal = vi.fn().mockImplementation(async (r: {action:string}) => r.action === "start" ? {id:"local"} : {claimed:true});
  const onCommand = vi.fn();
  await act(async () => { tree=create(<ShellTerminal engine={{terminal} as unknown as Engine} active autoStart generation="g" onCommand={onCommand} />,
    {createNodeMock: node => node.props.className === "terminal-emulator" ? {clientWidth:400,clientHeight:300} : null}); });
  await act(async () => output.mock.calls.at(-1)![0].command({id:"cmd",text:"/rsplit xterm",environment:"agent-env"}));
  expect(onCommand).toHaveBeenCalledWith("/rsplit xterm", "agent-env");
});

const reviewTarget = { environment: "qa", target: "local", revision: `sha256:${"a".repeat(64)}` };
const reviewReply = (previous = reviewTarget, char = "b") => ({ review: {
  target: { ...previous, revision: `sha256:${char.repeat(64)}` }, previousRevision: previous.revision, previousAvailable: true,
  before: { kind: "Local", destination: "This computer", shell: "Login shell", cwd: "/", variables: [], transport: {} },
  after: { kind: "Local", destination: "This computer", shell: "Login shell", cwd: "/tmp", variables: ["QA"], transport: {} },
  providers: ["echo"], changes: { added: ["echo"], removed: [], updated: [], configuration: [], credentialReferences: [], targetChanged: true, variables: ["QA"] },
} });
async function reviewMount(terminal: ReturnType<typeof vi.fn>, persist = vi.fn().mockResolvedValue(undefined)) {
  const { useState } = await import("react");
  const engine = { terminal } as unknown as Engine;
  function Harness() {
    const [target, setTarget] = useState(reviewTarget);
    return <ShellTerminal engine={engine} active autoStart allowTargetStart closeOnUnmount generation="g" history="review-history" target={target}
      onTargetChange={async (previous, next) => { await persist(previous, next); setTarget(next); }} />;
  }
  await act(async () => { tree = create(<Harness />, { createNodeMock: node => node.props.className === "terminal-emulator" ? { clientWidth: 400, clientHeight: 300 } : null }); });
  return persist;
}
const reviewButton = (label: string) => tree!.root.findAllByType("button").find(b => b.children.join("") === label)!;
it("reviews changes without logging an error; cancellation neither saves nor starts a process", async () => {
  const terminal = vi.fn().mockResolvedValue(reviewReply());
  const persist = await reviewMount(terminal);
  expect(tree!.root.findAllByProps({ "aria-label": "Review environment changes" })).toHaveLength(1);
  expect(JSON.stringify(tree!.toJSON())).toContain("Providers added");
  expect(applicationLog.snapshot()).toHaveLength(0);
  await act(async () => reviewButton("Cancel").props.onClick());
  expect(persist).not.toHaveBeenCalled();
  expect(terminal).toHaveBeenCalledTimes(1);
  expect(tree!.root.findAllByProps({ "aria-label": "Review environment changes" })).toHaveLength(0);
});
it("saves the reviewed revision before starting and preserves the terminal history", async () => {
  const reply = reviewReply();
  const events: string[] = [];
  const terminal = vi.fn().mockImplementation(async (r: { action: string }) => { events.push(r.action); return events.length === 1 ? reply : { id: "confirmed" }; });
  const persist = await reviewMount(terminal, vi.fn(async () => { events.push("save"); }));
  await act(async () => reviewButton("Start with current settings").props.onClick());
  expect(persist).toHaveBeenCalledWith(reviewTarget, reply.review.target);
  expect(events).toEqual(["start", "save", "start", "resize"]);
  expect(terminal.mock.calls[1]![0]).toEqual({ action: "start", target: reply.review.target, history: "review-history" });
  expect(tree!.root.findAllByProps({ "aria-label": "Review environment changes" })).toHaveLength(0);
});
it("requires another approval if the environment changes while the review is open", async () => {
  const first = reviewReply(), second = reviewReply(first.review.target, "c");
  const terminal = vi.fn().mockResolvedValueOnce(first).mockResolvedValueOnce(second).mockResolvedValue({ id: "confirmed" });
  const persist = await reviewMount(terminal);
  await act(async () => reviewButton("Start with current settings").props.onClick());
  expect(persist).toHaveBeenCalledTimes(1);
  expect(terminal).toHaveBeenCalledTimes(2);
  expect(JSON.stringify(tree!.toJSON())).toContain(second.review.target.revision);
  await act(async () => reviewButton("Start with current settings").props.onClick());
  expect(persist).toHaveBeenLastCalledWith(first.review.target, second.review.target);
  expect(terminal.mock.calls[2]![0].target).toEqual(second.review.target);
});
it("does not start after a failed approval save or a pane closed during save", async () => {
  const terminal = vi.fn().mockResolvedValue(reviewReply());
  await reviewMount(terminal, vi.fn().mockRejectedValue(new Error("disk unavailable")));
  await act(async () => reviewButton("Start with current settings").props.onClick());
  expect(terminal).toHaveBeenCalledTimes(1);
  expect(applicationLog.snapshot().at(-1)).toMatchObject({ operation: "save reviewed target", detail: "disk unavailable" });
  await act(async () => tree!.unmount()); tree = undefined;
  let release!: () => void;
  const save = new Promise<void>(resolve => { release = resolve; });
  terminal.mockClear();
  await reviewMount(terminal, vi.fn(() => save));
  await act(async () => reviewButton("Start with current settings").props.onClick());
  await act(async () => tree!.unmount()); tree = undefined;
  await act(async () => release());
  expect(terminal).toHaveBeenCalledTimes(1);
});
it("ignores a review returned to a closed pane without trying to close a nonexistent process", async () => {
  let release!: (reply: ReturnType<typeof reviewReply>) => void;
  const terminal = vi.fn(() => new Promise(resolve => { release = resolve; }));
  await reviewMount(terminal);
  await act(async () => tree!.unmount()); tree = undefined;
  await act(async () => release(reviewReply()));
  expect(terminal).toHaveBeenCalledTimes(1);
});
it("explains unavailable previous revisions without claiming there were no changes", async () => {
  const reply = reviewReply();
  const terminal = vi.fn().mockResolvedValue({ review: { ...reply.review, previousAvailable: false, before: null, changes: null } });
  await reviewMount(terminal);
  expect(JSON.stringify(tree!.toJSON())).toContain("Changes cannot be compared");
  expect(JSON.stringify(tree!.toJSON())).not.toContain("Providers added");
});

it("invalidates an old review handler when the workspace generation changes", async () => {
  const terminal = vi.fn().mockResolvedValue(reviewReply()), save = vi.fn();
  const engine = { terminal } as unknown as Engine;
  const props = { engine, active: true, autoStart: true, allowTargetStart: true, target: reviewTarget, onTargetChange: save };
  await act(async () => { tree = create(<ShellTerminal {...props} generation="old" />, { createNodeMock: node => node.props.className === "terminal-emulator" ? { clientWidth: 400, clientHeight: 300 } : null }); });
  const staleApprove = reviewButton("Start with current settings").props.onClick;
  await act(async () => tree!.update(<ShellTerminal {...props} generation="new" />));
  await act(async () => staleApprove());
  expect(save).not.toHaveBeenCalled();
  expect(terminal).toHaveBeenCalledTimes(1);
  expect(tree!.root.findAllByProps({ "aria-label": "Review environment changes" })).toHaveLength(0);
});
