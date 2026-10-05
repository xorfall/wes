vi.mock("./workspace-events", () => ({ WorkspaceEvents: function(path: string) { return new EventSource(path); } }));
import { afterEach, expect, it, vi } from "vitest";
import { Engine } from "./engine";
import { StorageError } from "./storage-error";
import type { FrameSample } from "./value-views/instances";
import { max_active_view_roots } from "./value-views/limits";
import { valueViewModules } from "./value-views/registry";

class Events {
  static current: Events;
  onmessage?: (message: { data: string }) => void;
  onerror?: () => void;
  constructor() { Events.current = this; }
  close() {}
  say(generation: string) { this.onmessage?.({ data: JSON.stringify({ event: "session", generation }) }); }
}
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

it("renews static view leases without rereading frames, but refreshes explicit observation changes", async () => {
  vi.useFakeTimers(); vi.stubGlobal("EventSource", Events);
  const metric = valueViewModules.named("metric")!.definition!;
  const frame = { root: "node", instances: [{ id: "node", instance: "identity", revision: "0", definition: metric.id, digest: metric.digest, artifact: metric.artifact,
    inputReference: { kind: "unlinked" }, inputDelivery: "finite", observing: false, inputRevision: "0", input: null, inputProblem: null, inputCautions: [], query: null, linkedInputs: [], members: {} }] };
  const fetch = vi.fn(async (url: string, _init: { body?: string }) => ({ ok: true, status: 200, headers: { get: () => "etag" },
    text: async () => JSON.stringify(url.startsWith("/view-mounts/") ? { token: "lease" } : frame) }));
  vi.stubGlobal("fetch", fetch);
  const engine = new Engine(), disconnect = engine.listen(() => {}, () => {}); Events.current.say("g");
  const seen: FrameSample[] = [], close = engine.watchViewFrame("node", "identity", sample => seen.push(sample));
  await vi.advanceTimersByTimeAsync(5100);
  expect(seen.at(-1)?.frame).toEqual(frame);
  expect(fetch.mock.calls.filter(([url]) => url.startsWith("/view-instances/"))).toHaveLength(1);
  expect(fetch.mock.calls.filter(([, init]) => init.body && JSON.parse(init.body).action === "touch")).toHaveLength(5);
  await engine.viewObservation("node", "identity", "g", true);
  await vi.advanceTimersByTimeAsync(100);
  expect(fetch.mock.calls.filter(([url]) => url.startsWith("/view-instances/"))).toHaveLength(2);
  expect(fetch.mock.calls.some(([url]) => url === "/submit")).toBe(false);
  close(); disconnect(); await vi.advanceTimersByTimeAsync(1);
});

it.each(["open", "touch"])("explains a %s lease timeout without automatic replay", async failing => {
  vi.useFakeTimers(); vi.stubGlobal("EventSource", Events);
  vi.spyOn(AbortSignal, "timeout").mockImplementation(ms => {
    const controller = new AbortController();
    setTimeout(() => controller.abort(), ms);
    return controller.signal;
  });
  const fetch = vi.fn(async (_url: string, init: { body?: string; signal: AbortSignal }) => {
    const action = init.body ? JSON.parse(init.body).action : undefined;
    if (action === failing) return await new Promise<never>((_resolve, reject) => {
      init.signal.addEventListener("abort", () => reject(new DOMException("Fetch is aborted", "AbortError")), { once: true });
    });
    return { ok: true, status: action ? 200 : 304, text: async () => JSON.stringify({ token: "lease" }) };
  });
  vi.stubGlobal("fetch", fetch);
  const engine = new Engine(), disconnect = engine.listen(() => {}, () => {}); Events.current.say("g");
  const seen: FrameSample[] = [];
  const close = engine.watchViewFrame("node", "identity", sample => seen.push(sample));
  await vi.advanceTimersByTimeAsync(3100);
  expect(seen.at(-1)?.problem).toContain("2-second budget");
  expect(seen.at(-1)?.problem).toContain("unconfirmed");
  expect(seen.at(-1)?.problem).not.toContain("Fetch is aborted");
  const calls = fetch.mock.calls.length; await vi.advanceTimersByTimeAsync(5000);
  expect(fetch).toHaveBeenCalledTimes(calls);
  expect(fetch.mock.calls.filter(([, init]) => init.body && JSON.parse(init.body).action === "open")).toHaveLength(1);
  expect(fetch.mock.calls.some(([url]) => url === "/submit")).toBe(false);
  close(); disconnect(); await vi.advanceTimersByTimeAsync(1);
});

it("admits waiting visible roots after cleanup without submitting source commands",async()=>{
  vi.useFakeTimers();vi.stubGlobal("EventSource",Events);
  const fetch=vi.fn(async(_url:string,init:{body?:string})=>{
    const action=init.body?JSON.parse(init.body).action:undefined;
    return {ok:true,status:action?200:304,text:async()=>JSON.stringify({token:action==="open"?"token":undefined})};
  });
  vi.stubGlobal("fetch",fetch);
  const engine=new Engine(),disconnect=engine.listen(()=>{},()=>{});Events.current.say("g");
  const active=Array.from({length:max_active_view_roots()},(_,n)=>engine.watchViewFrame(`n${n}`,`i${n}`,()=>{}));
  const seen:FrameSample[]=[];
  const waiting=engine.watchViewFrame("next","identity",sample=>seen.push(sample));
  expect(seen.at(-1)).toEqual({paused:true});
  await vi.advanceTimersByTimeAsync(100);active[0]!();await vi.advanceTimersByTimeAsync(100);
  expect(seen.at(-1)).toEqual({});
  expect(fetch.mock.calls.filter(([,init])=>init.body&&JSON.parse(init.body).action==="open")).toHaveLength(max_active_view_roots()+1);
  expect(fetch.mock.calls.every(([url])=>url.startsWith("/view-mounts/")||url.startsWith("/view-instances/"))).toBe(true);
  active.slice(1).forEach(close=>close());waiting();disconnect();
  await vi.advanceTimersByTimeAsync(1);
});

it("ends frame reads and reports a failed renewal without automatically reopening or restarting a query",async()=>{
  vi.useFakeTimers();vi.stubGlobal("EventSource",Events);
  const fetch=vi.fn(async(_url:string,init:{body?:string})=>{
    const action=init.body?JSON.parse(init.body).action:undefined;
    return {ok:action!=="touch",status:action==="touch"?403:action?200:304,text:async()=>action==="touch"?"View display session expired or closed":JSON.stringify({token:"token"})};
  });
  vi.stubGlobal("fetch",fetch);
  const engine=new Engine(),disconnect=engine.listen(()=>{},()=>{});Events.current.say("g");
  const seen:FrameSample[]=[];
  const close=engine.watchViewFrame("n","i",sample=>seen.push(sample));
  await vi.advanceTimersByTimeAsync(1000);
  expect(seen.at(-1)).toEqual({problem:"View display session expired or closed"});
  const calls=fetch.mock.calls.length;await vi.advanceTimersByTimeAsync(5000);
  expect(fetch).toHaveBeenCalledTimes(calls);
  close();disconnect();await vi.advanceTimersByTimeAsync(1);
});

it("sends each engine's own identity for source, console, repeat and revision submissions", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true, text: async () => "" });
  vi.stubGlobal("fetch", fetch);
  vi.stubGlobal("window", { fetch });
  const first = new Engine(), second = new Engine();
  expect(first.client).not.toBe(second.client);
  for (const engine of [first, second]) {
    const stop = engine.listen(() => {}, () => {}); Events.current.say("generation");
    await engine.submit("plain", ":help");
    await engine.submitConsole("console", ":help");
    await engine.rerun("repeat", ":help", "plain");
    await engine.revise("revision", ":help types", "plain");
    for (const [, init] of fetch.mock.calls.slice(-4)) expect(JSON.parse(init.body).client).toBe(engine.client);
    stop();
  }
  expect(fetch).toHaveBeenCalledTimes(8);
});

it("preserves authoritative work deletion blockers and does not retry them", async () => {
  vi.stubGlobal("EventSource", Events);
  const blockers = [{ node: "id1001", cells: ["affected"], state: "Pending", reason: "work not completed" }];
  const fetch = vi.fn().mockResolvedValue({ ok: false, text: async () => JSON.stringify({ code: "STO007", message: "Deletion blocked.", blockers }) });
  vi.stubGlobal("fetch", fetch);
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("g");
  const failure = await engine.previewDeleteWork("attempt").catch(error => error);
  expect(failure).toBeInstanceOf(StorageError); expect(failure.blockers).toEqual(blockers);
  expect(failure.message).toBe("Deletion blocked."); expect(fetch).toHaveBeenCalledOnce();
});

it("work deletion sends only reviewed authority and accepts its own generation transition", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true, text: async () => JSON.stringify({ token: "reviewed" }) });
  vi.stubGlobal("fetch", fetch);
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("before");
  expect((await engine.previewDeleteWork("attempt")).token).toBe("reviewed");
  expect(JSON.parse(fetch.mock.calls[0]![1].body)).toEqual({ request: "delete-work-preview", cell: "attempt", client: engine.client });
  fetch.mockImplementationOnce(async () => { Events.current.say("after"); return { ok: true, text: async () => '{"deleted":true}' }; });
  await engine.deleteWork("reviewed", true, false);
  expect(JSON.parse(fetch.mock.calls[1]![1].body)).toEqual({ request: "delete-work", token: "reviewed", client: engine.client, dependents: true, protected: false });
  expect(fetch.mock.calls[1]![1].headers["X-Wes-Session"]).toBe("before");
  fetch.mockRejectedValueOnce(new Error("disconnect"));
  await expect(engine.deleteWork("another", false, true)).rejects.toThrow("completion is unknown");
  expect(fetch).toHaveBeenCalledTimes(3);
});

it("storage confirmation uses the reviewed token and forwards owner errors without retry", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true, text: async () => JSON.stringify({ token: "reviewed", handle: "h" }) });
  vi.stubGlobal("fetch", fetch);
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("g");
  expect((await engine.previewRelease("h")).token).toBe("reviewed");
  expect(JSON.parse(fetch.mock.calls[0]![1].body)).toEqual({ request: "release-preview", handle: "h", client: engine.client });
  fetch.mockResolvedValue({ ok: false, text: async () => JSON.stringify({ code: "STO003", message: "Reviewed storage changed; nothing deleted." }) });
  await expect(engine.release("reviewed")).rejects.toThrow("Reviewed storage changed");
  expect(JSON.parse(fetch.mock.calls[1]![1].body)).toEqual({ request: "release", token: "reviewed", client: engine.client });
  expect(fetch).toHaveBeenCalledTimes(2);
  fetch.mockRejectedValue(new Error("lost connection"));
  await expect(engine.release("another-token")).rejects.toThrow("Stored copies may have changed");
  expect(fetch).toHaveBeenCalledTimes(3);
});

it("discards a storage preview belonging to a replaced session", async () => {
  vi.stubGlobal("EventSource", Events);
  let finish!: (value: unknown) => void;
  vi.stubGlobal("fetch", vi.fn().mockImplementation(() => new Promise(resolve => { finish = resolve; })));
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("one");
  const pending = engine.previewRelease("h"); Events.current.say("two");
  finish({ ok: true, text: async () => JSON.stringify({ token: "old" }) });
  await expect(pending).rejects.toThrow("Workspace changed");
});

it("uses the published default for new clients, but never overrides an explicit selection or clear", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true }); vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("g");
  const state = { event: "environments", managed: true, default: "default", revisions: { default: "d", "pg-demo": "p" }, enabled: {}, credentials: {}, providers: {}, clients: {} };
  const publish = (clients = {}) => Events.current.onmessage?.({ data: JSON.stringify({ ...state, clients }) });
  publish();
  expect(engine.environmentContext()).toEqual({ selected: "default", revisions: state.revisions });
  await engine.supply("fixture-token", "synthetic-only");
  expect(JSON.parse(fetch.mock.calls[0]![1].body).request).toBe("secret");
  fetch.mockClear();
  engine.compose("sh run command:hello");
  publish({ [engine.client]: { selected: "pg-demo", revisions: state.revisions } });
  expect(engine.environmentContext()?.selected).toBe("pg-demo");
  await expect(engine.supply("fixture-token", "synthetic-only")).rejects.toThrow("scoped environment");
  await engine.submitComposed("draft", "sh run command:hello");
  expect(JSON.parse(fetch.mock.calls[0]![1].body).environments.selected).toBe("default");
  publish({ [engine.client]: { selected: null, revisions: state.revisions } });
  expect(engine.environmentContext()?.selected).toBeNull();
  Events.current.say("next"); publish();
  expect(engine.environmentContext()?.selected).toBe("default");
});

it("retries exact repeat intent and environment context without a fresh declaration", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true }); vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("g");
  const env = (selected: string) => Events.current.onmessage?.({ data: JSON.stringify({ event: "environments", managed: true, revisions: {}, clients: { [engine.client]: { selected, revisions: {} } } }) });
  env("dev"); await engine.rerun("repeat", "source", "original", true);
  env("prod"); await engine.rerun("repeat", "source", "original", true);
  expect(fetch.mock.calls[0]![1].body).toBe(fetch.mock.calls[1]![1].body);
  expect(JSON.parse(fetch.mock.calls[0]![1].body)).toMatchObject({ repeat: "original", acknowledge_effects: true, environments: { selected: "dev" } });
  await expect(engine.rerun("repeat", "changed", "original", true)).rejects.toThrow("different repeat intent");
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("reports transport lifecycle separately and only rebases a draft after explicit review", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true }); vi.stubGlobal("window", { fetch });
  const engine = new Engine(); const states: string[] = [];
  engine.listen(() => {}, () => {}, state => states.push(state));
  expect(states).toEqual(["connecting"]);
  Events.current.say("one");
  const env = (name: string) => Events.current.onmessage?.({ data: JSON.stringify({ event: "environments", managed: true, enabled: {}, credentials: {}, revisions: { dev: "r1", prod: "r2" }, providers: {}, clients: { [engine.client]: { selected: name, revisions: { dev: "r1", prod: "r2" } } } }) });
  env("dev"); engine.compose("api call"); env("prod");
  expect(engine.compositionInfo()?.context?.selected).toBe("dev");
  Events.current.onerror?.();
  expect(states).toEqual(["connecting", "connected", "reconnecting"]);
  expect(() => engine.rebaseComposition()).toThrow("Wait");
  Events.current.say("two");
  expect(engine.compositionInfo()?.sessionChanged).toBe(true);
  expect(() => engine.rebaseComposition()).toThrow("Wait");
  env("prod"); engine.rebaseComposition();
  expect(engine.compositionInfo()?.context?.selected).toBe("prod");
  expect(engine.compositionInfo()?.sessionChanged).toBe(false);
  await engine.submitComposed("reviewed", "api call");
  expect(JSON.parse(fetch.mock.calls[0]![1].body).environments.selected).toBe("prod");
  engine.compose("next"); engine.compose("");
  expect(engine.compositionInfo()).toBeUndefined();
});

it("freezes composed and retried environment acknowledgements while filtering pane vocabulary", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true }); vi.stubGlobal("window", { fetch });
  const engine = new Engine(); const seen: unknown[] = []; engine.listen(e => seen.push(e), () => {}); Events.current.say("one");
  const env = (revision: string) => Events.current.onmessage?.({ data: JSON.stringify({ event: "environments", managed: true, enabled: { dev: true }, credentials: {}, revisions: { dev: revision }, providers: { dev: [] }, clients: { [engine.client]: { selected: "dev", revisions: { dev: revision } } } }) });
  env("old"); engine.compose("api call"); env("new");
  await engine.submitComposed("attempt", "api call");
  expect(JSON.parse(fetch.mock.calls[0]![1].body).environments.revisions.dev).toBe("old");
  await engine.submit("attempt", "api call");
  expect(fetch.mock.calls[1]![1].body).toBe(fetch.mock.calls[0]![1].body);
  expect(JSON.parse(fetch.mock.calls[0]![1].body).client).toBe(engine.client);
  engine.compose("next"); Events.current.say("two");
  await expect(engine.submitComposed("next", "api call")).rejects.toThrow("Session changed");
});

it("never restores legacy fallback after the last managed definition disappears", async () => {
  vi.stubGlobal("EventSource", Events); const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("one");
  Events.current.onmessage?.({ data: JSON.stringify({ event: "environments", managed: true, enabled: {}, credentials: {}, revisions: {}, providers: {}, clients: {} }) });
  expect(engine.environmentContext()).toEqual({ selected: null, revisions: {} });
  await expect(engine.supply("token", "qa-value")).rejects.toThrow("scoped environment");
});

it("a panel's ordinary submission does not consume the console draft's captured environment", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true }); vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("g");
  const select = (selected: string) => Events.current.onmessage?.({ data: JSON.stringify({ event: "environments", managed: true, enabled: {}, credentials: {}, revisions: { dev: "d", prod: "p" }, providers: {}, clients: { [engine.client]: { selected, revisions: { dev: "d", prod: "p" } } } }) });
  select("dev"); engine.compose("existing draft"); select("prod");
  await engine.submit("panel-attempt", "@env{pg-demo} pg run");
  expect(engine.compositionInfo()?.context?.selected).toBe("dev");
  await engine.submitComposed("draft-attempt", "existing draft");
  expect(JSON.parse(fetch.mock.calls[0]![1].body).environments.selected).toBe("prod");
  expect(JSON.parse(fetch.mock.calls[1]![1].body).environments.selected).toBe("dev");
});

it("reads saved pages with the selected session and preserves exact cursors without submitting source", async () => {
  vi.stubGlobal("EventSource", Events);
  const page = { generation: "one", entries: [], next: "writer:9007199254740993:200", through: "9007199254740993", unconfirmedWrites: "0" };
  const fetch = vi.fn().mockResolvedValue({ ok: true, text: async () => JSON.stringify(page) });
  vi.stubGlobal("window", { fetch });
  const seen: unknown[] = [];
  const engine = new Engine(); engine.listen(event => seen.push(event), () => {});
  await expect(engine.history()).rejects.toThrow("connect");
  expect(fetch).not.toHaveBeenCalled();
  Events.current.say("one");
  expect(await engine.history(page.next)).toEqual(page);
  expect(fetch.mock.calls[0]![0]).toBe("/history?cursor=writer%3A9007199254740993%3A200");
  expect(fetch.mock.calls[0]![1].headers).toEqual({ "X-Wes-Session": "one" });
  expect(fetch.mock.calls[0]![1].body).toBeUndefined();
  expect(seen).toEqual([{ event: "session", generation: "one" }]);
  fetch.mockResolvedValueOnce({ ok: false, status: 429, text: async () => "reader busy" });
  await expect(engine.history()).rejects.toThrow("429");
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("rejects saved pages completed after workspace replacement or with a foreign generation", async () => {
  vi.stubGlobal("EventSource", Events);
  let finish!: (value: unknown) => void;
  const fetch = vi.fn().mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("one");
  const waiting = engine.history();
  Events.current.say("two");
  finish({ ok: true, text: async () => JSON.stringify({ generation: "one", entries: [] }) });
  await expect(waiting).rejects.toThrow("session changed");
  fetch.mockResolvedValueOnce({ ok: true, text: async () => JSON.stringify({ generation: "one", entries: [] }) });
  await expect(engine.history()).rejects.toThrow("session changed");
});

it("aborts a saved page request when its reader closes, without automatically retrying", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockImplementation((_url, options) => new Promise((_resolve, reject) => {
    options.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
  }));
  vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("one");
  const controller = new AbortController();
  const waiting = engine.history(undefined, controller.signal);
  controller.abort();
  await expect(waiting).rejects.toThrow("aborted");
  expect(fetch.mock.calls[0]![1].signal.aborted).toBe(true);
  expect(fetch).toHaveBeenCalledTimes(1);
});

it("keeps a retry bound to its original session after reconnect or workspace change", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true });
  vi.stubGlobal("window", { fetch });
  const engine = new Engine();
  engine.listen(() => {}, () => {});
  await expect(engine.submit("not-sent", ":help")).rejects.toThrow("connect");
  expect(fetch).not.toHaveBeenCalled();
  Events.current.say("first");
  await engine.submit("attempt", ":help");
  Events.current.onerror?.();
  await expect(engine.submit("attempt", ":help")).rejects.toThrow("connect");
  Events.current.say("second");
  await engine.submit("attempt", ":help");
  await engine.submit("fresh", ":help");
  expect(fetch.mock.calls.map(call => call[1].headers["X-Wes-Session"])).toEqual(["first", "first", "second"]);
});

it("does not automatically resubmit after an uncertain network failure", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockRejectedValue(new Error("timeout"));
  vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("one");
  await expect(engine.submit("attempt", "effect run")).rejects.toThrow("timeout");
  Events.current.say("one");
  expect(fetch).toHaveBeenCalledTimes(1);
});

it("sends answers and EOF only to the chosen run without turning them into source or retrying", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true });
  vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("session");
  await engine.answer("node", "run", "private answer\n");
  await engine.eof("node", "run");
  expect(fetch.mock.calls.map(call => JSON.parse(call[1].body))).toEqual([
    { request: "input", node: "node", run: "run", text: "private answer\n" },
    { request: "eof", node: "node", run: "run" },
  ]);
  fetch.mockRejectedValueOnce(new Error("uncertain delivery"));
  await expect(engine.answer("node", "run", "private answer\n")).rejects.toThrow("uncertain delivery");
  Events.current.say("session");
  expect(fetch).toHaveBeenCalledTimes(3);
});

it("bounds answer UTF-8 bytes before sending and does not include the answer in refusal text", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true }); vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("session");
  await expect(engine.answer("node", "run", "🎉".repeat(16385))).rejects.toThrow("maximum 64 KiB");
  expect(fetch).not.toHaveBeenCalled();
});

it("keeps a slow documentation submission waiting without resubmitting", async () => {
  vi.useFakeTimers();
  vi.stubGlobal("EventSource", Events);
  let finish!: (response: { ok: boolean }) => void;
  const fetch = vi.fn().mockImplementation((_url, options) => new Promise((resolve, reject) => {
    finish = resolve;
    options.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
  }));
  vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("session");
  let settled = false;
  const waiting = engine.submit("import-attempt", ':import spec url:"http://example.invalid/docs/overview"')
    .then(() => { settled = true; });
  await vi.advanceTimersByTimeAsync(15_001);
  expect(settled).toBe(false);
  expect(fetch.mock.calls[0]![1].signal.aborted).toBe(false);
  await vi.advanceTimersByTimeAsync(234_999);
  finish({ ok: true });
  await waiting;
  expect(settled).toBe(true);
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(vi.getTimerCount()).toBe(0);
});

it("expires a missing submission reply and retains the exact attempt on explicit retry", async () => {
  vi.useFakeTimers();
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockImplementation((_url, options) => new Promise((_resolve, reject) => {
    options.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
  }));
  vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("original");
  const text = ':import spec url:"http://example.invalid/docs/overview"';
  const failure = engine.submit("attempt", text).catch(error => error);
  await vi.advanceTimersByTimeAsync(300_000);
  expect((await failure).message).toBe("aborted");
  expect(fetch).toHaveBeenCalledTimes(1);
  fetch.mockResolvedValueOnce({ ok: true });
  Events.current.say("replacement");
  await engine.submit("attempt", text);
  expect(fetch.mock.calls[1]![1].body).toBe(fetch.mock.calls[0]![1].body);
  expect(fetch.mock.calls[1]![1].headers["X-Wes-Session"]).toBe("original");
  expect(vi.getTimerCount()).toBe(0);
});

it("preserves shorter control waits and longer configured submission waits", async () => {
  vi.useFakeTimers();
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockImplementation((_url, options) => new Promise((_resolve, reject) => {
    options.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
  }));
  vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("session");
  engine.timeout = 1_000;
  const control = engine.cancel("node").catch(error => error);
  await vi.advanceTimersByTimeAsync(1_000);
  expect((await control).message).toBe("aborted");
  engine.timeout = 360_000;
  const source = engine.submit("script", ':run file:"synthetic.wes"').catch(error => error);
  await vi.advanceTimersByTimeAsync(300_001);
  expect(fetch.mock.calls[1]![1].signal.aborted).toBe(false);
  await vi.advanceTimersByTimeAsync(59_999);
  expect((await source).message).toBe("aborted");
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(vi.getTimerCount()).toBe(0);
});

it("records a refused Run again once with original cell and workspace, without recording source", async () => {
  const { applicationLog } = await import("./application-log"); applicationLog.clear();
  vi.stubGlobal("EventSource", Events);
  const reason = "Run again requires a successfully declared command or pipeline with unchanged definition evidence.";
  const fetch = vi.fn().mockResolvedValue({ ok: false, status: 400, text: async () => reason });
  vi.stubGlobal("fetch", fetch); vi.stubGlobal("window", { fetch });
  const engine = new Engine("agent-lab"); engine.listen(() => {}, () => {}); Events.current.say("live");
  await expect(engine.rerun("attempt", "sensitive source body", "original")).rejects.toThrow("Engine refused Run again (HTTP 400)");
  expect(applicationLog.snapshot()).toHaveLength(1);
  expect(applicationLog.snapshot()[0]).toMatchObject({ code: "ENGINE_HTTP_400", source: "Engine", operation: "Run again", workspace: "agent-lab", generation: "live", cell: "original", detail: reason });
  expect(applicationLog.snapshot()[0]?.next).toContain("unchanged definition");
  expect(JSON.stringify(applicationLog.snapshot())).not.toContain("sensitive source body");
  expect(fetch).toHaveBeenCalledOnce(); applicationLog.clear();
});

it("marks a refused submission as not started only from the engine's outcome header on a submit request", async () => {
  vi.stubGlobal("EventSource", Events);
  const refused = (headers?: Record<string, string>) => ({ ok: false, status: 400, headers: headers && new Headers(headers), text: async () => "SBX004: invalid member" });
  const fetch = vi.fn()
    .mockResolvedValueOnce(refused({ "x-wes-submission-outcome": "not-started" }))
    .mockResolvedValueOnce(refused())
    .mockResolvedValueOnce(refused({ "X-Wes-Submission-Outcome": "started" }))
    .mockResolvedValueOnce(refused({ "X-Wes-Submission-Outcome": "not-started" }));
  vi.stubGlobal("fetch", fetch); vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("g");
  await expect(engine.submit("a1", "sandbox s { }")).rejects.toMatchObject({ status: 400, detail: "SBX004: invalid member", submissionOutcome: "not-started" });
  await expect(engine.submit("a2", "sandbox s { }")).rejects.toMatchObject({ submissionOutcome: undefined });
  await expect(engine.submit("a3", "sandbox s { }")).rejects.toMatchObject({ submissionOutcome: undefined });
  await expect(engine.cancel("id1")).rejects.toMatchObject({ submissionOutcome: undefined });
  expect(fetch).toHaveBeenCalledTimes(4);
});

it("records uncertain delivery and timeout distinctly, but never retries or logs successful requests", async () => {
  const { applicationLog } = await import("./application-log"); applicationLog.clear();
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockRejectedValueOnce(new TypeError("Failed to fetch")).mockResolvedValueOnce({ ok: true });
  vi.stubGlobal("fetch", fetch); vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("g");
  await expect(engine.cancel("id1")).rejects.toThrow();
  expect(applicationLog.snapshot()[0]).toMatchObject({ code: "ENGINE_REQUEST_FAILED", node: "id1", source: "Browser → engine" });
  expect(applicationLog.snapshot()[0]?.next).toContain("does not prove");
  await engine.cancel("id2"); expect(applicationLog.snapshot()).toHaveLength(1);
  vi.useFakeTimers(); engine.timeout = 20;
  fetch.mockImplementationOnce((_url, init) => new Promise((_resolve, reject) => init.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")))));
  const pending = engine.cancel("id3").catch(error => error);
  await vi.advanceTimersByTimeAsync(25); await pending;
  expect(applicationLog.snapshot().at(-1)).toMatchObject({ code: "ENGINE_TIMEOUT", node: "id3" });
  expect(fetch).toHaveBeenCalledTimes(3); applicationLog.clear();
});

it("records storage refusal while preserving blockers and withholds secret-operation details", async () => {
  const { applicationLog, formatLogs } = await import("./application-log"); applicationLog.clear();
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValueOnce({ ok: false, text: async () => '{"message":"Dependent work exists.","blockers":[]}' })
    .mockResolvedValueOnce({ ok: false, status: 400, text: async () => "DO_NOT_LOG_SECRET" });
  vi.stubGlobal("fetch", fetch); vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("g");
  await expect(engine.previewDeleteWork("original")).rejects.toBeInstanceOf(StorageError);
  await expect(engine.supply("credential", "DO_NOT_LOG_SECRET")).rejects.toThrow("HTTP 400");
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(applicationLog.snapshot()[0]).toMatchObject({ code: "ENGINE_STORAGE_REFUSED", cell: "original", detail: "Dependent work exists." });
  expect(formatLogs(applicationLog.snapshot())).toContain("Cell: original");
  expect(JSON.stringify(applicationLog.snapshot())).not.toContain("DO_NOT_LOG_SECRET"); applicationLog.clear();
});

it("logs structured execution issues with the bound workspace once per failure identity", async () => {
  const { applicationLog } = await import("./application-log"); applicationLog.clear();
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
  const engine = new Engine("synthetic-api"); const received = vi.fn();
  const close = engine.listen(received, () => {}); Events.current.say("diagnostic-generation");
  const event = { event: "failed", node: "id1001", reason: "HTTP argument failed validation", error: {
    id: "http-validation-1", code: "HTTP001", message: "HTTP argument failed validation", causeId: "", issues: [
      { path: "/arguments/body/count", code: "TYP005", message: "number is outside Count's bounds" },
      { path: "/arguments/feed", code: "TYP005", message: "value is not in Feed's enum" },
    ],
  } };
  const send = () => Events.current.onmessage?.({ data: JSON.stringify(event) });
  send(); send(); close();
  engine.listen(received, () => {}); Events.current.say("diagnostic-generation"); send();
  expect(applicationLog.snapshot()).toHaveLength(1);
  expect(applicationLog.snapshot()[0]).toMatchObject({ workspace: "synthetic-api", node: "id1001", source: "Execution", code: "HTTP001", count: 1 });
  expect(applicationLog.snapshot()[0]?.detail).toContain("/arguments/body/count · TYP005: number is outside Count's bounds");
  event.error.id = "http-validation-2"; send();
  expect(applicationLog.snapshot().reduce((n, row) => n + row.count, 0)).toBe(2);
  expect(fetch).not.toHaveBeenCalled(); applicationLog.clear();
});

it("preserves uncertain run protection across rereads and refuses duplicate mutations until confirmation", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockRejectedValue(new Error("connection lost")); vi.stubGlobal("fetch", fetch);
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("history-session");
  await expect(engine.protectRun("cell", "old-run")).rejects.toThrow("completion is unknown");
  expect(engine.runProtection("old-run")).toBe("uncertain");
  await expect(engine.protectRun("cell", "old-run")).rejects.toThrow("unconfirmed");
  expect(fetch).toHaveBeenCalledTimes(1);
  fetch.mockResolvedValue({ ok: true, text: async () => JSON.stringify({ run: { protected: false } }) });
  await engine.workRun("cell", "old-run");
  expect(engine.runProtection("old-run")).toBe("uncertain");
  fetch.mockResolvedValue({ ok: true, text: async () => JSON.stringify({ run: { protected: true } }) });
  await engine.workRun("cell", "old-run");
  expect(engine.runProtection("old-run")).toBeUndefined();
});

it("distinguishes authoritative protection refusal from possibly applied protection", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: false, text: async () => JSON.stringify({ code: "STO012", message: "Evidence unavailable", mayHaveApplied: false }) });
  vi.stubGlobal("fetch", fetch);
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("history-session");
  await expect(engine.protectRun("cell", "old-run")).rejects.toMatchObject({ code: "STO012", mayHaveApplied: false });
  expect(engine.runProtection("old-run")).toBeUndefined();
  fetch.mockResolvedValue({ ok: false, text: async () => JSON.stringify({ code: "STO013", message: "Not confirmed", mayHaveApplied: true }) });
  await expect(engine.protectRun("cell", "old-run")).rejects.toMatchObject({ code: "STO013", mayHaveApplied: true });
  expect(engine.runProtection("old-run")).toBe("uncertain");
  Events.current.say("another-session");
  expect(engine.runProtection("old-run")).toBeUndefined();
});

it("submits workspace plans through the normal cell request path", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true });
  vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {}); Events.current.say("g");
  await engine.submitComposed("plan-cell", ':workspace plan delete workspace:"demo"');
  expect(fetch).toHaveBeenCalledTimes(1);
  const request = JSON.parse(fetch.mock.calls[0]![1].body);
  expect(request.cell).toBe("plan-cell");
  expect(request.text).toBe(':workspace plan delete workspace:"demo"');
});

it("carries ordered view events in the same CAS request and preserves the owner's rejection reason",async()=>{
  vi.stubGlobal("EventSource",Events);
  const state={owner:"id1",identity:"instance",definitionRevision:"0",revision:"1",definition:"timeline",digest:"digest",fields:{},outputs:{}};
  const fetch=vi.fn().mockResolvedValue({ok:true,status:200,text:async()=>JSON.stringify(state)});vi.stubGlobal("fetch",fetch);
  const engine=new Engine();const close=engine.listen(()=>{},()=>{});Events.current.say("g");
  const events=[{port:"picked",value:7},{port:"picked",value:7}];
  await engine.commitViewState("id1","instance","g",{...state,events},new AbortController().signal);
  expect(JSON.parse(fetch.mock.calls[0]![1].body)).toEqual({owner:"id1",identity:"instance",definitionRevision:"0",revision:"1",fields:{},outputs:{},events});
  fetch.mockResolvedValueOnce({ok:false,status:409,text:async()=>JSON.stringify({...state,problem:"View event capacity exceeded"})});
  const result=await engine.commitViewState("id1","instance","g",{...state,events},new AbortController().signal);
  expect(result).toMatchObject({conflict:true,problem:"View event capacity exceeded"});expect(fetch).toHaveBeenCalledTimes(2);close();
});
it("retries only an explicit pre-mutation admission refusal for shared view writes, never an unknown outcome",async()=>{
 vi.useFakeTimers();vi.stubGlobal("EventSource",Events);
 const engine=new Engine(),close=engine.listen(()=>{},()=>{});Events.current.say("g");
 const edit={owner:"node",identity:"instance",definitionRevision:"0",revision:"1",fields:{},outputs:{}};
 const fetch=vi.fn().mockResolvedValueOnce(new Response(null,{status:503})).mockResolvedValueOnce(new Response(JSON.stringify({...edit,definition:"timeline",digest:"digest"}),{status:200}));vi.stubGlobal("fetch",fetch);
 const pending=engine.commitViewState("node","instance","g",edit,new AbortController().signal);
 await vi.advanceTimersByTimeAsync(50);expect((await pending).conflict).toBe(false);expect(fetch).toHaveBeenCalledTimes(2);
 fetch.mockReset().mockRejectedValue(new Error("connection lost"));
 await expect(engine.commitViewState("node","instance","g",edit,new AbortController().signal)).rejects.toThrow("connection lost");
 await vi.advanceTimersByTimeAsync(500);expect(fetch).toHaveBeenCalledOnce();close();
});

it("should_SubmitOneGuardedPinUnderAnUnusedName_When_AViewInputIsPinned", async () => {
  // Arrange
  vi.stubGlobal("EventSource", Events);
  const engine = new Engine(), disconnect = engine.listen(() => {}, () => {}); Events.current.say("g");
  const created = (node: string, name: string, errorNames: string[] = []) => Events.current.onmessage?.({ data: JSON.stringify({ event: "created", node, name, dependsOn: [], command: ":calc", interactive: false, errorNames }) });
  created("chart", "chart"); created("id2", "chart_pin"); created("chart_pin2", ""); created("id4", "other", ["chart_pin3"]);
  const submit = vi.spyOn(engine, "submit").mockResolvedValue();
  // Act
  const name = await engine.pinViewInput({ id: "chart", instance: "chart-identity", revision: "2", inputRevision: "5" }, "g");
  // Assert
  expect(name).toBe("chart_pin4");
  expect(submit).toHaveBeenCalledTimes(1);
  expect(submit.mock.calls[0]![1]).toBe(':view pin $chart instance:"chart-identity" revision:2 inputRevision:5 > chart_pin4');
  disconnect();
});

it("should_NotResubmitAPin_When_TheSubmissionFails", async () => {
  // Arrange
  vi.stubGlobal("EventSource", Events);
  const engine = new Engine(), disconnect = engine.listen(() => {}, () => {}); Events.current.say("g");
  const submit = vi.spyOn(engine, "submit").mockRejectedValue(new Error("Connection lost"));
  // Act
  const pinning = engine.pinViewInput({ id: "chart", instance: "chart-identity", revision: "2", inputRevision: "5" }, "g");
  // Assert
  await expect(pinning).rejects.toThrow("Connection lost");
  expect(submit).toHaveBeenCalledTimes(1);
  await expect(engine.pinViewInput({ id: "chart", instance: "chart-identity", revision: "2", inputRevision: "5" }, "stale-generation")).rejects.toThrow(/reopen it/);
  expect(submit).toHaveBeenCalledTimes(1);
  disconnect();
});

it("should_FreeADroppedNodesNames_When_ChoosingTheNextPinName", async () => {
  // Arrange
  vi.stubGlobal("EventSource", Events);
  const engine = new Engine(), disconnect = engine.listen(() => {}, () => {}); Events.current.say("g");
  const send = (event: object) => Events.current.onmessage?.({ data: JSON.stringify(event) });
  send({ event: "created", node: "id2", name: "chart_pin", dependsOn: [], command: ":calc", interactive: false });
  send({ event: "created", node: "id3", name: "chart_pin2", dependsOn: [], command: ":calc", interactive: false });
  send({ event: "dropped", nodes: ["id2"] });
  vi.spyOn(engine, "submit").mockResolvedValue();
  // Act
  const name = await engine.pinViewInput({ id: "chart", instance: "chart-identity", revision: "2", inputRevision: "5" }, "g");
  // Assert
  expect(name).toBe("chart_pin");
  disconnect();
});

it("should_RefuseStaleFeedbackWithoutResubmitting_When_TheWorkspaceChangesDuringAPin", async () => {
  // Arrange
  vi.stubGlobal("EventSource", Events);
  const engine = new Engine(), disconnect = engine.listen(() => {}, () => {}); Events.current.say("g");
  const submit = vi.spyOn(engine, "submit").mockImplementation(async () => { Events.current.say("g2"); });
  // Act
  const pinning = engine.pinViewInput({ id: "chart", instance: "chart-identity", revision: "2", inputRevision: "5" }, "g");
  // Assert
  await expect(pinning).rejects.toThrow(/Workspace changed while the Pin was submitted.*not repeated/);
  expect(submit).toHaveBeenCalledTimes(1);
  disconnect();
});
