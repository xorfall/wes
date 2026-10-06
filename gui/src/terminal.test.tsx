vi.mock("./workspace-events", () => ({ WorkspaceEvents: function(path: string) { return new EventSource(path); } }));
import { TerminalUnavailable } from "./terminal-errors";
import { afterEach, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ShellTerminal, xtermTheme } from "./ShellTerminal";
import { variables } from "./surface/cascade.test-support";
import { Engine } from "./engine";
import { terminalChunks } from "./terminal-input";
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });
it("uses the accepted terminal surface for both canvas and cursor contrast in each palette", () => {
  for (const palette of ["paper", "ink", "white"] as const) {
    const tokens = variables({palette,density:"normal"});
    vi.stubGlobal("getComputedStyle", () => ({getPropertyValue:(name:string)=>tokens.get(name) ?? ""}));
    const host = {closest:()=>({getAttribute:()=>palette})} as unknown as HTMLElement;
    const theme = xtermTheme(host);
    expect(theme.background).toBe(tokens.get("--terminal"));
    expect(theme.cursorAccent).toBe(theme.background);
    expect(theme.foreground).toBe(tokens.get("--mono-ink"));
    if (palette === "paper") expect(theme.background).toBe(tokens.get("--sheet"));
    if (palette === "ink") expect(theme.background).not.toBe(tokens.get("--sheet"));
  }
});
it("does not spawn a shell or restore authority during render", () => {
  const engine = new Engine(); const start = vi.spyOn(engine, "terminal");
  const html = renderToStaticMarkup(<ShellTerminal engine={engine} active generation="fresh" />);
  expect(html).toContain("Start terminal"); expect(html).not.toContain("End terminal");
  expect(start).not.toHaveBeenCalled();
});
it("terminal requests are generation/client scoped and are never automatically retried", async () => {
  class Events { static current: Events; onmessage?: (e: { data: string }) => void; constructor() { Events.current = this; } close() {} }
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockRejectedValue(new Error("lost reply")); vi.stubGlobal("fetch", fetch);
  const engine = new Engine(); engine.listen(() => {}, () => {});
  await expect(engine.terminal({ action: "start" })).rejects.toThrow("Wait");
  expect(fetch).not.toHaveBeenCalled();
  Events.current.onmessage?.({ data: JSON.stringify({ event: "session", workspace: null, generation: "g" }) });
  await expect(engine.terminal({ action: "start" })).rejects.toThrow("lost reply");
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(fetch.mock.calls[0]![1].headers["X-Wes-Session"]).toBe("g");
  expect(JSON.parse(fetch.mock.calls[0]![1].body).client).toBe(engine.client);
});

it("large Unicode pastes preserve scalar boundaries and exact input under the byte limit", () => {
  const text = "a".repeat(2047) + "😀".repeat(4096) + "\r";
  const chunks = terminalChunks(text);
  expect(chunks.join("")).toBe(text);
  for (const chunk of chunks) {
    expect(new TextDecoder("utf-8", { fatal: true }).decode(new TextEncoder().encode(chunk))).toBe(chunk);
    expect(new TextEncoder().encode(chunk).length).toBeLessThanOrEqual(8192);
  }
});

it("terminal reads can abort and reject a body arriving after the workspace changes", async () => {
  class Events { static current: Events; onmessage?: (e: { data: string }) => void; constructor() { Events.current = this; } close() {} }
  vi.stubGlobal("EventSource", Events);
  const engine = new Engine(); engine.listen(() => {}, () => {});
  const session = (generation: string) => Events.current.onmessage?.({ data: JSON.stringify({ event: "session", workspace: null, generation }) });
  session("old");
  const fetch = vi.fn().mockImplementation((_url, options) => new Promise((_resolve, reject) => options.signal.addEventListener("abort", () => reject(new Error("aborted")))));
  vi.stubGlobal("fetch", fetch);
  const controller = new AbortController(); const pending = engine.terminal({ action: "poll" }, controller.signal);
  controller.abort(); await expect(pending).rejects.toThrow("aborted");
  expect(fetch).toHaveBeenCalledTimes(1);
  fetch.mockResolvedValueOnce({ ok: true, text: async () => { session("new"); return "{}"; } });
  await expect(engine.terminal({ action: "poll" })).rejects.toThrow("Workspace changed");
});

it("distinguishes ended authority from retryable terminal transport failures", async () => {
  class Events { static current: Events; onmessage?: (e: { data: string }) => void; constructor() { Events.current = this; } close() {} }
  vi.stubGlobal("EventSource", Events);
  const engine = new Engine(); engine.listen(() => {}, () => {});
  Events.current.onmessage?.({ data: JSON.stringify({ event: "session", workspace: null, generation: "g" }) });
  const fetch = vi.fn().mockResolvedValueOnce({ ok: false, status: 410 })
    .mockResolvedValueOnce({ ok: false, status: 503, text: async () => "Temporarily unavailable" });
  vi.stubGlobal("fetch", fetch);
  await expect(engine.terminal({ action: "poll", id: "expired" })).rejects.toBeInstanceOf(TerminalUnavailable);
  await expect(engine.terminal({ action: "poll", id: "live" })).rejects.toMatchObject({ code: "TERM_HTTP_503", operation: "poll", detail: "Temporarily unavailable" });
  expect(fetch).toHaveBeenCalledTimes(2);
});


it.each(["fetch", "body"])("classifies the actual deadline during %s without retrying", async stage => {
  class Events { static current: Events; onmessage?: (e: { data: string }) => void; constructor() { Events.current = this; } close() {} }
  vi.stubGlobal("EventSource", Events);
  const deadline = new AbortController();
  vi.spyOn(AbortSignal, "timeout").mockReturnValue(deadline.signal);
  const engine = new Engine(); engine.listen(() => {}, () => {});
  Events.current.onmessage?.({ data: JSON.stringify({ event: "session", workspace: null, generation: "g" }) });
  const blocked = () => new Promise((_resolve, reject) => deadline.signal.addEventListener("abort", () => reject(new DOMException("Fetch is aborted", "AbortError"))));
  const fetch = vi.fn().mockImplementation(() => stage === "fetch" ? blocked() : Promise.resolve({ ok: true, text: blocked }));
  vi.stubGlobal("fetch", fetch);
  const pending = engine.terminal({ action: "write", text: "not to be logged" });
  await Promise.resolve(); deadline.abort();
  await expect(pending).rejects.toMatchObject({ code: "TERM_TIMEOUT", operation: "write", message: "Terminal request timed out after 15 seconds." });
  expect(fetch).toHaveBeenCalledTimes(1);
});

it("distinguishes an unexplained browser abort from a deadline", async () => {
  class Events { static current: Events; onmessage?: (e: { data: string }) => void; constructor() { Events.current = this; } close() {} }
  vi.stubGlobal("EventSource", Events);
  const engine = new Engine(); engine.listen(() => {}, () => {});
  Events.current.onmessage?.({ data: JSON.stringify({ event: "session", workspace: null, generation: "g" }) });
  vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new DOMException("Fetch is aborted", "AbortError")));
  await expect(engine.terminal({ action: "poll" })).rejects.toMatchObject({ code: "TERM_ABORTED", source: "Browser → terminal server", operation: "poll" });
});
