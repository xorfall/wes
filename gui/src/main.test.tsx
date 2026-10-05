import { afterEach, beforeEach, expect, it, vi } from "vitest";

const startup = vi.hoisted(() => ({ render: vi.fn(), initialize: vi.fn(), budgets: vi.fn(), gallery: vi.fn(), opened: vi.fn(), summoned: vi.fn() }));
vi.mock("react-dom/client", () => ({ createRoot: () => ({ render: startup.render }) }));
vi.mock("./local-telemetry", () => ({ initializeDiagnostics: vi.fn() }));
vi.mock("./limits/policy", async (original) => ({ ...await original<typeof import("./limits/policy")>(), initializeBudgets: startup.budgets }));
vi.mock("./desktop-preferences", () => ({ initializeDesktopPreferences: startup.initialize }));
vi.mock("./settings", () => ({ defaults: { surfacePalette: "paper" } }));
vi.mock("./surface/Gallery", () => ({ Gallery: "gallery", readGalleryRoute: startup.gallery }));
vi.mock("./surface/SurfaceApp", () => ({ SurfaceApp: "surface" }));
vi.mock("./surface/OpenWindow", () => ({ OpenWindow: "opened" }));
vi.mock("./surface/ScreenWindow", () => ({ ScreenWindow: "summoned" }));
vi.mock("./surface/open-route", () => ({ readOpenRoute: startup.opened, readScreenRoute: startup.summoned }));
vi.mock("./Boundary", () => ({ Boundary: "boundary" }));

beforeEach(() => {
  vi.resetModules(); vi.clearAllMocks();
  startup.initialize.mockResolvedValue(undefined); startup.budgets.mockResolvedValue(undefined);
  startup.gallery.mockReturnValue(undefined); startup.opened.mockReturnValue(undefined); startup.summoned.mockReturnValue(undefined);
  vi.stubGlobal("window", { location: { hash: "" } });
  vi.stubGlobal("document", { getElementById: () => ({}) });
});
afterEach(() => vi.unstubAllGlobals());
const mounted = () => startup.render.mock.calls[0]![0].props.children.props.children;

it("waits for personal preferences before mounting the Surface client", async () => {
  let ready!: () => void;
  startup.initialize.mockReturnValue(new Promise<void>(resolve => { ready = resolve; }));
  await import("./main");
  expect(startup.render).not.toHaveBeenCalled();
  await vi.waitFor(() => expect(startup.initialize).toHaveBeenCalled());
  ready(); await vi.waitFor(() => expect(startup.render).toHaveBeenCalled());
  expect(mounted().type).toBe("surface");
});
it("keeps result-window routes independent of the session client", async () => {
  const route = { node: "n1", view: "result" };
  startup.opened.mockReturnValue(route);
  await import("./main");
  await vi.waitFor(() => expect(startup.render).toHaveBeenCalled());
  expect(mounted()).toMatchObject({ type: "opened", props: { route } });
});
it("keeps synthetic gallery routes addressable", async () => {
  const route = { screen: "cells-minimal", palette: "ink" };
  startup.gallery.mockReturnValue(route);
  await import("./main");
  await vi.waitFor(() => expect(startup.render).toHaveBeenCalled());
  expect(mounted()).toMatchObject({ type: "gallery", props: { route } });
});
it("keeps screen-window routes independent of the session client", async () => {
  const route = { screen: "graph", workspace: "w1" };
  startup.summoned.mockReturnValue(route);
  await import("./main");
  await vi.waitFor(() => expect(startup.render).toHaveBeenCalled());
  expect(mounted()).toMatchObject({ type: "summoned", props: { route } });
});

it("loads active budgets before preferences or client consumers mount", async () => {
  let ready!:()=>void; startup.budgets.mockReturnValue(new Promise<void>(resolve=>{ready=resolve;}));
  await import("./main"); expect(startup.initialize).not.toHaveBeenCalled(); expect(startup.render).not.toHaveBeenCalled();
  ready(); await vi.waitFor(()=>expect(startup.render).toHaveBeenCalled()); expect(startup.initialize).toHaveBeenCalledTimes(1);
});
