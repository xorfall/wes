import { terminalPane } from "./surface/split-model";
import { afterEach, expect, it, vi } from "vitest";

afterEach(() => { vi.unstubAllGlobals(); vi.resetModules(); });
async function setup(desktop = true) {
  vi.resetModules();
  const data = new Map<string,string>();
  vi.stubGlobal("window", { __WES_DESKTOP__: desktop, localStorage: {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => data.set(key, value),
  }});
  const settings = await import("./settings");
  const persistence = await import("./desktop-preferences");
  return {settings, persistence, data};
}
it("restores from desktop storage before the first save despite a new origin's empty localStorage", async () => {
  const {settings, persistence} = await setup();
  const stored = {...settings.defaults, surfacePalette:"white", surfaceChrome:"controls", surfaceTailKeys:"hidden", direction:"TB", stayAtNewest:false, previewChars:8000, requestTimeoutMs:30000};
  const fetch = vi.fn().mockResolvedValue({ok:true,json:async()=>({settings:stored})}); vi.stubGlobal("fetch",fetch);
  await persistence.initializeDesktopPreferences(settings.load());
  expect(settings.load()).toEqual(stored);
  settings.save(settings.load());
  await Promise.resolve();
  expect(fetch).toHaveBeenCalledTimes(1); // Initial Surface effect cannot write defaults over the file.
});
it("keeps browser preferences local and never contacts desktop storage", async () => {
  const {settings, persistence} = await setup(false);
  const fetch = vi.fn(); vi.stubGlobal("fetch",fetch);
  await persistence.initializeDesktopPreferences(settings.load());
  settings.save({...settings.defaults,surfacePalette:"white"});
  expect(settings.load().surfacePalette).toBe("white");
  expect(fetch).not.toHaveBeenCalled();
});
it("serializes and coalesces rapid changes so an earlier save cannot overwrite the latest one", async () => {
  const {settings, persistence} = await setup();
  let release!: (value: {ok:boolean}) => void;
  const held = new Promise<{ok:boolean}>(resolve => {release=resolve;});
  const fetch = vi.fn().mockResolvedValueOnce({ok:true,json:async()=>({settings:settings.defaults})})
    .mockReturnValueOnce(held).mockResolvedValue({ok:true}); vi.stubGlobal("fetch",fetch);
  await persistence.initializeDesktopPreferences(settings.load());
  const work = persistence.saveDesktopPreferences({...settings.defaults,surfacePalette:"white"});
  await Promise.resolve();
  void persistence.saveDesktopPreferences({...settings.defaults,surfacePalette:"system"});
  void persistence.saveDesktopPreferences({...settings.defaults,surfacePalette:"ink",surfaceChrome:"controls"});
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(persistence.persistenceState().saving).toBe(true);
  release({ok:true}); await work;
  expect(fetch).toHaveBeenCalledTimes(3);
  expect(JSON.parse(fetch.mock.calls[2]![1].body)).toMatchObject({surfacePalette:"ink",surfaceChrome:"controls"});
  expect(fetch.mock.calls[2]![1].keepalive).toBe(true);
  expect(persistence.persistenceState()).toEqual({saving:false});
  expect(settings.load().surfaceChrome).toBe("controls");
});
it("never overwrites unreadable saved settings with fallback defaults", async () => {
  const {settings, persistence} = await setup();
  const fetch = vi.fn().mockResolvedValue({ok:false}); vi.stubGlobal("fetch",fetch);
  await persistence.initializeDesktopPreferences(settings.load());
  settings.save(settings.load()); await Promise.resolve();
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(persistence.persistenceState().error).toContain("not been overwritten");
});
it('saves large layout definitions without exceeding the browser keepalive body budget',async()=>{
  const {persistence}=await setup();
  const fetch=vi.fn().mockResolvedValueOnce({ok:true,json:async()=>({settings:null})}).mockResolvedValue({ok:true});vi.stubGlobal('fetch',fetch);
  await persistence.initializeDesktopPreferences({});
  const settings={definitions:'界'.repeat(30_000)};
  await persistence.saveDesktopPreferences(settings);
  expect(fetch.mock.calls[1]![1].keepalive).toBe(false);
  expect(JSON.parse(fetch.mock.calls[1]![1].body)).toEqual(settings);
  expect(persistence.persistenceState()).toEqual({saving:false});
});
it("surfaces failed writes and retries only on a later explicit change", async () => {
  const {settings, persistence} = await setup();
  const fetch = vi.fn().mockResolvedValueOnce({ok:true,json:async()=>({settings:null})})
    .mockResolvedValueOnce({ok:false}).mockResolvedValue({ok:true}); vi.stubGlobal("fetch",fetch);
  await persistence.initializeDesktopPreferences(settings.load());
  await persistence.saveDesktopPreferences({...settings.defaults,surfacePalette:"white"});
  expect(persistence.persistenceState().error).toContain("could not be saved");
  expect(fetch).toHaveBeenCalledTimes(2);
  await persistence.saveDesktopPreferences({...settings.defaults,surfacePalette:"system"});
  expect(persistence.persistenceState()).toEqual({saving:false});
});
it("durably restores pane identities, nested layout and focus on a new desktop origin without losing other settings", async () => {
  const {settings, persistence} = await setup();
  const {oneP, SESSION_PANE, splitPane} = await import("./surface/split-model");
  const layout = splitPane(oneP(SESSION_PANE), "left", terminalPane("p2", { cwd:"/synthetic/project" }), true);
  const stored = {...settings.defaults, surfacePalette:"white", paneLayout:layout};
  const fetch = vi.fn().mockResolvedValue({ok:true,json:async()=>({settings:stored})}); vi.stubGlobal("fetch", fetch);
  await persistence.initializeDesktopPreferences(settings.defaults);
  expect(settings.load().paneLayout).toEqual(layout);
  settings.save({...settings.load(), surfaceChrome:"controls"});
  await Promise.resolve(); await Promise.resolve();
  expect(JSON.parse(fetch.mock.calls.at(-1)![1].body)).toMatchObject({paneLayout:layout,surfacePalette:"white",surfaceChrome:"controls"});
});

it("saves and restores personal aliases in desktop preferences independent of the HTTP origin", async () => {
  const {settings, persistence, data} = await setup();
  const stored = {...settings.defaults, surfacePalette: "white", aliases: {twice: ":calc { return 2 * (_); }"}};
  const fetch = vi.fn().mockResolvedValueOnce({ok:true,json:async()=>({settings:stored})})
    .mockResolvedValue({ok:true}); vi.stubGlobal("fetch", fetch);
  await persistence.initializeDesktopPreferences(settings.defaults);
  expect(data.size).toBe(0);
  expect(settings.load().aliases).toEqual(stored.aliases);
  settings.save({...settings.load(), aliases: {...stored.aliases, nodes: ":list nodes"}});
  await persistence.flushDesktopPreferences();
  const saved = JSON.parse(fetch.mock.calls.at(-1)![1].body);
  expect(saved).toMatchObject({surfacePalette:"white", aliases:{...stored.aliases, nodes:":list nodes"}});
  expect(fetch.mock.calls.at(-1)![0]).toBe("/client-preferences");
  expect(persistence.persistenceState()).toEqual({saving:false});
});

it("persists a newly created terminal tab identity before allowing its shell to start", async () => {
  const {settings, persistence} = await setup();
  const {oneP, SESSION_PANE, splitPane} = await import("./surface/split-model");
  const layout = splitPane(oneP(SESSION_PANE), "right", terminalPane("p2"));
  let release!: (response: {ok:boolean}) => void;
  const fetch = vi.fn().mockResolvedValueOnce({ok:true,json:async()=>({settings:settings.defaults})})
    .mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
  vi.stubGlobal("fetch", fetch);
  await persistence.initializeDesktopPreferences(settings.defaults);
  const created = {...settings.load(), paneLayout:layout};
  const history = created.paneLayout.panes[1]!.history;
  settings.save(created);
  const started = vi.fn();
  const preparing = persistence.flushDesktopPreferences().then(started);
  await Promise.resolve();
  expect(started).not.toHaveBeenCalled();
  expect(JSON.parse(fetch.mock.calls[1]![1].body).paneLayout.panes[1].history).toBe(history);
  release({ok:true}); await preparing;
  expect(started).toHaveBeenCalledOnce();
  expect(settings.load().paneLayout!.panes[1]!.history).toBe(history);
});
it("keeps history identities per selected home and starts an empty home without old-browser fallback", async () => {
  let firstHistory: string | undefined;
  for (const existing of [true, false]) {
    const {settings, persistence} = await setup();
    const {oneP, SESSION_PANE, splitPane} = await import("./surface/split-model");
    const layout = splitPane(oneP(SESSION_PANE), "right", terminalPane("p2"));
    firstHistory ??= layout.panes[1]!.history;
    window.localStorage.setItem("wes.settings", JSON.stringify({...settings.defaults,paneLayout:layout}));
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ok:true,json:async()=>({settings:existing ? {...settings.defaults,paneLayout:layout} : null})}));
    await persistence.initializeDesktopPreferences(settings.defaults);
    if (existing) expect(settings.load().paneLayout!.panes[1]!.history).toBe(firstHistory);
    else expect(settings.load().paneLayout).toBeUndefined();
  }
});

it("bounds durability waits without cancelling or overlapping writes, and recovers on late completion", async () => {
  vi.useFakeTimers();
  try {
    const {settings, persistence} = await setup();
    let finish!: (response: {ok:boolean}) => void;
    const fetch = vi.fn().mockResolvedValueOnce({ok:true,json:async()=>({settings:settings.defaults})})
      .mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }))
      .mockResolvedValue({ok:true});
    vi.stubGlobal("fetch", fetch);
    await persistence.initializeDesktopPreferences(settings.defaults);
    const saving = persistence.saveDesktopPreferences({...settings.defaults,surfacePalette:"white"});
    const flush = expect(persistence.flushDesktopPreferences()).rejects.toThrow("still pending");
    await vi.advanceTimersByTimeAsync(5000); await flush;
    expect(persistence.persistenceState()).toMatchObject({saving:true, error:expect.stringContaining("still pending")});
    void persistence.saveDesktopPreferences({...settings.defaults,surfacePalette:"ink"});
    expect(fetch).toHaveBeenCalledTimes(2); // The uncertain old request still owns the writer.
    finish({ok:true}); await saving;
    await persistence.flushDesktopPreferences();
    expect(fetch).toHaveBeenCalledTimes(3);
    expect(JSON.parse(fetch.mock.calls[2]![1].body).surfacePalette).toBe("ink");
    expect(persistence.persistenceState()).toEqual({saving:false});
  } finally { vi.useRealTimers(); }
});

it("does not notify or write repeatedly for an unchanged snapshot", async () => {
  const {settings, persistence} = await setup();
  const fetch = vi.fn().mockResolvedValue({ok:true,json:async()=>({settings:settings.defaults})});
  vi.stubGlobal("fetch", fetch);
  await persistence.initializeDesktopPreferences(settings.defaults);
  const listener = vi.fn();
  const off = persistence.subscribePersistence(listener);
  for (let i = 0; i < 20; i++) await persistence.saveDesktopPreferences(settings.defaults);
  expect(listener).not.toHaveBeenCalled(); expect(fetch).toHaveBeenCalledTimes(1);
  off();
});

it("restores named tabs and their identities together on a fresh desktop origin and module lifetime", async () => {
  const first = await setup();
  const { oneP, SESSION_PANE, splitPane } = await import("./surface/split-model");
  const { bindWorkspace, openWorkspaceTab } = await import("./surface/workspace-tabs");
  const { openWorkspace } = await import("./workspace-binding");
  let saved: unknown = null;
  const fetch = vi.fn(async (url: string, options?: RequestInit) => {
    if (url === "/workspaces") return Response.json({ workspace: "synthetic", generation: "g1", identity: "stable-one" });
    if (options?.method === "PUT") { saved = JSON.parse(String(options.body)); return new Response(null, {status:204}); }
    return Response.json({settings:saved});
  });
  vi.stubGlobal("fetch", fetch);
  await first.persistence.initializeDesktopPreferences(first.settings.defaults);
  const identity = await openWorkspace("synthetic");
  const layout = bindWorkspace(splitPane(openWorkspaceTab(oneP(SESSION_PANE), "p1", "synthetic"), "right",
    {id:"p2",title:"session",workspace:"synthetic"}), "synthetic", identity);
  first.settings.save({...first.settings.load(), paneLayout:layout});
  await first.persistence.flushDesktopPreferences();
  expect(saved).toMatchObject({paneLayout:{workspaceBindings:{synthetic:"stable-one"},panes:[{tabs:[{}, {workspace:"synthetic"}]}, {workspace:"synthetic"}]}});
  const fresh = await setup(); // New modules and empty origin-local storage.
  vi.stubGlobal("fetch", fetch);
  await fresh.persistence.initializeDesktopPreferences(fresh.settings.defaults);
  expect(fresh.data.size).toBe(0);
  const restored = fresh.settings.load().paneLayout!;
  expect(restored).toEqual(layout);
  const { restoreWorkspace } = await import("./workspace-binding");
  await restoreWorkspace("synthetic", restored.workspaceBindings?.synthetic);
  expect(JSON.parse(String(fetch.mock.calls.at(-1)![1]?.body))).toEqual({name:"synthetic",create:false,identity:"stable-one"});
  expect(fetch.mock.calls.filter(([url])=>url==="/workspaces")).toHaveLength(2);
});
