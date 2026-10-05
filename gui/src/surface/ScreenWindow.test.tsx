import { SpecScreen } from "./screens/Spec";
vi.mock("./screens/Spec", () => ({ SpecScreen: () => null }));
const transport = vi.hoisted(() => ({ submit: vi.fn<(...args: unknown[]) => Promise<void>>(), generation: "g1" }));
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { emptyWorkspace, type WorkspaceNode } from "../workspace";
import { GraphScreen } from "./screens/Graph";
import { SettingsScreen } from "./screens/Settings";
import { ScreenWindow } from "./ScreenWindow";

/* Synthetic workspace only. */
const node = (id: string, dependsOn: string[] = []): WorkspaceNode =>
  ({ id, name: id, command: ":calc {}", dependsOn, state: "ready", kept: true, provenance: {}, cautions: [] });
const workspace = { ...emptyWorkspace, nodes: [node("one"), node("two", ["one"])] };

vi.mock("./window-workspace", () => ({ useWindowWorkspace: () => ({ engine: { submit: transport.submit }, workspace, connection: "connected", generation: transport.generation }) }));
vi.mock("./results", () => ({ useResults: () => ({ held: new Map(), reads: new Map(), retry: () => undefined }) }));
vi.mock("./language", async (original) => ({ ...(await original<typeof import("./language")>()), language: () => new Promise(() => undefined) }));
vi.mock("./screens/GraphCanvas", () => ({ GraphCanvas: () => null }));

let tree: ReactTestRenderer | undefined;
afterEach(() => { transport.submit.mockReset(); transport.generation = "g1"; act(() => tree?.unmount()); tree = undefined; vi.unstubAllGlobals(); });

const draw = (route: Parameters<typeof ScreenWindow>[0]["route"]) => {
  act(() => { tree = create(<ScreenWindow route={route} />); });
  return tree!;
};

describe("a screen in a window of its own", () => {
  it("should_DrawTheGraphOfTheWorkspace_And_CloseTheWindowOnClose", () => {
    // Arrange
    const close = vi.fn();
    vi.stubGlobal("window", { close, open: vi.fn(), localStorage: { getItem: () => null, setItem: () => undefined } });
    vi.stubGlobal("document", { title: "" });
    // Act
    const shown = draw({ screen: "graph", workspace: "w1" });
    const graph = shown.root.findByType(GraphScreen);
    act(() => graph.props.onClose());
    // Assert
    expect(graph.props.nodes.map((it: { id: string }) => it.id)).toEqual(["one", "two"]);
    expect(graph.props.edges).toHaveLength(1);
    expect(graph.props.onJump).toBeUndefined();
    expect(graph.props.onRepeat).toBeUndefined();
    expect(close).toHaveBeenCalledOnce();
  });

  it("should_OpenTheSelectedResultInAnotherWindow_When_Asked", () => {
    // Arrange
    const open = vi.fn(() => ({}));
    vi.stubGlobal("window", { close: vi.fn(), open, localStorage: { getItem: () => null, setItem: () => undefined } });
    vi.stubGlobal("document", { title: "" });
    const shown = draw({ screen: "graph", workspace: "w1" });
    // Act: the newest node is selected until another is chosen
    act(() => shown.root.findByType(GraphScreen).props.onOpenResult());
    // Assert
    expect(open).toHaveBeenCalledWith(expect.stringMatching(/^#open\/two\/result\?workspace=w1$/), "_blank");
  });

  it("should_ShowTheSection_And_AnnounceAChoiceInsteadOfPersisting", () => {
    // Arrange
    const posted: unknown[] = [];
    class Channel { postMessage(data: unknown) { posted.push(data); } addEventListener() {} removeEventListener() {} close() {} }
    vi.stubGlobal("BroadcastChannel", Channel);
    const setItem = vi.fn();
    vi.stubGlobal("window", { close: vi.fn(), open: vi.fn(), localStorage: { getItem: () => null, setItem } });
    vi.stubGlobal("document", { title: "" });
    const shown = draw({ screen: "settings", section: "appearance" });
    const settings = shown.root.findByType(SettingsScreen);
    const row = settings.props.rows.find((it: { key?: string }) => it.key === "palette");
    const other = row.options.find((option: { name: string }) => option.name !== row.chosen);
    // Act
    act(() => settings.props.onChoose(row, other));
    // Assert
    expect(settings.props.section).toBe("appearance");
    expect(posted).toHaveLength(1);
    expect((posted[0] as { kind: string }).kind).toBe("settings");
    expect(setItem).not.toHaveBeenCalled();
  });

  it("should_MoveToTheSection_When_ATabIsChosen", () => {
    // Arrange
    const title = { title: "" };
    vi.stubGlobal("window", { close: vi.fn(), open: vi.fn(), localStorage: { getItem: () => null, setItem: vi.fn() } });
    vi.stubGlobal("document", title);
    const shown = draw({ screen: "settings", section: "appearance" });
    // Act
    act(() => shown.root.findByType(SettingsScreen).props.onSection("keys"));
    // Assert: the same window, on the other section, and named after it
    expect(shown.root.findByType(SettingsScreen).props.section).toBe("keys");
    expect(shown.root.findByType(SettingsScreen).props.rows).toEqual([]);
    expect(title.title).toBe("/settings keys · wes");
  });
});

it("renders /spec in its own window and submits through its bound engine, retaining failed attempt identity", async () => {
  const close = vi.fn();
  vi.stubGlobal("window", { close, open: vi.fn(), localStorage: { getItem: () => null, setItem: () => undefined } });
  vi.stubGlobal("document", { title: "" });
  const shown = draw({ screen: "spec", workspace: "api-import-lab" });
  expect(document.title).toBe("/spec · wes");
  expect(shown.root.findAllByType(GraphScreen)).toHaveLength(0);
  const spec = shown.root.findByType(SpecScreen);
  transport.submit.mockRejectedValueOnce(new Error("Connection lost; inspect the work before retrying."));
  await expect(spec.props.onSubmit(":describe file:synthetic.json provider:demo")).rejects.toThrow("Connection lost");
  expect(close).not.toHaveBeenCalled();
  const first = transport.submit.mock.calls[0]!;
  transport.submit.mockResolvedValueOnce(undefined);
  await expect(spec.props.onSubmit(":describe file:synthetic.json provider:demo")).resolves.toBe(first[0]);
  expect(transport.submit.mock.calls[1]).toEqual(first);
  transport.generation = "g2";
  act(() => shown.update(<ScreenWindow route={{ screen: "spec", workspace: "api-import-lab" }} />));
  await expect(shown.root.findByType(SpecScreen).props.onSubmit(":describe file:synthetic.json provider:demo")).rejects.toThrow("Workspace changed");
  act(() => shown.root.findByType(SpecScreen).props.onClose());
  expect(close).toHaveBeenCalledOnce();
});
