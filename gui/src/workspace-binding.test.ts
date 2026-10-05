vi.mock("./workspace-events", () => ({ WorkspaceEvents: function(path: string) { return new EventSource(path); } }));
import { afterEach, expect, it, vi } from "vitest";
import { Engine } from "./engine";
import { openWorkspace, restoreWorkspace, workspaceHeaders, workspaceName } from "./workspace-binding";
import { openRoute, readOpenRoute } from "./surface/open-route";
import { read } from "./surface/commands";
import { applyPaneCommand } from "./surface/pane-command";
import { clearPane, close, focus, oneP, paneFor, sendToPane, SESSION_PANE } from "./surface/split-model";
import { restoreSplit } from "./surface/split-storage";
import { promptCompletion } from "./surface/prompt-complete";
import { emptyCatalogue } from "./vocabulary";

afterEach(() => vi.unstubAllGlobals());
it.each(["", " ", "../other", "a/b", "a\\b", "x\n", "x\u007f", "ç".repeat(49)])("rejects invalid workspace names %j", name => {
  expect(workspaceName(name)).toBe(false);
  expect(() => new Engine(name)).toThrow("Invalid workspace");
});
it.each(["l", "r", "t", "b"])("distinguishes workspace, value, shell and explicit screen in /%ssplit", direction => {
  for (const name of ["demo", "env", "graph", '"two words"']) {
    expect(read(`/${direction}splitx ${name}`)).toMatchObject({ kind: "directional-split", takeFocus: true, content: { workspace: name.replaceAll('"', '') } });
  }
  expect(read(`/${direction}split $node`)).toMatchObject({ content: { value: "node" } });
  expect(read(`/${direction}split xterm`)).toMatchObject({ content: { terminal: true } });
  expect(read(`/${direction}split /env`)).toMatchObject({ content: { screen: "env" } });
});
it("supports split aliases, quoted unicode names and bottom split discovery", () => {
  expect(read('/split down "çalışma + 100%"')).toEqual(read('/bsplit "çalışma + 100%"'));
  expect(read('/split right demo')).toEqual(read('/rsplit demo'));
  expect(promptCompletion({ line: "/bs", caret: 3, catalogue: emptyCatalogue, names: [], aliases: {} }).items.map(i => i.text)).toEqual(["/bsplit", "/bsplitx"]);
});
it("scopes remembered screens and preserves ownership through restoration and dismissal", () => {
  let state = applyPaneCommand(oneP(SESSION_PANE), "/rsplit second", "p1", s => s);
  state = sendToPane(focus(state, "p1"), { screen: "env" });
  state = sendToPane(focus(state, "p2"), { screen: "env" });
  expect(paneFor(state, "env")).toBe("p3");
  expect(paneFor(state, "env", "second")).toBe("p4");
  state = sendToPane(focus(state, "p2"), { screen: "env" });
  expect(state.panes).toHaveLength(4);
  expect(state.panes.find(p => p.id === "p4")?.workspace).toBe("second");
  expect(restoreSplit(JSON.parse(JSON.stringify(state)))).toEqual(state);
  expect(clearPane(state, "p4").panes.find(p => p.id === "p4")?.workspace).toBe("second");
  const remaining = close(close(close(state, "p1"), "p3"), "p4");
  expect(close(remaining, "p2")).toBe(remaining);
});
it("round trips workspace identity in result routes and confirms explicit opening", async () => {
  const name = "çalışma + 100%";
  expect(readOpenRoute(openRoute("same/node", "json", name))).toEqual({ node: "same/node", tab: "json", workspace: name });
  const fetch = vi.fn().mockResolvedValue(Response.json({ workspace: name, generation: "g", identity: "identity-one" }));
  vi.stubGlobal("fetch", fetch);
  await openWorkspace(name, false);
  expect(JSON.parse(fetch.mock.calls[0]![1].body)).toEqual({ name, create: false });
  fetch.mockResolvedValueOnce(Response.json({ workspace: "neighbor", generation: "g" }));
  await expect(openWorkspace(name)).rejects.toThrow("requested workspace");
});

class Events {
  static all = new Map<string, Events>();
  onmessage?: (message: { data: string }) => void;
  constructor(readonly url: string) { Events.all.set(url, this); }
  close() {}
  emit(event: object) { this.onmessage?.({ data: JSON.stringify(event) }); }
}
it("routes commands, results, traces, history and terminals to their owner with independent context", async () => {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn(async (url: string, options?: RequestInit) => {
    const name = decodeURIComponent(new Headers(options?.headers).get("X-Wes-Workspace")!);
    if (url.startsWith("/history")) return Response.json({ generation: `g-${encodeURIComponent(name)}`, cells: [], more: false });
    if (url === "/submit") return new Response("", { status: 202 });
    return Response.json({ owner: name });
  });
  vi.stubGlobal("fetch", fetch); vi.stubGlobal("window", { fetch });
  const engines = [new Engine("first", "shared-client"), new Engine("çalışma", "shared-client")];
  for (const engine of engines) {
    engine.listen(() => {}, () => {});
    const stream = Events.all.get(`/events?${new URLSearchParams({ workspace: engine.binding! })}`)!;
    stream.emit({ event: "session", generation: `g-${encodeURIComponent(engine.binding!)}` });
    stream.emit({ event: "environments", managed: true, default: engine.binding, enabled: {}, credentials: {}, revisions: {}, providers: {}, clients: {} });
    await engine.submitComposed("same-cell", ":calc 1");
    await engine.fetch("same-result");
    await engine.trace("same-node");
    await engine.history();
    await engine.terminal({ action: "forget", history: "synthetic-history" });
    for (const [, options] of fetch.mock.calls.slice(-5)) {
      expect(options?.headers).toMatchObject({ ...workspaceHeaders(engine.binding), "X-Wes-Session": `g-${encodeURIComponent(engine.binding!)}` });
    }
    expect(JSON.parse(String(fetch.mock.calls.at(-5)![1]?.body)).environments.selected).toBe(engine.binding);
  }
  expect(engines[0]!.environmentContext()?.selected).toBe("first");
  expect(engines[1]!.environmentContext()?.selected).toBe("çalışma");
});

it("restoring a saved tab uses its layout identity; explicit reopening can select a replacement", async () => {
  const fetch = vi.fn().mockResolvedValue(Response.json({workspace:"identity-test",generation:"g1",identity:"one"}));
  vi.stubGlobal("fetch",fetch);
  await expect(restoreWorkspace("unconfirmed-tab")).rejects.toThrow("Reopen");
  expect(fetch).not.toHaveBeenCalled();
  const first = await openWorkspace("identity-test");
  expect(first).toBe("one");
  fetch.mockResolvedValueOnce(new Response("Workspace identity changed",{status:409}));
  await expect(restoreWorkspace("identity-test", first)).rejects.toThrow("identity changed");
  expect(JSON.parse(fetch.mock.calls[1]![1].body)).toEqual({name:"identity-test",create:false,identity:"one"});
  fetch.mockResolvedValueOnce(Response.json({workspace:"identity-test",generation:"g2",identity:"two"}));
  const replacement = await openWorkspace("identity-test");
  fetch.mockResolvedValueOnce(Response.json({workspace:"identity-test",generation:"g2",identity:"two"}));
  await restoreWorkspace("identity-test", replacement);
  expect(JSON.parse(fetch.mock.calls[3]![1].body).identity).toBe("two");
});

it.each([undefined, "", "bad\nidentity", "x".repeat(129)])("refuses an open without a valid identity: %j", async identity => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({workspace:"synthetic",generation:"g",identity})));
  await expect(openWorkspace("synthetic")).rejects.toThrow("stable workspace identity");
});
it("rejects an unexpected identity even in a successful restore response", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({workspace:"synthetic",generation:"g",identity:"replacement"})));
  await expect(restoreWorkspace("synthetic", "original")).rejects.toThrow("identity changed");
});

it('discovers workspace deletion as a preview command',()=>{
 expect(promptCompletion({line:'/workspace d',caret:12,catalogue:emptyCatalogue,names:[],aliases:{}}).items).toMatchObject([{text:'delete',detail:'open deletion preview'}]);
});

it("restores only valid identities referenced by active or hidden panes and removes retired bindings", async () => {
  const { bindWorkspace, openWorkspaceTab, removeWorkspaceViews } = await import("./surface/workspace-tabs");
  const base = bindWorkspace(openWorkspaceTab(oneP(SESSION_PANE), "p1", "hidden"), "hidden", "stable-hidden");
  const saved = {...base, workspaceBindings: {...base.workspaceBindings, unused:"unused-id", malformed:7}};
  expect(restoreSplit(saved).workspaceBindings).toEqual({hidden:"stable-hidden"});
  expect(removeWorkspaceViews(base, "hidden").workspaceBindings).toEqual({});
  const invalid = restoreSplit({...base,workspaceBindings:{hidden:"bad\nidentity"}});
  const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
  await expect(restoreWorkspace("hidden", invalid.workspaceBindings?.hidden)).rejects.toThrow("Reopen");
  expect(fetch).not.toHaveBeenCalled();
});
