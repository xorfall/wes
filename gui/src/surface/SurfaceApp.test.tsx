vi.mock("./screens/Spec", () => ({ SpecScreen: () => null }));
import { SpecScreen } from "./screens/Spec";
vi.mock("../workspace-events", () => ({ WorkspaceEvents: function(path: string) { return new EventSource(path); } }));
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { EditorState, type Transaction } from "@codemirror/state";
import { indentSelection } from "@codemirror/commands";
import { calcExtensions } from "./calc-editor";
import { bundledPackage, readLanguage } from "./language";
import { Engine } from "../engine";
import { applicationLog } from "../application-log";
import { SurfaceApp } from "./SurfaceApp";
import { DashboardHost } from "../dashboard/DashboardHost";
import { addDashboardMember,newDashboard } from "../dashboard/model";
import { OpenWindow } from "./OpenWindow";
import { Prompt } from "./Prompt";
import { Session } from "./Session";
import { PaneCommand } from "./PaneCommand";
import { ShellTerminal } from "../ShellTerminal";
import { close as closePane } from "./split-model";
import { Split } from "./Split";
import { Cell } from "./Cell";
import { ComponentPane } from "./ComponentPane";
import { GraphScreen } from "./screens/Graph";
import { SettingsScreen } from "./screens/Settings";
import { PeekScreen } from "./screens/Peek";
import { GraphCanvas } from "./screens/GraphCanvas";
import { OpenScreen } from "./screens/Open";
import { EditFileScreen } from "./screens/EditFile";
import { EditScreen } from "./screens/Edit";
import { EnvScreen } from "./screens/Env";
import { InteractiveResponse } from "./InteractiveResponse";
import { InteractivePreview } from "./forms/Interactive";
import { ReadStatus } from "./ReadStatus";
import { httpValue } from "../testing/http-response";
import { SourceView } from "./SourceView";
import { lineText, MonoLine, type Segment } from "./MonoLine";
import type { Event, Request, StoredValue } from "../protocol";

it("submits unsaved env editor text as one plan operation and keeps apply explicit", async () => {
  await act(async () => tree.root.findByType(Prompt).props.onSubmit("/edit env"));
  const screen = tree.root.findByType(EditFileScreen);
  let result!: Promise<string>;
  await act(async () => {
    result = screen.props.onRun("version: 1\npackage: synthetic", {
      name: "", origin: "editor:fixture", base: "/synthetic", generation: "g1", environments: { selected: "DEV", revisions: {} },
    });
  });
  const request = requests.find(request => request.request === "submit");
  expect(request).toMatchObject({ request: "submit", document: { source: "version: 1\npackage: synthetic" } });
  if (request?.request !== "submit") throw new Error("missing submit");
  expect(request.text).toMatch(/^:env plan source:"" origin:"editor:fixture" base:"\/synthetic" > editorPlan/);
  await emit({ event: "planned", cell: request.cell, text: request.text, nodes: [], diagnostics: [{ code: "ENV000", severity: "info", message: "Synthetic plan preview", start: 0, end: 0, hints: [] }] });
  await expect(result).resolves.toContain(`:env apply $${request.text.split(" > ")[1]}`);
  await expect(result).resolves.toContain("Synthetic plan preview");
  await expect(result).resolves.toContain(`:env discard $${request.text.split(" > ")[1]}`);
  expect(requests.filter(request => request.request === "submit")).toHaveLength(1);
  expect(tree.root.findAllByType(EditFileScreen)).toHaveLength(1);
});

const preferenceFixture = vi.hoisted(() => ({ state: { saving: false } as { saving: boolean; error?: string }, listeners: new Set<() => void>() }));
vi.mock("../desktop-preferences", async original => ({
  ...await original<typeof import("../desktop-preferences")>(),
  persistenceState: () => preferenceFixture.state,
  subscribePersistence: (listener: () => void) => { preferenceFixture.listeners.add(listener); return () => preferenceFixture.listeners.delete(listener); },
}));
vi.mock("../ShellTerminal", () => ({ ShellTerminal: () => null }));
vi.mock("./screens/GraphCanvas", () => ({ GraphCanvas: () => null }));
vi.mock("./language", async original => {
  const m = await original<typeof import("./language")>();
  return { ...m, language: () => Promise.resolve(m.readLanguage(m.bundledPackage, "engine")) };
});

/** The real Engine transport, fed entirely by synthetic events and responses. */
class Events {
  static current: Events;
  static all = new Map<string, Events>();
  onmessage?: (message: { data: string }) => void;
  onerror?: () => void;
  constructor(url = "/events") { Events.current = this; Events.all.set(url, this); }
  close() {}
  emit(event: Event) { this.onmessage?.({ data: JSON.stringify(event) }); }
}
let tree: ReactTestRenderer;
let requests: Request[];
let fetchValue: ReturnType<typeof vi.fn<(handle: string) => Promise<Response>>>;
let send: ReturnType<typeof vi.fn<(request: Request) => Promise<Response>>>;
const ok = () => new Response("", { status: 200 });
const value = (data: string): StoredValue => ({ type: { kind: "primitive", name: "Text" }, data, provenance: {} });
beforeEach(async () => {
  applicationLog.clear();
  vi.spyOn(Engine.prototype, "terminal").mockResolvedValue({ target: null });
  preferenceFixture.state = { saving: false };
  requests = [];
  Events.all.clear();
  fetchValue = vi.fn(async () => Response.json(value("synthetic result")));
  send = vi.fn(async () => ok());
  vi.stubGlobal("EventSource", Events);
  vi.stubGlobal("document", { title: "", querySelector: () => null, createElement: () => ({ getContext: () => null }) });
  vi.stubGlobal("window", {
    localStorage: { getItem: () => null, setItem: () => {} },
    requestAnimationFrame: (callback: () => void) => callback(),
    open: vi.fn(() => ({})),
    fetch: vi.fn(async (url: string, options?: RequestInit) => {
      if (url.startsWith("/values/")) return fetchValue(decodeURIComponent(url.slice("/values/".length)));
      const request = JSON.parse(String(options?.body)) as Request;
      requests.push(request);
      return send(request);
    }),
  });
  await act(async () => { tree = create(<SurfaceApp />); });
  await emit({ event: "session", generation: "g1" });
  await environment("DEV");
});
afterEach(() => {
  act(() => tree.unmount());
  vi.restoreAllMocks(); vi.unstubAllGlobals();
});
async function emit(...events: Event[]) {
  await act(async () => { for (const event of events) Events.current.emit(event); });
}
function env(selected: string | null): Extract<Event, { event: "environments" }> {
  return { event: "environments", managed: true, default: selected, enabled: { DEV: true, PROD: false },
    credentials: {}, revisions: { DEV: "dev-r1", PROD: "prod-r1" }, providers: {}, clients: {} };
}
async function environment(selected: string | null) { await emit(env(selected)); }
async function type(text: string) { await act(async () => tree.root.findByType(Prompt).props.onDraft(text)); }
async function submit(text: string) {
  await type(text);
  await act(async () => tree.root.findByType(Prompt).props.onSubmit(text));
}
const submits = () => requests.filter((request): request is Extract<Request, { request: "submit" }> => request.request === "submit");
const last = () => submits().at(-1)!;
function created(id: string, extra = {}): Event {
  return { event: "created", dependencyLifetime: "continuous", node: id, name: id, command: ":calc 1", dependsOn: [], repeatable: true, traced: false, interactive: false, ...extra };
}
function ready(node: string, handle: string): Event {
  return { event: "ready", node, handle, type: "Text", bytes: 1, provenance: {} } as Event;
}
async function accept(id: string, extra = {}) {
  const request = last();
  await emit(created(id, extra), { event: "planned", cell: request.cell, text: request.text, nodes: [id], repeatOf: request.repeat },
    { event: "node", constructionComplete: false, node: id, state: "ready" } as Event);
}
function session() { return tree.root.findByType(Session); }
function repeat(index = 0) {
  const current = session();
  current.props.actions(current.props.model.cells[index]).repeat(false);
}
function text() { return JSON.stringify(tree.toJSON()); }
function button(label: string) { return tree.root.findAllByType("button").find(node => node.children.join("") === label)!; }

it("opens a restored multiline calculation's source read only, leaving the draft and the scrollback alone", async () => {
  const source = ':calc {\n\tconst label = "two  spaces";\n\n  return label;\n} > label';
  await emit(created("label-node", { name: "label", command: source }),
    { event: "planned", cell: "restored-calc", text: source, nodes: ["label-node"], restored: true });
  await type(":calc 999");
  expect(session().props.model.cells[0].rows.map((row: { segments: Segment[] }) => lineText(row.segments)).join("\n")).toBe(source);
  // The cell it came from keeps its own output slot: a view is somewhere else to read, not a swap.
  expect(tree.root.findAllByType(SourceView)).toHaveLength(0);
  await act(async () => tree.root.findAllByProps({ className: "cell-source" })[0]!.props.onClick({
    metaKey: true, button: 0, preventDefault() {}, stopPropagation() {},
  }));
  // The plain window: only the code, the calc body formatted, no tabs.
  expect(tree.root.findAllByType(OpenScreen)).toHaveLength(0);
  expect(tree.root.findByType(PeekScreen).props.what).toBe("source");
  expect(tree.root.findByType(SourceView).props.source).toBe(':calc {\n  const label = "two  spaces";\n  return label;\n} > label');
  expect(tree.root.findAllByType(EditScreen)).toHaveLength(0);
  expect(submits()).toHaveLength(0);
  await act(async () => tree.root.findByType(PeekScreen).props.onClose());
  expect(tree.root.findAllByType(SourceView)).toHaveLength(0);
  expect(session().props.model.cells[0].source).toBe(source);
  // Reading the source never touched what was half-typed, which is the whole point of elsewhere.
  expect(tree.root.findByType(Prompt).props.draft).toBe(":calc 999");
  expect(submits()).toHaveLength(0);
  // With results opening in a window, the piece goes there too, at its own address.
  await submit("/settings results open window");
  await act(async () => tree.root.findAllByProps({ className: "cell-source" })[0]!.props.onClick({
    metaKey: true, button: 0, preventDefault() {}, stopPropagation() {},
  }));
  expect(window.open).toHaveBeenLastCalledWith(expect.stringMatching(/^#source\/restored-calc/), "_blank");
  expect(tree.root.findAllByType(PeekScreen)).toHaveLength(0);
});

it("repeats with a new attempt and retries a lost repeat reply with the same intent", async () => {
  await submit(":calc 1"); const original = last(); await accept("one");
  send.mockRejectedValueOnce(new Error("synthetic disconnect"));
  await act(async () => repeat());
  const again = last();
  expect(again.cell).not.toBe(original.cell);
  expect(again.repeat).toBe(original.cell);
  expect(session().props.model.cells[0].state).toBe("failed");
  await environment("PROD");
  await act(async () => repeat());
  expect(last()).toEqual(again);
  expect(session().props.model.cells).toHaveLength(1);
});

it("reports a refused cancel request instead of swallowing it, and sends nothing else", async () => {
  await submit(":calc 1"); await accept("one");
  await emit({ event: "node", constructionComplete: false, node: "one", state: "running" } as Event);
  send.mockResolvedValueOnce(new Response("synthetic checkpoint in progress", { status: 409 }));
  const cancelled = session().props.actions(session().props.model.cells[0]).cancel();
  await act(async () => { await cancelled.catch(() => undefined); });
  expect(requests.filter(request => request.request === "cancel")).toEqual([{ request: "cancel", node: "one" }]);
  expect(text()).toContain("synthetic checkpoint in progress");
  expect(submits()).toHaveLength(1);
});

it("retries an unanswered first submission without inventing a repeat", async () => {
  send.mockRejectedValueOnce(new Error("synthetic disconnect"));
  await submit(":calc 1"); const original = last();
  await act(async () => repeat());
  expect(last()).toEqual(original);
  expect(last().repeat).toBeUndefined();
});

it("waits for the first submission acknowledgement before admitting a repeat", async () => {
  await submit(":calc 1");
  await act(async () => repeat());
  expect(submits()).toHaveLength(1);
  expect(text()).toContain("Wait for the submission acknowledgement");
});

it("asks before graph and editor repeat, then records a fresh acknowledged attempt", async () => {
  await submit(":calc 1"); await accept("one", { repeatable: false });
  await submit("/graph");
  await act(async () => tree.root.findByType(GraphScreen).props.onRepeat());
  expect(submits()).toHaveLength(1);
  expect(text()).toContain("confirm repeat");
  await act(async () => button("cancel").props.onClick());
  expect(submits()).toHaveLength(1);
  await act(async () => tree.root.findByType(GraphScreen).props.onClose());
  await submit("/edit $one");
  await act(async () => tree.root.findByType(EditScreen).props.onRunAgain());
  expect(submits()).toHaveLength(1);
  await act(async () => button("confirm repeat").props.onClick());
  expect(submits()).toHaveLength(2);
  expect(last().acknowledge_effects).toBe(true);
});

it("says the previous outcome is unknown in the graph and editor repeat question and repeats only on confirmation", async () => {
  await submit(":calc 1"); await accept("one", { repeatable: false }); const original = last();
  await emit({ event: "interrupted", node: "one", cell: original.cell, capability: "UNSAFE", safe: false, when: "2026-10-04T09:00:00Z" });
  await submit("/graph");
  await act(async () => tree.root.findByType(GraphScreen).props.onRepeat());
  expect(text()).toContain("the previous attempt's outcome is unknown");
  expect(submits()).toHaveLength(1);
  await act(async () => button("cancel").props.onClick());
  await act(async () => tree.root.findByType(GraphScreen).props.onClose());
  await submit("/edit $one");
  await act(async () => tree.root.findByType(EditScreen).props.onRunAgain());
  expect(text()).toContain("the previous attempt's outcome is unknown");
  expect(submits()).toHaveLength(1);
  await act(async () => button("confirm repeat").props.onClick());
  expect(submits()).toHaveLength(2);
  expect(last().acknowledge_effects).toBe(true);
});

it("does not confirm work that changed while the question was open", async () => {
  await submit(":calc 1"); await accept("one", { repeatable: false }); const original = last();
  await submit("/graph");
  await act(async () => tree.root.findByType(GraphScreen).props.onRepeat());
  await emit({ event: "planned", cell: "external-repeat", text: original.text, nodes: ["one"], repeatOf: original.cell });
  await act(async () => button("confirm repeat").props.onClick());
  expect(submits()).toHaveLength(1);
  expect(text()).toContain("work changed while confirming");
});

it("holds a permanent read failure until explicit retry and never resubmits source", async () => {
  fetchValue.mockResolvedValueOnce(new Response("gone", { status: 410 }));
  await submit(":calc 1"); await accept("one"); await emit(ready("one", "gone"));
  expect(fetchValue).toHaveBeenCalledTimes(1);
  expect(text()).toContain("HTTP 410");
  await environment("PROD"); await emit({ event: "workspace-context", name: "synthetic", saved: [] });
  expect(fetchValue).toHaveBeenCalledTimes(1);
  await act(async () => tree.root.findByType(ReadStatus).props.onRetry());
  expect(fetchValue).toHaveBeenCalledTimes(2);
  expect(submits()).toHaveLength(1);
  expect(text()).toContain("synthetic result");
});

it("draws a completed value without waiting for a slow peer and deduplicates shared handles", async () => {
  let finish!: (response: Response) => void;
  fetchValue.mockImplementation(handle => handle === "slow" ? new Promise(resolve => { finish = resolve; }) : Promise.resolve(Response.json(value("fast result"))));
  await submit(":calc 1"); await accept("one");
  await submit(":calc 2"); await accept("two");
  await emit(ready("one", "slow"), ready("two", "fast"), created("three"), ready("three", "fast"));
  expect(fetchValue).toHaveBeenCalledTimes(2);
  expect(text()).toContain("fast result");
  await act(async () => finish(Response.json(value("slow result"))));
  expect(text()).toContain("slow result");
});

it("opens the selected graph node and obeys screen/window preferences", async () => {
  await emit(created("one"), created("two"));
  await submit("/graph");
  await act(async () => tree.root.findByType(GraphCanvas).props.onSelect("one"));
  await act(async () => tree.root.findByType(GraphScreen).props.onOpenResult());
  expect(lineText(tree.root.findByType(OpenScreen).props.subject)).toContain("$one");
  await act(async () => tree.root.findByType(OpenScreen).props.onClose());
  // With results opening in a window, the graph and the settings go to a window of their own too.
  await submit("/settings results open window"); await submit("/graph");
  expect(window.open).toHaveBeenLastCalledWith(expect.stringMatching(/^#graph/), "_blank");
  expect(tree.root.findAllByType(GraphScreen)).toHaveLength(0);
  await submit("/settings appearance");
  expect(window.open).toHaveBeenLastCalledWith(expect.stringMatching(/^#settings\/appearance/), "_blank");
  expect(submits()).toHaveLength(0);

});

it("opens the settings from the top line the way /settings does, and moves between sections by tab", async () => {
  // The glyph at the right of the top line is `/settings` pressed rather than typed.
  const press = async () => act(async () => tree.root.findByProps({ "aria-label": "Settings" }).props.onClick({ stopPropagation() {} }));
  await press();
  expect(tree.root.findByType(SettingsScreen).props.section).toBe("appearance");
  await act(async () => tree.root.findByType(SettingsScreen).props.onSection("keys"));
  expect(tree.root.findByType(SettingsScreen).props.section).toBe("keys");
  await act(async () => tree.root.findByType(SettingsScreen).props.onClose());
  expect(tree.root.findAllByType(SettingsScreen)).toHaveLength(0);
  // With results opening in a window, the glyph opens a window of its own, like the typed command.
  await submit("/settings results open window");
  await press();
  expect(window.open).toHaveBeenLastCalledWith(expect.stringMatching(/^#settings/), "_blank");
  expect(tree.root.findAllByType(SettingsScreen)).toHaveLength(0);
  // In a pane, the tabs move that pane between sections and its head follows.
  await submit("/settings results open screen");
  await submit("/settings keys split");
  const pane = () => tree.root.findByType(Split).props.state.panes.find((it: { shows?: { screen: string } }) => it.shows?.screen === "settings");
  expect(pane().shows.section).toBe("keys");
  await act(async () => tree.root.findByType(SettingsScreen).props.onSection("aliases"));
  expect(pane().shows.section).toBe("aliases");
  expect(pane().title).toBe("/settings aliases");
});

it("shows engine environments, preserves no selection, and waits for authoritative selection", async () => {
  await environment(null); await submit("/env");
  const screen = () => tree.root.findByType(EnvScreen);
  expect(screen().props.environments.map((e: { name: string }) => e.name)).toEqual(["DEV", "PROD"]);
  expect(screen().props.environments[0].credentials).toBeUndefined();
  expect(screen().props.chosen).toBe("");
  await act(async () => screen().props.onChoose("PROD"));
  expect(last().text).toBe(':env use "PROD"');
  expect(screen().props.chosen).toBe("");
  await environment("PROD");
  expect(screen().props.chosen).toBe("PROD");
  await act(async () => screen().props.onClear());
  expect(last().text).toBe(":env clear");
});

it("keeps an environment refusal visible across unrelated events", async () => {
  await submit("/env");
  await act(async () => tree.root.findByType(EnvScreen).props.onChoose("PROD"));
  await emit({ event: "planned", cell: last().cell, text: last().text, nodes: [], failure: "synthetic selection refused" });
  await emit(created("unrelated"));
  expect(text()).toContain("synthetic selection refused");
  expect(tree.root.findByType(EnvScreen).props.chosen).toBe("DEV");
});

it("enables the selected environment through the existing command without choosing or granting", async () => {
  await environment("PROD"); await submit("/env");
  await act(async () => tree.root.findByType(EnvScreen).props.onEnable("PROD"));
  expect(last().text).toBe(':env enable "PROD"');
  expect(tree.root.findByType(EnvScreen).props.chosen).toBe("PROD");
});

it("retains editor draft context through prompt transfer, environment controls and repeated runs", async () => {
  await type(":calc 1");
  await act(async () => tree.root.findByType(Prompt).props.onGrow(":calc 1"));
  await environment("PROD");
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  await submit("/env");
  await act(async () => tree.root.findByType(EnvScreen).props.onChoose("PROD"));
  await act(async () => tree.root.findByType(EnvScreen).props.onClose());
  await submit("/edit");
  await act(async () => tree.root.findByType(EditScreen).props.onRun(":calc 1"));
  expect(last().environments?.selected).toBe("DEV");
  await act(async () => tree.root.findByType(EditScreen).props.onRun(":calc 2"));
  expect(last().environments?.selected).toBe("DEV");
  await act(async () => button("use selected context for this draft").props.onClick());
  await act(async () => tree.root.findByType(EditScreen).props.onRun(":calc 3"));
  expect(last().environments?.selected).toBe("PROD");
});

it("refuses a stale prompt repeatedly without consuming it and requires explicit context review", async () => {
  await type(":calc 1");
  await emit({ event: "session", generation: "g2" }); await environment("PROD");
  await act(async () => tree.root.findByType(Prompt).props.onSubmit(":calc 1"));
  await act(async () => tree.root.findByType(Prompt).props.onSubmit(":calc 1"));
  expect(submits()).toHaveLength(0);
  expect(tree.root.findByType(Prompt).props.draft).toBe(":calc 1");
  await act(async () => button("use selected context for this draft").props.onClick());
  await act(async () => tree.root.findByType(Prompt).props.onSubmit(":calc 1"));
  expect(last().environments?.selected).toBe("PROD");
});

it("keeps prompt context independent from a retained editor draft", async () => {
  await submit("/edit");
  await act(async () => tree.root.findByType(EditScreen).props.onChange(":calc 1"));
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  await type(""); // Deliberately start new work instead of editing the returned draft.
  await environment("PROD"); await submit(":calc 2");
  expect(last().environments?.selected).toBe("PROD");
  await submit("/edit");
  await act(async () => tree.root.findByType(EditScreen).props.onRun(":calc 1"));
  expect(last().environments?.selected).toBe("DEV");
});

it("sends interactive input and EOF to the exact run, without making commands", async () => {
  await submit("@interactive fixture ask"); await accept("one", { interactive: true });
  await emit({ event: "node", constructionComplete: false, node: "one", state: "running" } as Event,
    { event: "conversation", node: "one", run: "run-a", active: true });
  const preview = () => tree.root.findByType(InteractivePreview);
  await act(async () => preview().props.onAnswer("synthetic answer"));
  expect(preview().props.answer).toBe("synthetic answer");
  await act(async () => preview().props.onSend());
  expect(requests.at(-1)).toEqual({ request: "input", node: "one", run: "run-a", text: "synthetic answer\n" });
  expect(preview().props.answer).toBe("");
  await act(async () => preview().props.onAnswer("old run draft"));
  await emit({ event: "conversation", node: "one", run: "run-b", active: true });
  expect(preview().props.answer).toBe("");
  await act(async () => button("close input (EOF)").props.onClick());
  expect(requests.at(-1)).toEqual({ request: "eof", node: "one", run: "run-b" });
  expect(submits()).toHaveLength(1);
});

it("does not resend unconfirmed input and isolates typing from cell shortcuts", async () => {
  await submit("@interactive fixture ask"); await accept("one", { interactive: true });
  await emit({ event: "node", constructionComplete: false, node: "one", state: "running" } as Event,
    { event: "conversation", node: "one", run: "run-a", active: true });
  const input = tree.root.findByType(InteractiveResponse).findByType("input");
  const stopPropagation = vi.fn();
  act(() => input.props.onKeyDown({ key: "r", stopPropagation }));
  expect(stopPropagation).toHaveBeenCalledTimes(1);
  send.mockRejectedValueOnce(new Error("synthetic disconnect"));
  await act(async () => tree.root.findByType(InteractivePreview).props.onSend());
  await emit({ event: "workspace-context", name: "synthetic", saved: [] });
  expect(requests.filter(r => r.request === "input")).toHaveLength(1);
  expect(text()).toContain("input delivery is unconfirmed");
});

it("shows the editor's result through the same output slot as the scrollback", async () => {
  await submit("/edit");
  await act(async () => tree.root.findByType(EditScreen).props.onRun(":calc 1"));
  await accept("one"); await emit(ready("one", "held"));
  expect(tree.root.findByType(EditScreen).findByType(Cell).props.blocks.length).toBeGreaterThan(0);
  expect(text()).toContain("synthetic result");
});

it("does not treat a slash typed in an editor as screen navigation", async () => {
  await submit("/edit");
  act(() => tree.root.findByProps({ className: "wes-terminal surface-terminal surface-app" }).props.onKeyDown({
    key: "/", target: { closest: () => ({}) }, preventDefault: vi.fn(),
  }));
  expect(tree.root.findAllByType(EditScreen)).toHaveLength(1);
});

it("retains a labelled observation during handle replacement and discards it on workspace change", async () => {
  act(() => tree.unmount());
  await act(async () => { tree = create(<OpenWindow route={{ node: "one", tab: "json" }} />); });
  await emit({ event: "session", generation: "window-g1" }, created("one"), ready("one", "first"));
  expect(tree.root.findByType(OpenScreen).props.json).toContain("synthetic result");
  let finish!: (response: Response) => void;
  fetchValue.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  await emit(ready("one", "second"));
  expect(tree.root.findByType(OpenScreen).props.json).toContain("synthetic result");
  expect(JSON.stringify(tree.toJSON())).toContain("Updating · previous result");
  await emit({ event: "session", generation: "window-g2" }, { event: "workspace-context", name: "empty", saved: [] });
  await act(async () => finish(Response.json(value("late old result"))));
  expect(tree.root.findByType(OpenScreen).props.json).toBe("");
  expect(lineText(tree.root.findByType(OpenScreen).props.subject)).toContain("no result called one");
});


/*
 * The workspace in panes.
 *
 * The model and the screen came from the gallery and nothing could reach them; these are the four
 * things that had to be true before the mode was real — the words work, a screen goes where it is
 * sent and stays there, a second session is a second session, and the room can be given back.
 */
const panes = () => tree.root.findByType(Split).props.state;
const prompts = () => tree.root.findAllByType(Prompt);
/** Once the workspace is divided there is a prompt per session pane, so a test says which. */
async function fromPane(at: number, text: string) {
  await act(async () => prompts()[at]!.props.onDraft(text));
  await act(async () => prompts()[at]!.props.onSubmit(text));
}
const from = (text: string) => fromPane(0, text);

/** `Shift+D` on the first cell of a session view: the review gesture every chrome shares. */
const reviewDeletion = (view: ReturnType<typeof session>) => act(async () => {
  view.findAllByType(Cell)[0]!.findByType("section").props.onKeyDown({
    key: "D", shiftKey: true, repeat: false, metaKey: false, ctrlKey: false, altKey: false,
    preventDefault() {}, stopPropagation() {},
  });
});

it.each([0, 1])("deletes reviewed work from session pane %s only through authoritative retirement", async pane => {
  if (pane === 1) await from("/split right");
  await fromPane(pane, ":calc 1"); const original = last(); await accept("one");
  const current = () => tree.root.findAllByType(Session)[pane]!;
  await act(async () => current().props.actions(current().props.model.cells[0]).repeat(false));
  const repeated = last(); await accept("one");
  expect(repeated.cell).not.toBe(original.cell);
  vi.stubGlobal("fetch", window.fetch);
  send.mockImplementation(async request => request.request === "delete-work-preview"
    ? Response.json({ token: "reviewed", cells: [original.cell, repeated.cell], nodes: ["one"], dependents: [],
      labels: { [original.cell]: original.text, [repeated.cell]: repeated.text }, payloads: [], protected: [], sharedWorkspaces: ["other"], expiresInSeconds: 120 })
    : Response.json({ deleted: true }));
  await reviewDeletion(current());
  expect(requests.at(-1)).toMatchObject({ request: "delete-work-preview", cell: repeated.cell });
  expect(current().props.model.cells).toHaveLength(1);
  expect(text()).toContain("Shared results stay for workspaces");
  await act(async () => button("Delete work").props.onClick());
  expect(requests.at(-1)).toMatchObject({ request: "delete-work", token: "reviewed", dependents: true, protected: false });
  // A response is not an authoritative event; never remove the row optimistically.
  expect(current().props.model.cells).toHaveLength(1);
  await emit({ event: "dropped", nodes: ["one"] });
  await emit({ event: "work-retired", cells: [original.cell, repeated.cell] });
  expect(tree.root.findAllByType(Session).every(view => view.props.model.cells.length === 0)).toBe(true);
});

it("a same-session reconnect removes missed retired cells without revoking the terminal generation", async () => {
  await submit(":calc 1"); await accept("one");
  expect(session().props.model.cells).toHaveLength(1);
  await emit({ event: "session", generation: "g1", cells: [] });
  expect(session().props.model.cells).toHaveLength(0);
});

it("leaves completed cells intact when an affected downstream node blocks deletion", async () => {
  await submit(":calc 1"); await accept("one");
  vi.stubGlobal("fetch", window.fetch);
  send.mockImplementation(async () => Response.json({ code: "STO007", message: "Deletion blocked; no work removed.",
    blockers: [{ node: "two", cells: ["dependent"], state: "Running", reason: "execution remains active" }] }, { status: 409 }));
  await reviewDeletion(session());
  expect(text()).toContain("execution remains active"); expect(text()).toContain("$two");
  // The blocker's cells are the engine's ids, not something to read; the node is.
  expect(text()).not.toContain("cells:");
  expect(session().props.model.cells).toHaveLength(1);
  expect(requests.filter(request => request.request === "delete-work")).toHaveLength(0);
});

async function promptKey(key: string, pane = 0) {
  await act(async () => {
    const field = prompts()[pane]!.findByType("textarea");
    const end = field.props.value.length;
    field.props.onKeyDown({ key, currentTarget: { selectionStart: end, selectionEnd: end },
      preventDefault: () => {}, stopPropagation: () => {} });
  });
}

it("recalls restored commands as drafts and captures their new composition context without executing", async () => {
  await emit(
    { event: "planned", cell: "saved-one", text: ":calc 11", nodes: [], restored: true },
    { event: "planned", cell: "saved-two", text: ":calc 22", nodes: [], restored: true },
  );
  await promptKey("ArrowUp");
  expect(prompts()[0]!.props.draft).toBe(":calc 22");
  await promptKey("ArrowUp");
  expect(prompts()[0]!.props.draft).toBe(":calc 11");
  await promptKey("ArrowDown"); await promptKey("ArrowDown");
  expect(prompts()[0]!.props.draft).toBe("");
  expect(submits()).toHaveLength(0);
  await promptKey("ArrowUp");
  await environment("PROD");
  await promptKey("Enter");
  expect(submits()).toHaveLength(1);
  expect(last().text).toBe(":calc 22");
  expect(last().environments?.selected).toBe("DEV");
  expect(prompts()[0]!.props.draft).toBe("");
});

it("drops an old history walk on workspace replacement", async () => {
  await emit({ event: "planned", cell: "old", text: ":calc 11", nodes: [], restored: true });
  await type("unfinished old draft");
  await promptKey("ArrowUp");
  await emit({ event: "session", generation: "g2" },
    { event: "planned", cell: "new", text: ":calc 22", nodes: [], restored: true });
  await promptKey("ArrowDown");
  expect(prompts()[0]!.props.draft).toBe(":calc 11");
  await promptKey("ArrowUp"); await promptKey("ArrowUp");
  expect(prompts()[0]!.props.draft).toBe(":calc 22");
  expect(submits()).toHaveLength(0);
});

it("recalls only each session pane's commands and restores their independent drafts", async () => {
  await submit(":calc 1"); await accept("one");
  await from("/split right");
  await fromPane(1, ":calc 2");
  const sent = last();
  await emit(created("two"), { event: "planned", cell: sent.cell, text: sent.text, nodes: ["two"] });
  await act(async () => {
    prompts()[0]!.props.onDraft("left unfinished");
    prompts()[1]!.props.onDraft("right unfinished");
  });
  await promptKey("ArrowUp", 0); await promptKey("ArrowUp", 1);
  expect(prompts().map(prompt => prompt.props.draft)).toEqual([":calc 1", ":calc 2"]);
  await promptKey("ArrowUp", 0); await promptKey("ArrowUp", 1);
  expect(prompts().map(prompt => prompt.props.draft)).toEqual([":calc 1", ":calc 2"]);
  await promptKey("ArrowDown", 0); await promptKey("ArrowDown", 1);
  expect(prompts().map(prompt => prompt.props.draft)).toEqual(["left unfinished", "right unfinished"]);
  expect(submits()).toHaveLength(2);
});

it("keeps an authoritative pre-execution refusal in the cell and retries it only on an explicit action", async () => {
  const source = ":sandbox { :calc { return $outsideCount + 3; } > projected } > scratch";
  const reason = "CAL010: Reference $outsideCount. Sandbox calculations can only reference their own declared members.";
  send.mockImplementation(async () => new Response(reason, { status: 400, headers: { "X-Wes-Submission-Outcome": "not-started" } }));
  await from(source);
  const first = last();
  const cell = () => tree.root.findByType(Cell);
  const verdict = () => cell().props.verdict.flatMap((field: { segments: Segment[] }) => field.segments).map((segment: Segment) => segment.text).join(" ");
  expect(verdict()).toContain("refused");
  expect(verdict()).toContain("nothing ran");
  expect(verdict()).toContain(reason);
  expect(verdict()).not.toContain("outcome unknown");
  expect(submits()).toHaveLength(1);
  // A transient notice being cleared and a same-generation reconnect cannot erase local refusal evidence.
  await from("/theme ink");
  await emit({ event: "session", generation: "g1", cells: [] });
  expect(verdict()).toContain(reason);
  expect(submits()).toHaveLength(1);
  await act(async () => cell().props.actions.repeat(false));
  expect(submits()).toHaveLength(2);
  expect(last().cell).not.toBe(first.cell);
  expect(last().repeat).toBeUndefined();
  expect(verdict()).toContain("refused");
});

it.each(["network", "unmarked-refusal", "server-error", "unknown-marker"])("keeps %s uncertain without falsely claiming nothing ran", async failure => {
  if (failure === "network") send.mockRejectedValue(new Error("synthetic connection loss"));
  else send.mockResolvedValue(new Response("CAL010: synthetic refusal text is not execution evidence", {
    status: failure === "server-error" ? 500 : 400,
    headers: failure === "unknown-marker" ? { "X-Wes-Submission-Outcome": "unrecognized" } : {},
  }));
  await from(":calc { return 7; }");
  const verdict = tree.root.findByType(Cell).props.verdict.flatMap((field: { segments: Segment[] }) => field.segments).map((segment: Segment) => segment.text).join(" ");
  expect(verdict).toContain("outcome unknown");
  expect(verdict).not.toContain("nothing ran");
  expect(submits()).toHaveLength(1);
});

it("divides the workspace and gives the last pane back", async () => {
  await submit("/split right");
  expect(panes().panes).toHaveLength(2);
  expect(panes().axis).toBe("right");
  // The session keeps the pane it is in: dividing the workspace is not leaving it.
  expect(panes().panes[0].title).toBe("session");

  await from("/split 4");
  expect(panes().panes).toHaveLength(4);

  await from("/close");
  expect(panes().panes).toHaveLength(3);
});

it("says what /split takes and leaves the workspace alone", async () => {
  await submit("/split sideways");
  expect(text()).toContain("'/split' takes right, down, 3 or 4");
  expect(panes().panes).toHaveLength(1);
});

it("sends a screen to a pane and keeps sending it to the same one", async () => {
  await submit("/graph split");
  expect(panes().panes.map((pane: { title: string }) => pane.title)).toEqual(["session", "/graph"]);
  expect(tree.root.findByType(GraphScreen).props.chrome).toBe("pane");

  await from("/env split");
  expect(panes().panes).toHaveLength(3);

  // The second `/graph split` lands in the pane the first one went to.
  await from("/graph split");
  expect(panes().panes).toHaveLength(3);
  expect(panes().focused).toBe("p2");
});

it("opens a screen over the whole workspace when it is not sent to a pane", async () => {
  await submit("/split right");
  await from("/graph");
  // Over the panes, in full chrome, and `esc` brings the panes back rather than the bare session.
  expect(tree.root.findByType(GraphScreen).props.chrome).toBe("full");
  expect(tree.root.findByProps({ className: "surface-workspace" }).props.hidden).toBe(true);
  await act(async () => tree.root.findByType(GraphScreen).props.onClose());
  expect(panes().panes).toHaveLength(2);
});

it("closes the screen in a pane and leaves the pane standing", async () => {
  await submit("/graph split");
  expect(panes().panes[1].shows).toEqual({ screen: "graph" });
  await act(async () => tree.root.findByType(GraphScreen).props.onClose());
  expect(panes().panes).toHaveLength(2);
  expect(panes().panes[1].shows).toBeUndefined();
  expect(panes().panes[1].title).toBe("session");
});

it("gives a second session pane its own prompt and its own scrollback", async () => {
  await submit(":calc 1"); await accept("one");
  await from("/split right");

  // Two prompts, and the session's own cell is in the session's own pane only.
  expect(prompts()).toHaveLength(2);
  const [mine, theirs] = tree.root.findAllByType(Session);
  expect(mine!.props.model.cells).toHaveLength(1);
  expect(theirs!.props.model.cells).toEqual([]);

  // A command typed in the second pane is that pane's, and stays out of the first.
  await fromPane(1, ":calc 2");
  const sent = last();
  await emit(created("two"), { event: "planned", cell: sent.cell, text: sent.text, nodes: ["two"] });
  const [first, second] = tree.root.findAllByType(Session);
  expect(second!.props.model.cells).toHaveLength(1);
  expect(first!.props.model.cells).toHaveLength(1);
  expect(lineText(second!.props.model.cells[0].rows[0].segments)).toContain(":calc 2");
  expect(lineText(first!.props.model.cells[0].rows[0].segments)).toContain(":calc 1");
});

it("hands a pane's screen command up to the workspace", async () => {
  await submit("/split right");
  // A second session cannot summon a screen for itself: screens belong to the workspace.
  await fromPane(1, "/graph split");
  expect(panes().panes.map((pane: { title: string }) => pane.title))
    .toEqual(["session", "session", "/graph"]);
});


it("badges the named result and sends its one view chip to the result's own HTTP tab", async () => {
  const stored = httpValue("synthetic full body");
  fetchValue.mockImplementation(async () => Response.json(stored));
  await emit(created("http-node", { name: "response", command: 'http request url:"https://example.test"' }),
    { event: "planned", cell: "http-cell", text: 'http request url:"https://example.test" > response', nodes: ["http-node"], restored: true },
    { event: "node", constructionComplete: false, node: "http-node", state: "ready" } as Event, ready("http-node", "http-result"));
  expect(tree.root.findAllByType("span").find(span=>span.props["data-variable-name"] === "$response")?.props["data-variable-name"]).toBe("$response");
  // One offer, named after where it goes — `w http` in the keys theme — and the cell's own output
  // is not what it changes.
  expect(tree.root.findAllByProps({"aria-label":"w http"})).toHaveLength(1);
  await act(async () => tree.root.findAllByType("section").find(node => node.props["data-cell"] === "http-cell")!.props.onKeyDown({
    key: "w", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, preventDefault() {}, stopPropagation() {},
  }));
  expect(tree.root.findByType(OpenScreen).props.tab).toBe("http");
  expect(tree.root.findByType(OpenScreen).props.value).toEqual(stored);
  expect(text()).toContain("synthetic full body");
  await act(async () => tree.root.findByType(OpenScreen).props.onClose());
  await submit("/open response");
  expect(tree.root.findByType(OpenScreen).props.value).toEqual(stored);
  expect(tree.root.findByType(OpenScreen).findAllByProps({ "aria-label": "HTTP response" })).toHaveLength(1);
  await act(async () => tree.root.findByType(OpenScreen).props.onTab("http"));
  expect(tree.root.findByType(OpenScreen).findAllByProps({ "aria-label": "HTTP response" })).toHaveLength(1);
  expect(text()).toContain("synthetic full body");
  expect(submits()).toHaveLength(0);
  expect(fetchValue).toHaveBeenCalledTimes(1);
});

it("reaches a single-line annotated failed command's source without advertising or editing it", async () => {
  const source = '@env{pg-demo} :calc { return call("pg", ["run"], {}); }';
  await emit(created("failed-node", { command: source }),
    { event: "planned", cell: "failed-cell", text: source, nodes: ["failed-node"], restored: true },
    { event: "node", constructionComplete: false, node: "failed-node", state: "failed" } as Event);
  await type(":calc 123");
  // The row shows this command whole, so nothing points at a second place to read it.
  expect(tree.root.findAllByType("button").map(node => node.children.join("")).filter(label => label.startsWith("⇄")))
    .toEqual([]);
  await act(async () => tree.root.findAllByProps({ className: "cell-source" })[0]!.props.onClick({
    metaKey: true, button: 0, preventDefault() {}, stopPropagation() {},
  }));
  // The plain window formats the calc body; the annotation before it is left as written.
  const formatted = '@env{pg-demo} :calc {\n  return call("pg", ["run"], {});\n}';
  expect(tree.root.findByType(SourceView).props.source).toBe(formatted);
  const field = tree.root.findAllByType("textarea").find(node => node.props.readOnly)!;
  expect(field.props.value).toBe(formatted);
  expect(tree.root.findAllByType(EditScreen)).toHaveLength(0);
  await act(async () => tree.root.findByType(PeekScreen).props.onClose());
  expect(tree.root.findByType(Prompt).props.draft).toBe(":calc 123");
  expect(submits()).toHaveLength(0);
});

it("uses complete HTTP decoding in a separate result window", async () => {
  act(() => tree.unmount());
  fetchValue.mockImplementation(async () => Response.json(httpValue("window body")));
  await act(async () => { tree = create(<OpenWindow route={{ node: "http-node", tab: "json" }} />); });
  await emit({ event: "session", generation: "window-g1" }, created("http-node"), ready("http-node", "window-result"));
  expect(tree.root.findByType(OpenScreen).props.value).toEqual(httpValue("window body"));
  expect(tree.root.findAllByType("pre").some(pre => pre.children.join("").includes('"body": "d2luZG93IGJvZHk="'))).toBe(true);
  expect(submits()).toHaveLength(0);
});

/** `Mod` is Cmd only where the platform says Mac; Node names the real host, so these tests name theirs. */
const onMac = () => vi.stubGlobal("navigator", { platform: "MacIntel" });

it("returns unrun text and resumes the exact multiline draft with Cmd+Shift+Enter", async () => {
  onMac();
  await type(":calc 1");
  await act(async () => tree.root.findByType(Prompt).props.onGrow(":calc 1"));
  const source = ':calc {\n\tconst message = "two  spaces";\n\n  return message;\n} > saved\n';
  await act(async () => tree.root.findByType(EditScreen).props.onChange(source));
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  expect(tree.root.findByType(Prompt).props.draft).toBe(source);
  expect(tree.root.findByType(Prompt).findByType("textarea").props.value).toBe(source);
  const grow = async () => act(async () => tree.root.findByType(Prompt).findByType("textarea").props.onKeyDown({
    key: "Enter", shiftKey: true, metaKey: true, preventDefault() {}, stopPropagation() {},
  }));
  await grow();
  expect(tree.root.findByType(EditScreen).props.source).toBe(source);
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  const changed = source + "// continued from prompt";
  await type(changed); await grow();
  expect(tree.root.findByType(EditScreen).props.source).toBe(changed);
  expect(submits()).toHaveLength(0);
});

it("returns an editor opened directly by /edit through the outer Escape path", async () => {
  await submit("/edit");
  const source = ':calc { return "not run"; }';
  await act(async () => tree.root.findByType(EditScreen).props.onChange(source));
  await act(async () => tree.root.findByProps({ className: "wes-terminal surface-terminal surface-app" }).props.onKeyDown({
    key: "Escape", preventDefault() {},
  }));
  expect(tree.root.findByType(Prompt).props.draft).toBe(source);
  await submit("/edit");
  expect(tree.root.findByType(EditScreen).props.source).toBe(source);
  expect(submits()).toHaveLength(0);
});

it("does not resurrect an older editor draft after deliberately erasing it", async () => {
  await type(":calc 100");
  await act(async () => tree.root.findByType(Prompt).props.onGrow(":calc 100"));
  await act(async () => tree.root.findByType(EditScreen).props.onChange(""));
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  expect(tree.root.findByType(Prompt).props.draft).toBe("");
  await act(async () => tree.root.findByType(Prompt).props.onGrow(""));
  expect(tree.root.findByType(EditScreen).props.source).toBe("");
  expect(submits()).toHaveLength(0);
});

it("preserves the captured environment when the returned draft is submitted from the prompt", async () => {
  await submit("/edit");
  await act(async () => tree.root.findByType(EditScreen).props.onChange(":calc 7"));
  await environment("PROD");
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  await act(async () => tree.root.findByType(Prompt).props.onSubmit(":calc 7"));
  expect(last().environments?.selected).toBe("DEV");
  expect(submits()).toHaveLength(1);
});

it("keeps a returned draft stale across editor round trips until explicit workspace review", async () => {
  await type(":calc 7");
  await act(async () => tree.root.findByType(Prompt).props.onGrow(":calc 7"));
  await emit({ event: "session", generation: "g2" }); await environment("PROD");
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  expect(tree.root.findByType(Prompt).props.draft).toBe(":calc 7");
  await act(async () => tree.root.findByType(Prompt).props.onSubmit(":calc 7"));
  expect(submits()).toHaveLength(0);
  await act(async () => tree.root.findByType(Prompt).props.onGrow(":calc 7"));
  await act(async () => tree.root.findByType(EditScreen).props.onRun(":calc 7"));
  expect(submits()).toHaveLength(0);
  await act(async () => button("use selected context for this draft").props.onClick());
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  await act(async () => tree.root.findByType(Prompt).props.onSubmit(":calc 7"));
  expect(last().environments?.selected).toBe("PROD");
  expect(submits()).toHaveLength(1);
});

it("badges automatic ids alongside explicit names in pipeline order after replay", async () => {
  await emit(created("node1000", { name: null }), created("node1001", { name: "total" }), created("node1002", { name: "" }),
    { event: "planned", cell: "pipeline", text: ":calc 1 | :calc 2 > total | :calc 3", nodes: ["node1000", "node1001", "node1002"], restored: true },
    { event: "planned", cell: "no-result", text: ":calc invalid", nodes: [], restored: true });
  const labels = (cell: { rows: { nodes: { label: string }[] }[] }) => cell.rows.flatMap((row) => row.nodes.map((node) => node.label));
  expect(labels(session().props.model.cells[0])).toEqual(["node1000", "$total", "node1002"]);
  expect(labels(session().props.model.cells[1])).toEqual([]);
  expect(submits()).toHaveLength(0);
});


it("still submits the returned draft with plain Cmd+Enter", async () => {
  onMac();
  await submit("/edit");
  await act(async () => tree.root.findByType(EditScreen).props.onChange(":calc 8"));
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  await act(async () => tree.root.findByType(Prompt).findByType("textarea").props.onKeyDown({
    key: "Enter", metaKey: true, shiftKey: false, preventDefault() {}, stopPropagation() {},
  }));
  expect(submits()).toHaveLength(1);
  expect(last().text).toBe(":calc 8");
  expect(tree.root.findAllByType(EditScreen)).toHaveLength(0);
});


it("splits the command's source pane, keeps ordinary focus, and focuses x variants", async () => {
  await from("/rsplit");
  expect(panes().focused).toBe("p1");
  expect(prompts()[1]!.props.autoFocus).toBe(false);
  await fromPane(1, "/bsplitx");
  expect(panes().focused).toBe("p3");
  expect(panes().layout).toMatchObject({ axis: "right", first: { pane: "p1" }, second: { axis: "down", first: { pane: "p2" }, second: { pane: "p3" } } });
  await from("/tsplit");
  const before = panes();
  await from("/lsplitx");
  expect(panes()).toBe(before);
  expect(text()).toContain("The pane budget is full");
});
it("opens terminal and screen descriptors without stealing focus, and accepts commands in those panes", async () => {
  await from("/rsplit xterm");
  expect(panes().focused).toBe("p1");
  expect(tree.root.findByType(ShellTerminal).props).toMatchObject({ autoStart: true, focused: false, closeOnUnmount: true });
  expect(tree.root.findAllByType(PaneCommand)).toHaveLength(0);
  await act(async () => tree.root.findByType(ShellTerminal).props.onCommand("/bsplitx /graph"));
  expect(panes().focused).toBe("p3");
  expect(tree.root.findByType(GraphScreen).props.chrome).toBe("pane");
  const before = panes();
  await from("/lsplit /open $missing");
  expect(panes()).toBe(before);
});
it("restores layout and focus from saved settings after unmount, preserving secondary drafts while splitting", async () => {
  const saved = new Map<string, string>();
  window.localStorage.setItem = (key, value) => { saved.set(key, value); };
  window.localStorage.getItem = key => saved.get(key) ?? null;
  await from("/rsplitx");
  await act(async () => prompts()[1]!.props.onDraft("unfinished secondary draft"));
  await from("/bsplit");
  expect(prompts()[1]!.props.draft).toBe("unfinished secondary draft");
  const layout = panes();
  await act(async () => tree.unmount());
  await act(async () => { tree = create(<SurfaceApp />); });
  expect(panes()).toEqual(layout);
  await fromPane(1, "/close");
  expect(panes().panes.map((p: {id:string}) => p.id)).toEqual(["p1", "p3"]);
});

it("keeps the primary draft when a secondary pane splits or closes, including the single survivor", async () => {
  await from("/rsplit");
  await act(async () => prompts()[0]!.props.onDraft("unfinished primary draft"));
  await fromPane(1, "/bsplit");
  expect(prompts()[0]!.props.draft).toBe("unfinished primary draft");
  await fromPane(1, "/close");
  expect(prompts()[0]!.props.draft).toBe("unfinished primary draft");
  await fromPane(1, "/close");
  expect(prompts()).toHaveLength(1);
  expect(prompts()[0]!.props.draft).toBe("unfinished primary draft");
});

it("routes wesx to its source, preserves the last wes pane, and closes the shell independently", async () => {
  vi.spyOn(Engine.prototype, "terminal").mockResolvedValue({ forgotten: true, target: null });
  await from("/rsplit xterm");
  const shell = () => tree.root.findByType(ShellTerminal).props.onCommand;
  expect(panes().focused).toBe("p1");
  await act(async () => shell()("/bsplit"));
  expect(panes().layout).toMatchObject({ first: { pane: "p1" }, second: { axis: "down", first: { pane: "p2" }, second: { pane: "p3" } } });
  const before = panes();
  for (const text of ["lab-sensors readings", "/theme", "/split right ../invalid", "/close unexpected", "/rsplit open $missing"]) {
    await expect(shell()(text)).rejects.toThrow();
    expect(panes()).toBe(before);
  }
  await act(async () => shell()("/lsplit"));
  await expect(shell()("/rsplit")).rejects.toThrow("The pane budget is full");
  await act(async () => shell()("/close"));
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(0);
  await from("/rsplit xterm");
  // Attempt every session close: one wes context must survive beside the shell.
  for (const pane of [...panes().panes].filter((p: { terminal?: boolean }) => !p.terminal)) {
    await act(async () => tree.root.findByType(Split).props.onChange(
      closePane(panes(), pane.id)));
  }
  expect(panes().panes.filter((p: { terminal?: boolean }) => !p.terminal)).toHaveLength(1);
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(1);
  expect(prompts()).toHaveLength(1);
  await act(async () => shell()("/close"));
  expect(panes().panes).toHaveLength(1);
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(0);
  expect(prompts()).toHaveLength(1);
});

it("keeps persistence feedback in one independent live region without replacing panes or their drafts", async () => {
  await from("/rsplit xterm");
  await act(async () => prompts()[0]!.props.onDraft("unfinished draft"));
  const layout = panes();
  const terminal = tree.root.findByType(ShellTerminal);
  const notice = tree.root.findByProps({ className: "surface-save-status" });
  const sessionProps = tree.root.findByType(Session).props;
  for (const state of [{ saving: true }, { saving: false, error: "UI preferences could not be saved." }, { saving: true }, { saving: false }]) {
    await act(async () => { preferenceFixture.state = state; preferenceFixture.listeners.forEach(listener => listener()); });
    expect(tree.root.findByProps({ className: "surface-save-status" })).toBe(notice);
    expect(notice.props.role).toBe("status");
    expect(notice.props["aria-live"]).toBe("polite");
    expect(notice.children.join("")).toBe(state.error ?? (state.saving ? "Saving layout and preferences…" : ""));
    expect(notice.props["data-error"]).toBe(state.error ? "true" : undefined);
    expect(panes()).toBe(layout);
    expect(tree.root.findByType(Session).props).toBe(sessionProps);
    expect(tree.root.findByType(ShellTerminal)).toBe(terminal);
    expect(prompts()[0]!.props.draft).toBe("unfinished draft");
  }
});


it.each(["env", "types"])("returns cell edit and prompt growth to calc after visiting /edit %s", async context => {
  await submit(":calc 1");
  await emit(created("edit-regression"), ready("edit-regression", "h-edit"));
  await submit(`/edit ${context} sample`);
  await act(async () => tree.root.findByType(EditFileScreen).props.onClose());
  await act(async () => tree.root.findByType(Cell).props.actions.edit());
  expect(tree.root.findAllByType(EditFileScreen)).toHaveLength(0);
  expect(tree.root.findByType(EditScreen).props.source).toBe(":calc 1");
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  await submit(`/edit ${context} sample`);
  await act(async () => tree.root.findByType(EditFileScreen).props.onClose());
  await act(async () => tree.root.findByType(Prompt).props.onGrow(":calc 2"));
  expect(tree.root.findAllByType(EditFileScreen)).toHaveLength(0);
  expect(tree.root.findByType(EditScreen).props.source).toBe(":calc 2");
});


it("opens YAML in a split pane and replaces its named context instead of mounting the calc pane editor", async () => {
  await submit("/edit types old split");
  expect(tree.root.findByType(EditFileScreen).props).toMatchObject({ context: "types", initialName: "old", chrome: "pane" });
  await submit("/edit env next split");
  expect(tree.root.findByType(EditFileScreen).props).toMatchObject({ context: "env", initialName: "next", chrome: "pane" });
  expect(submits()).toHaveLength(0);
});

it("closing YAML through outer Escape does not restore an old calc draft", async () => {
  await submit("/edit");
  await act(async () => tree.root.findByType(EditScreen).props.onChange(":calc 99"));
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  await submit("/edit types");
  await act(async () => tree.root.findByProps({ className: "wes-terminal surface-terminal surface-app" }).props.onKeyDown({ key: "Escape", preventDefault() {} }));
  expect(tree.root.findByType(Prompt).props.draft).toBe("");
});

it("defines and lists the actual alias recipe, previews without executing, and records expanded source", async () => {
  const recipe = JSON.parse(readFileSync(new URL("../../../examples/aliases/recipe.json", import.meta.url), "utf8"));
  for (const definition of recipe.define) await submit(definition);
  expect(submits()).toHaveLength(0);
  await submit("/alias");
  const listing = lineText(tree.root.findByProps({ "aria-label": "Personal aliases" }).findByType(MonoLine).props.segments);
  expect(listing).toContain("twice = :calc");
  expect(listing).toContain("nodes = :list nodes");
  for (const { input, expanded } of recipe.cases) {
    const before = submits().length;
    await type(input);
    expect(submits()).toHaveLength(before);
    expect(lineText(tree.root.findByProps({ "aria-label": "Alias expansion" }).findByType(MonoLine).props.segments)).toBe(`alias → ${expanded}`);
    await act(async () => prompts()[0]!.props.onSubmit(input));
    expect(last().text).toBe(expanded);
    expect(prompts()[0]!.props.history.at(-1)).toBe(expanded);
  }
  await submit("/alias twice = :calc 99");
  expect(prompts()[0]!.props.history[0]).toBe(recipe.cases[0].expanded);
  await submit("/unalias twice");
  expect(prompts()[0]!.props.aliases).not.toHaveProperty("twice");
  await submit("/alias");
  expect(text()).toContain("nodes = :list nodes");
});

it("manages shared personal aliases from a secondary prompt and preserves invalid input there", async () => {
  await from("/split right");
  await fromPane(1, '/alias say = :calc { return "_"; }');
  expect(prompts().map(prompt => prompt.props.aliases)).toEqual([
    { say: ':calc { return "_"; }' }, { say: ':calc { return "_"; }' },
  ]);
  expect(submits()).toHaveLength(0);
  const invalid = "/alias broken = /settings";
  await fromPane(1, invalid);
  expect(prompts()[1]!.props.draft).toBe(invalid);
  expect(text()).toContain("not a client");
  const argument = '"hello"\\path\n:calc 99';
  await fromPane(1, `say ${argument}`);
  expect(last().text).toBe(`:calc { return ${JSON.stringify(argument)}; }`);
  expect(prompts()[1]!.props.history).toEqual([last().text]);
  await fromPane(1, "/unalias say");
  expect(prompts()[0]!.props.aliases).toEqual({});
  await fromPane(1, "/alias");
  expect(text()).toContain("No aliases.");
  expect(submits()).toHaveLength(1);
});

it("refuses invalid aliases and changed catalogue collisions without executing or losing the draft", async () => {
  for (const definition of ["/alias broken", "/alias one = one", '/alias one = :calc "_ _"']) {
    await submit(definition);
    expect(prompts()[0]!.props.draft).toBe(definition);
    expect(prompts()[0]!.props.aliases).toEqual({});
  }
  await submit("/alias fetch = :calc 1");
  await emit({ event: "vocabulary", commands: [], annotations: [], providers: [],
    templates: [{ name: "fetch", body: ":calc 2", parameters: [] }] });
  await submit("fetch");
  expect(prompts()[0]!.props.draft).toBe("fetch");
  expect(text()).toContain("conflicts with a provider or command template");
  expect(submits()).toHaveLength(0);
  await submit("/unalias fetch");
  expect(prompts()[0]!.props.aliases).toEqual({});
});

it("retains aliases across workspace replacement and restores browser preferences without losing other settings", async () => {
  const saved = new Map<string, string>();
  window.localStorage.setItem = (key, value) => { saved.set(key, value); };
  window.localStorage.getItem = key => saved.get(key) ?? null;
  await submit("/theme white");
  await submit("/alias twice = :calc { return 2 * (_); }");
  await emit({ event: "session", generation: "other-workspace" });
  expect(prompts()[0]!.props.aliases).toEqual({ twice: ":calc { return 2 * (_); }" });
  act(() => tree.unmount());
  await act(async () => { tree = create(<SurfaceApp />); });
  await emit({ event: "session", generation: "reopened" });
  await environment("DEV");
  expect(prompts()[0]!.props.aliases).toEqual({ twice: ":calc { return 2 * (_); }" });
  expect(tree.root.findByProps({ className: "wes-terminal surface-terminal surface-app" }).props["data-palette"]).toBe("white");
  await submit("twice 21");
  expect(last().text).toBe(":calc { return 2 * (21); }");
});

it("pins and unpins primary and secondary cells by button and p without executing", async () => {
  await submit(":calc 1"); await accept("one");
  await from("/theme cell controls");
  await from("/split right");
  await fromPane(1, ":calc 2"); await accept("two");
  const count = submits().length;
  for (const at of [0, 1]) {
    const current = () => tree.root.findAllByType(Session)[at]!;
    await act(async () => current().findByProps({ "aria-label": "p pin" }).props.onClick());
    expect(current().findByProps({ "aria-label": "Pinned cells" }).findAllByType(Cell)).toHaveLength(1);
    expect(current().findByProps({ role: "log" }).findAllByType(Cell)).toHaveLength(0);
    await act(async () => current().findAllByType("section").find(node=>node.props["data-cell"])!.props.onKeyDown({ key: "p", preventDefault() {}, stopPropagation() {} }));
    expect(current().findAllByProps({ "aria-label": "Pinned cells" })).toHaveLength(0);
    expect(current().findByProps({ role: "log" }).findAllByType(Cell)).toHaveLength(1);
  }
  expect(submits()).toHaveLength(count);
});

it("preserves interactive drafts and pending delivery across pin moves without resending", async () => {
  await submit("@interactive fixture ask"); await accept("one", { interactive: true });
  await emit({ event: "node", constructionComplete: false, node: "one", state: "running" } as Event,
    { event: "conversation", node: "one", run: "run-a", active: true });
  const preview = () => tree.root.findByType(InteractivePreview);
  const pin = async () => act(async () => session().findByType(Cell).props.actions.pin());
  await act(async () => preview().props.onAnswer("unsent fixture"));
  await pin();
  expect(preview().props.answer).toBe("unsent fixture");
  let reject!: (error: Error) => void;
  send.mockImplementationOnce(() => new Promise((_resolve, fail) => { reject = fail; }));
  await act(async () => preview().props.onSend());
  expect(preview().props.disabled).toBe(true);
  await pin();
  expect(preview().props.disabled).toBe(true);
  await act(async () => preview().props.onSend());
  expect(requests.filter(request => request.request === "input")).toHaveLength(1);
  await act(async () => reject(new Error("synthetic disconnected")));
  expect(preview().props.disabled).toBe(false);
  expect(text()).toContain("input delivery is unconfirmed");
  await pin();
  expect(text()).toContain("input delivery is unconfirmed");
  await act(async () => preview().props.onAnswer("run-a only"));
  await emit({ event: "conversation", node: "one", run: "run-b", active: true });
  expect(preview().props.answer).toBe("");
  expect(text()).not.toContain("input delivery is unconfirmed");
  expect(submits()).toHaveLength(1);
});

it("clears only the invoking viewport while retaining cells, pinned results and command history", async () => {
  await submit(":calc 1"); await accept("one");
  await submit(":calc 2"); await accept("two");
  const first = session().props.model.cells[0];
  await act(async () => session().props.actions(first).pin());
  const before = session().props.model.cells;
  const count = requests.length;
  await submit("/clear");
  expect(session().props.model.cells).toEqual(before);
  expect(session().props.clearRequest).toEqual({ after: before[1].id, revision: 1 });
  expect(tree.root.findByType(Prompt).props.history).toEqual([":calc 1", ":calc 2"]);
  expect(requests).toHaveLength(count);
  await submit("/clear");
  expect(session().props.clearRequest.revision).toBe(2);
  await from("/split right");
  await fromPane(1, "/clear");
  const sessions = tree.root.findAllByType(Session);
  expect(sessions[0]!.props.clearRequest).toEqual({ after: before[1].id, revision: 2 });
  expect(sessions[1]!.props.clearRequest).toEqual({ after: undefined, revision: 1 });
  expect(sessions[1]!.props.model.cells).toEqual([]);
  await emit({ event: "session", generation: "new" });
  expect(tree.root.findAllByType(Session).every(item => item.props.clearRequest === undefined)).toBe(true);
});

it("requests read-only storage for /debug and renders the reply without an engine submission", async () => {
  await submit("/debug");
  expect(submits()).toEqual([]);
  expect(requests.filter(request => request.request === "storage")).toHaveLength(1);
  expect(text()).toContain("waiting for current engine storage");
  expect(text()).toContain("localStorage wes.settings");
  expect(text()).not.toContain("wes.cells");
  await emit({ event: "storage", workspace: "fixture", places: [{ name: "archive", where: "/synthetic/home/archive", holds: "saved values", durable: true, files: 2, bytes: 4096 }] });
  expect(text()).toContain("storage received");
  expect(text()).toContain("/synthetic/home/archive");
  expect(text()).toContain("2 files, 4.0 KB");
  expect(text()).toContain("pin/collapse state and clear position: memory");
  const debug = session().props.model.cells[0];
  await act(async () => session().props.actions(debug).repeat(false));
  expect(submits()).toEqual([]);
  expect(requests.filter(request => request.request === "storage")).toHaveLength(2);
});

it("keeps a secondary session's debug report local and handles a refused storage request", async () => {
  await from("/split right");
  send.mockImplementation(async request => request.request === "storage" ? new Response("unavailable", { status: 503 }) : ok());
  await fromPane(1, "/debug");
  expect(tree.root.findAllByType(Session).map(item => item.props.model.cells.length)).toEqual([0, 1]);
  expect(text()).toContain("storage unavailable");
  expect(submits()).toEqual([]);
  const secondary = tree.root.findAllByType(Session)[1]!;
  await act(async () => secondary.props.actions(secondary.props.model.cells[0]).repeat(false));
  expect(submits()).toEqual([]);
});

it("reports the selected desktop home without disclosing alias values or claiming browser arrangement storage", async () => {
  window.__WES_DESKTOP__ = true;
  vi.stubGlobal("fetch", vi.fn(async () => Response.json({ path: "/synthetic/custom-home", identity: "fixture" })));
  await submit("/debug");
  expect(text()).toContain("/synthetic/custom-home/desktop-ui.json");
  expect(text()).toContain("client        desktop app");
  expect(text()).not.toContain("localStorage");
  expect(text()).not.toContain("wes.cells");
});

it("rejects session-only commands in screen command fields without clearing any session", async () => {
  await submit("/graph split");
  await act(async () => tree.root.findByType(PaneCommand).props.onCommand("/clear"));
  expect(text()).toContain("Use a session prompt for /clear");
  expect(session().props.clearRequest).toBeUndefined();
  await act(async () => tree.root.findByType(PaneCommand).props.onCommand("/debug"));
  expect(requests.some(request => request.request === "storage")).toBe(false);
  expect(submits()).toEqual([]);
});


it.each([0, 1])("submits and recalls exact ordinary source in session pane %s", async index => {
  await from("/rsplit");
  const source = "\n\t:calc {\n\n  return [1, 2];\n}\n\n";
  await fromPane(index, source);
  expect(last().text).toBe(source);
  expect(prompts()[index]!.props.history).toEqual([source]);
  expect(tree.root.findAllByType(Session)[index]!.props.model.cells[0].source).toBe(source);
});

it("returns the editor to its originating secondary pane with exact source and captured context", async () => {
  await from("/rsplit");
  await act(async () => prompts()[0]!.props.onDraft("primary draft"));
  const source = "\n\t:calc {\n\n  return 7;\n}\n";
  await act(async () => prompts()[1]!.props.onDraft(source));
  await environment("PROD");
  await act(async () => prompts()[1]!.props.onGrow(source));
  expect(tree.root.findByType(EditScreen).props.source).toBe(source);
  const edited = source + "// continued\n";
  await act(async () => tree.root.findByType(EditScreen).props.onChange(edited));
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  expect(prompts()[0]!.props.draft).toBe("primary draft");
  expect(prompts()[1]!.props.draft).toBe(edited);
  expect(panes().focused).toBe("p2");
  await act(async () => prompts()[1]!.props.onSubmit(edited));
  expect(last().text).toBe(edited);
  expect(last().environments?.selected).toBe("DEV");
  expect(prompts()[1]!.props.history).toEqual([edited]);
});

it("requires explicit context review after a secondary editor draft crosses a workspace change", async () => {
  await from("/rsplit");
  await act(async () => prompts()[1]!.props.onDraft(":calc 9"));
  await act(async () => prompts()[1]!.props.onGrow(":calc 9"));
  await emit({ event: "session", generation: "g2" }); await environment("PROD");
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  await act(async () => prompts()[1]!.props.onSubmit(":calc 9"));
  expect(submits()).toHaveLength(0);
  await act(async () => button("use selected context for this draft").props.onClick());
  await act(async () => prompts()[1]!.props.onSubmit(":calc 9"));
  expect(last().environments?.selected).toBe("PROD");
});


it.each([0, 1])("returns actual editor indentation unchanged to pane %s, then permits plain typing", async index => {
  const raw = ':calc {\nreturn [1,\n2];\n}';
  let state = EditorState.create({ doc: raw, selection: { anchor: 0, head: raw.length },
    extensions: calcExtensions({ language: readLanguage(bundledPackage, "engine"), names: [],
      onChange() {}, onRun() {}, onRunAgain() {}, onLeave() {} }),
  });
  expect(indentSelection({ state, dispatch: (transaction: Transaction) => { state = transaction.state; } })).toBe(true);
  const formatted = state.doc.toString();
  expect(formatted).not.toBe(raw);
  expect(formatted).toContain("\n  return");
  await from("/rsplit");
  await act(async () => prompts()[index]!.props.onGrow(raw));
  await act(async () => tree.root.findByType(EditScreen).props.onChange(formatted));
  await act(async () => tree.root.findByType(EditScreen).props.onClose());
  expect(prompts()[index]!.findByType("textarea").props.value).toBe(formatted);
  const continued = formatted + "\n\t({";
  await act(async () => prompts()[index]!.props.onDraft(continued));
  expect(prompts()[index]!.findByType("textarea").props.value).toBe(continued);
  expect(submits()).toHaveLength(0);
});


it.each([
  ["/edit view", "/edit"],
  ["/open view", "/open"],
  ["/rsplit /edit view", "/edit"],
  ["/rsplit /open view", "/open"],
])("attributes a missing result in %s to %s", async (command, screen) => {
  await emit(created("known", { name: "orders" }));
  await from(command);
  expect(text()).toContain(`'${screen}' knows no result called view · names: $orders`);
  if (screen === "/edit") expect(text()).not.toContain("'/open' knows no result");
  expect(panes().panes).toHaveLength(1);
  expect(tree.root.findAllByType(EditScreen)).toHaveLength(0);
  expect(tree.root.findAllByType(OpenScreen)).toHaveLength(0);
  expect(submits()).toHaveLength(0);
});


it("persists terminal history identity with the open layout and retains it across application remount", async () => {
  const saved = new Map<string, string>();
  window.localStorage.setItem = (key, value) => { saved.set(key, value); };
  window.localStorage.getItem = key => saved.get(key) ?? null;
  const terminal = vi.spyOn(Engine.prototype, "terminal").mockResolvedValue({ forgotten: true, target: null });
  await from("/rsplit xterm");
  terminal.mockClear();
  const history = tree.root.findByType(ShellTerminal).props.history;
  expect(history).toMatch(/^[a-f0-9-]{36}$/);
  await act(async () => tree.root.findByType(ShellTerminal).props.beforeStart());
  expect(JSON.parse(saved.get("wes.settings")!).paneLayout.panes[1].history).toBe(history);
  await act(async () => tree.unmount());
  expect(terminal).not.toHaveBeenCalled();
  await act(async () => { tree = create(<SurfaceApp />); });
  await emit({ event: "session", generation: "g2" });
  expect(tree.root.findByType(ShellTerminal).props.history).toBe(history);
  expect(terminal).not.toHaveBeenCalled();
});
it("retains a terminal pane on failed forget and removes it only after an explicit successful retry", async () => {
  await from("/rsplit xterm");
  const terminal = vi.spyOn(Engine.prototype, "terminal").mockClear().mockRejectedValueOnce(new Error("forget acknowledgement lost"))
    .mockResolvedValue({ forgotten: true, target: null });
  const history = tree.root.findByType(ShellTerminal).props.history;
  await act(async () => { await expect(tree.root.findByType(ShellTerminal).props.onCommand("/close")).rejects.toThrow("acknowledgement lost"); });
  expect(tree.root.findByType(ShellTerminal).props.history).toBe(history);
  await act(async () => tree.root.findByType(ShellTerminal).props.onCommand("/close"));
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(0);
  expect(terminal.mock.calls.map(([request]) => request)).toEqual([{ action: "forget", history }, { action: "forget", history }]);
});
it("forgets a terminal removed by the split close control and leaves unrelated terminal history alone", async () => {
  const terminal = vi.spyOn(Engine.prototype, "terminal").mockResolvedValue({ forgotten: true, target: null });
  await from("/rsplit xterm"); await from("/bsplit xterm");
  terminal.mockClear();
  const [first, second] = tree.root.findAllByType(ShellTerminal);
  const firstHistory = first!.props.history, secondHistory = second!.props.history;
  await act(async () => tree.root.findByType(Split).props.onChange(closePane(panes(), "p2")));
  expect(terminal).toHaveBeenCalledExactlyOnceWith({ action: "forget", history: firstHistory });
  expect(tree.root.findByType(ShellTerminal).props.history).toBe(secondHistory);
});


it("keeps newer prompt text when a queued pane command completes after delayed history retirement", async () => {
  let finish!: () => void;
  await from("/rsplit xterm");
  vi.spyOn(Engine.prototype, "terminal").mockImplementationOnce(() => new Promise(resolve => { finish = () => resolve({ forgotten: true, target: null }); }));
  let closing!: Promise<void>;
  await act(async () => { closing = tree.root.findByType(ShellTerminal).props.onCommand("/close"); });
  await from("/rsplit"); // Queued behind the terminal's transactional close.
  await act(async () => prompts()[0]!.props.onDraft("new draft while close was pending"));
  await act(async () => { finish(); await closing; });
  expect(prompts()[0]!.props.draft).toBe("new draft while close was pending");
  expect(panes().panes.map((pane: { id: string }) => pane.id)).toEqual(["p1", "p3"]);
});
it("does not clear a queued command draft or publish its old error after workspace replacement", async () => {
  let finish!: () => void;
  await from("/rsplit xterm");
  vi.spyOn(Engine.prototype, "terminal").mockImplementationOnce(() => new Promise(resolve => { finish = () => resolve({ forgotten: true, target: null }); }));
  let closing!: Promise<void>;
  await act(async () => { closing = tree.root.findByType(ShellTerminal).props.onCommand("/close"); });
  await from("/rsplit");
  await emit({ event: "session", generation: "replacement" });
  await act(async () => { finish(); await closing; });
  expect(prompts()[0]!.props.draft).toBe("/rsplit");
  expect(text()).not.toContain("Workspace changed; the pane change was discarded.");
});

it("opens the complete live connected component without submitting, and keeps disconnected work out", async () => {
  const definitions = [
    { id: "A", dependsOn: [] }, { id: "B", dependsOn: ["A"] },
    { id: "C", dependsOn: ["B", "X"] }, { id: "X", dependsOn: [] }, { id: "outside", dependsOn: [] },
  ];
  for (const node of definitions) await emit(created(node.id, { dependsOn: node.dependsOn }),
    { event: "planned", cell: `cell-${node.id}`, text: `:calc ${node.id}`, nodes: [node.id], restored: true });
  await from("/lsplit related $B");
  const component = () => tree.root.findByType(ComponentPane).findByType(Session);
  const ids = () => component().props.model.cells.map((cell: { id: string }) => cell.id);
  expect(ids()).toEqual(["cell-A", "cell-B", "cell-C", "cell-X"]);
  expect(submits()).toHaveLength(0);
  expect(panes().focused).toBe("p1"); expect(panes().panes[1].value).toEqual({ node: "B", generation: "g1", label: "B", related: true });
  // A consumer attached to X joins the same component even though it is not a descendant of B.
  await emit(created("Z", { dependsOn: ["X"] }), { event: "planned", cell: "cell-Z", text: ":calc $X", nodes: ["Z"] });
  expect(ids()).toEqual(["cell-A", "cell-B", "cell-C", "cell-X", "cell-Z"]);
  await emit({ event: "node", constructionComplete: false, node: "B", state: "failed", message: "synthetic failure" } as Event);
  expect(component().props.model.cells.find((cell: { id: string }) => cell.id === "cell-B").state).toBe("failed");
  expect(submits()).toHaveLength(0);
  // Reading and explicitly repeating use the real node/cell authority.
  act(() => component().props.actions(component().props.model.cells[1]).details());
  expect(tree.root.findByType(OpenScreen).props.tab).toBe("details");
  await act(async () => tree.root.findByType(OpenScreen).props.onClose());
  await act(async () => component().props.actions(component().props.model.cells[1]).repeat(true));
  expect(last()).toMatchObject({ repeat: "cell-B", text: ":calc B", acknowledge_effects: true });
  applicationLog.clear();
  send.mockResolvedValueOnce(new Response("Stream pipelines require an explicit source restart", { status: 400 }));
  await act(async () => component().props.actions(component().props.model.cells[1]).repeat(true));
  expect(applicationLog.snapshot()).toEqual([expect.objectContaining({ source: "Engine", operation: "Run again", cell: "cell-B" })]);

});

it("includes acknowledged work from another session pane and refuses unknown split references", async () => {
  await from("/split right");
  await fromPane(1, ":calc 1"); const sent = last();
  await emit(created("other", { name: "named" }), { event: "planned", cell: sent.cell, text: sent.text, nodes: ["other"] });
  await from("/rsplitx related $named");
  const projected = tree.root.findByType(ComponentPane).findByType(Session).props.model.cells;
  expect(projected.map((cell: { id: string }) => cell.id)).toEqual([sent.cell]);
  expect(panes().focused).toBe("p3"); expect(panes().panes[2].value.node).toBe("other");
  expect(submits()).toHaveLength(1);
  await from("/bsplit $missing");
  expect(panes().panes).toHaveLength(3);
  expect(text()).toContain("knows no result called missing");
  expect(submits()).toHaveLength(1);
});

it("never retargets a linked pane when the workspace generation changes and reuses a node ID", async () => {
  await emit(created("one"), { event: "planned", cell: "original", text: ":calc 1", nodes: ["one"], restored: true });
  await from("/rsplit related $one");
  await emit({ event: "session", generation: "different-workspace" }, created("one"),
    { event: "planned", cell: "replacement", text: ":calc 2", nodes: ["one"], restored: true });
  expect(tree.root.findAllByType(ComponentPane)).toHaveLength(0);
  expect(text()).toContain("previous workspace session");
  expect(submits()).toHaveLength(0);
});

it.each(["env", "types"] as const)("reopens captured %s YAML for editing and offers no generic node repeat", async context => {
  const { documentCommand } = await import("./document-command");
  const source = 'version: 1\r\npackage: "Türkçe"\n';
  const base = '/synthetic/with "quote"/C:\\folder';
  const text = documentCommand(context, "editor:captured", base, "editorPlan42");
  await emit({ event: "planned", cell: "document-cell", text, nodes: [], document: { source }, diagnostics: context === "env"
    ? [{ code: "ENV000", severity: "info", message: "DEV: added synthetic", start: 0, end: 0, hints: [] }] : [] });
  const cell = tree.root.findAllByType(Cell).find(cell => cell.props.label === "document-cell")!;
  expect(cell.props.actions.repeat).toBeUndefined();
  if (context === "env") {
    expect(lineText(cell.props.rows[0].segments)).toContain("editorPlan42");
    const notice = tree.root.findAll(node => node.props.className === "value-notice cell-document-notice")[0]!.findAllByType(MonoLine)
      .map(line => lineText(line.props.segments)).join("\n");
    expect(notice).toContain("DEV: added synthetic");
    expect(notice).toContain(":env apply $editorPlan42");
  }
  await act(async () => cell.props.actions.edit());
  let editor = tree.root.findByType(EditFileScreen);
  expect(editor.props.initialSource).toBe(source);
  expect(editor.props.context).toBe(context);
  expect(editor.props.initialBase).toBe(context === "env" ? base : "");
  expect(editor.props.initialOrigin).toBe("editor:captured");
  expect(editor.props.environmentContext.selected).toBe("DEV");
  await act(async () => editor.props.onClose());
  await act(async () => tree.root.findByType(Prompt).props.onSubmit(`/edit ${context}`));
  editor = tree.root.findByType(EditFileScreen);
  expect(editor.props.initialSource).toBeUndefined();
  expect(editor.props.initialBase).toBeUndefined();
  expect(editor.props.initialOrigin).toBeUndefined();
  expect(requests.filter(request => request.request === "submit")).toHaveLength(0);
});

it("a reopened environment document creates a fresh plan binding with the captured body", async () => {
  const text = ':env plan source:"" origin:"editor:original" base:"/synthetic" > editorPlanOriginal';
  const source = "version: 1\npackage: synthetic";
  await emit({ event: "planned", cell: "original-document", text, nodes: [], document: { source } });
  await act(async () => tree.root.findAllByType(Cell).find(cell => cell.props.label === "original-document")!.props.actions.edit());
  const screen = tree.root.findByType(EditFileScreen);
  let result!: Promise<string>;
  await act(async () => {
    result = screen.props.onRun(source, { name: "", base: screen.props.initialBase, origin: screen.props.initialOrigin, generation: "g1" });
  });
  const request = requests.find(request => request.request === "submit");
  if (request?.request !== "submit") throw new Error("missing document operation");
  expect(request.document?.source).toBe(source);
  expect(request.repeat).toBeUndefined();
  expect(request.text).toContain('origin:"editor:original" base:"/synthetic"');
  expect(request.text).not.toContain("editorPlanOriginal");
  await emit({ event: "planned", cell: request.cell, text: request.text, nodes: [], document: { source }, diagnostics: [] });
  await expect(result).resolves.toContain("Nothing applied");
});


function mockWorkspaceOpen() {
  const open = vi.fn(async (_url: string, options: RequestInit) => {
    const { name } = JSON.parse(String(options.body));
    return Response.json({ workspace: name, generation: `g-${name}`, identity: `identity-${name}` });
  });
  vi.stubGlobal("fetch", open);
  return open;
}

it.each([
  "/tab xterm", "/tabx xterm", "/split xterm", "/split right xterm", "/split down xterm",
  ...["l", "r", "t", "b"].flatMap(direction => ["", "x"].map(suffix => `/${direction}split${suffix} xterm`)),
])("opens a terminal for %s even when a workspace named xterm already exists", async command => {
  await emit({ event: "workspace-context", name: "sample-work", saved: ["sample-work", "xterm"] });
  const open = mockWorkspaceOpen();
  const api = vi.spyOn(Engine.prototype, "terminal").mockClear().mockResolvedValue({ target: null });
  const prompt = panePrompt("p1");
  await paneSubmit("p1", command);
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(1);
  expect(tree.root.findByType(ShellTerminal).props.history).toMatch(/^[a-f0-9-]{36}$/);
  expect(api).toHaveBeenCalledExactlyOnceWith({ action: "resolve", environment: "DEV" });
  expect(open).not.toHaveBeenCalled();
  expect(panePrompt("p1")).toBe(prompt);
  expect(submits()).toHaveLength(0);
  const takeFocus = command.startsWith("/tabx ") || /^\/[lrtb]splitx /.test(command);
  expect(panes().focused).toBe(takeFocus ? "p2" : "p1");
});

it("adds xterm tabs to the existing terminal without changing focus or the active tab unless requested", async () => {
  const api = vi.spyOn(Engine.prototype, "terminal").mockClear().mockResolvedValue({ target: null });
  const open = mockWorkspaceOpen();
  await paneSubmit("p1", "/tab xterm");
  const first = tree.root.findByType(ShellTerminal);
  const firstHistory = first.props.history;
  await paneSubmit("p1", "/tab xterm");
  expect(panes().panes).toHaveLength(2);
  expect(panes().focused).toBe("p1");
  expect(panes().panes[1].history).toBe(firstHistory);
  expect(tree.root.findAllByType(ShellTerminal)[0]).toBe(first);
  await paneSubmit("p1", "/tabx xterm pane:p2");
  expect(panes().panes).toHaveLength(2);
  expect(panes().focused).toBe("p2");
  const terminals = tree.root.findAllByType(ShellTerminal);
  expect(terminals).toHaveLength(3);
  expect(terminals[2]!.props.focused).toBe(true);
  expect(panes().panes[1].history).toBe(terminals[2]!.props.history);
  expect(api).toHaveBeenCalledTimes(3);
  expect(open).not.toHaveBeenCalled();
  expect(submits()).toHaveLength(0);
});

it("refuses invalid xterm tab destinations and exhausted budgets before resolving a terminal", async () => {
  const api = vi.spyOn(Engine.prototype, "terminal").mockClear().mockResolvedValue({ target: null });
  for (const command of ["/tab xterm pane:p1", "/tab xterm pane:p99", "/tab xterm env:", "/split xterm pane:p1"]) {
    await paneSubmit("p1", command);
    expect(panes().panes).toHaveLength(1);
  }
  expect(api).not.toHaveBeenCalled();
  for (let i = 0; i < 4; i++) await paneSubmit("p1", "/tab xterm");
  api.mockClear();
  const full = panes();
  await paneSubmit("p1", "/tabx xterm");
  expect(api).not.toHaveBeenCalled();
  expect(panes()).toBe(full);
  expect(text()).toContain("terminal budget");
});

it("rejects a terminal tab destination changed while resolving rather than adding to a different tab selection", async () => {
  await paneSubmit("p1", "/tab xterm");
  await paneSubmit("p1", "/tabx xterm");
  const first = tree.root.findAllByType(ShellTerminal)[0]!;
  const api = vi.spyOn(Engine.prototype, "terminal");
  let finish!: (value: unknown) => void;
  api.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  await paneSubmit("p1", "/tab xterm pane:p2");
  await act(async () => tree.root.findAllByProps({ role: "tab" }).find(tab => tab.props.id === `p2:tab:terminal:${first.props.history}`)!.props.onClick({ stopPropagation() {} }));
  const before = panes();
  await act(async () => finish({ target: null }));
  expect(panes()).toBe(before);
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(2);
  expect(text()).toContain("destination");
});

it("keeps terminal tab destinations within the issuing workspace", async () => {
  mockWorkspaceOpen();
  await paneSubmit("p1", "/tab xterm");
  const original = tree.root.findByType(ShellTerminal);
  await paneSubmit("p1", "/rsplitx other-work");
  await emitWorkspace("other-work", { event: "session", generation: "g-other-work" }, env("PROD"));
  const api = vi.spyOn(Engine.prototype, "terminal").mockClear().mockResolvedValue({ target: null });
  await paneSubmit("p3", "/tab xterm pane:p2");
  expect(api).not.toHaveBeenCalled();
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(1);
  await paneSubmit("p3", "/tabx xterm");
  expect(api).toHaveBeenCalledExactlyOnceWith({ action: "resolve", environment: "PROD" });
  expect(panes().panes[3]).toMatchObject({ id: "p4", workspace: "other-work", terminal: true });
  expect(panes().focused).toBe("p4");
  expect(tree.root.findAllByType(ShellTerminal)[0]).toBe(original);
});
async function emitWorkspace(name: string, ...events: Event[]) {
  await act(async () => {
    const source = Events.all.get(`/events?${new URLSearchParams({ workspace: name })}`);
    expect(source).toBeDefined();
    for (const event of events) source!.emit(event);
  });
}
const panePrompt = (id: string) => tree.root.findByProps({ "data-pane-id": id }).findByType(Prompt);
async function paneSubmit(id: string, text: string) {
  await act(async () => panePrompt(id).props.onSubmit(text));
}
it("opens named workspaces without remounting existing prompts or terminals and retains hidden owners", async () => {
  const open = mockWorkspaceOpen();
  await paneSubmit("p1", "/rsplit xterm");
  const shell = tree.root.findByType(ShellTerminal);
  const originalPrompt = panePrompt("p1");
  await act(async () => originalPrompt.props.onDraft("unfinished original"));
  await paneSubmit("p1", "/bsplitx second");
  await emitWorkspace("second", { event: "session", generation: "g-second" }, env("PROD"));
  expect(panePrompt("p1")).toBe(originalPrompt);
  expect(panePrompt("p1").props.draft).toBe("unfinished original");
  expect(tree.root.findByType(ShellTerminal)).toBe(shell);
  expect(tree.root.findByType(Split).props.state.focused).toBe("p3");
  expect(tree.root.findByProps({ "data-pane-id": "p3" }).props["aria-label"]).toBe("second · session");
  await paneSubmit("p3", ":calc 2 > same");
  expect(last()).toMatchObject({ text: ":calc 2 > same", environments: { selected: "PROD" } });
  const admitted = last();
  await emitWorkspace("second", created("second-node", { name: "same" }), { event: "planned", cell: admitted.cell, text: admitted.text, nodes: ["second-node"] });
  expect(tree.root.findByProps({ "data-pane-id": "p1" }).findByType(Session).props.model.cells).toHaveLength(0);
  await paneSubmit("p3", "/close");
  expect(tree.root.findAllByProps({ "data-pane-id": "p3" })).toHaveLength(0);
  await paneSubmit("p1", "/rsplit second");
  expect(Events.all.size).toBe(2);
  expect(tree.root.findByProps({ "data-pane-id": "p4" }).findByType(Session).props.model.cells).toHaveLength(1);
  expect(open.mock.calls.filter(([, options]) => JSON.parse(String(options.body)).create === false)).toHaveLength(1);
  expect(tree.root.findByType(ShellTerminal)).toBe(shell);
});

it("inherits named ownership for screens, shells and empty splits and preserves it on close", async () => {
  mockWorkspaceOpen();
  await paneSubmit("p1", "/rsplit second");
  await emitWorkspace("second", { event: "session", generation: "g-second" }, env("PROD"));
  await paneSubmit("p2", "/bsplit xterm");
  expect(tree.root.findByType(ShellTerminal).props.engine.binding).toBe("second");
  await act(async () => tree.root.findByType(ShellTerminal).props.onCommand("/lsplit /env"));
  const state = tree.root.findByType(Split).props.state;
  expect(state.panes.slice(1).map((p: { workspace: string }) => p.workspace)).toEqual(["second", "second", "second"]);
  expect(tree.root.findAllByType(EnvScreen)).toHaveLength(1);
  await act(async () => tree.root.findByType(Split).props.onChange(closePane(closePane(state, "p1"), "p4")));
  await paneSubmit("p2", "/close");
  expect(tree.root.findByType(Split).props.state.panes.map((p: { id: string }) => p.id)).toEqual(["p2", "p3"]);
});

it("leaves the layout and draft usable after failed opens and ignores late opens from replaced sessions", async () => {
  const open = mockWorkspaceOpen();
  const before = tree.root.findByType(Split).props.state;
  open.mockResolvedValueOnce(new Response("Workspace limit reached", { status: 409 }));
  await type("/rsplit missing");
  await paneSubmit("p1", "/rsplit missing");
  expect(tree.root.findByType(Split).props.state).toBe(before);
  expect(panePrompt("p1").props.draft).toBe("/rsplit missing");
  expect(text()).toContain("Workspace limit reached");
  let finish!: (response: Response) => void;
  open.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  await paneSubmit("p1", "/rsplit delayed");
  await emit({ event: "session", generation: "replacement" });
  await act(async () => finish(Response.json({ workspace: "delayed", generation: "g-delayed", identity: "identity-delayed" })));
  expect(tree.root.findByType(Split).props.state).toBe(before);
});

it("restores an explicit workspace layout without creating names or submitting work", async () => {
  const open = mockWorkspaceOpen();
  await paneSubmit("p1", "/rsplit second");
  const paneLayout = tree.root.findByType(Split).props.state;
  act(() => tree.unmount());
  Events.all.clear(); open.mockClear(); requests = [];
  window.localStorage.getItem = () => JSON.stringify({ paneLayout });
  await act(async () => { tree = create(<SurfaceApp />); });
  expect(tree.root.findByType(Split).props.state.panes[1].workspace).toBe("second");
  expect(paneLayout.workspaceBindings).toEqual({second:"identity-second"});
  expect(open).toHaveBeenCalledOnce();
  expect(JSON.parse(String(open.mock.calls[0]![1].body))).toEqual({ name: "second", create: false, identity: "identity-second" });
  expect(requests).toHaveLength(0);
});

it("opens a shared tab through the assistant bridge and preserves origin draft, terminal and execution routing", async () => {
  const open = mockWorkspaceOpen();
  await paneSubmit("p1", "/rsplit xterm");
  const shell = tree.root.findByType(ShellTerminal), engine = shell.props.engine as Engine;
  const originalPrompt = panePrompt("p1");
  await act(async () => originalPrompt.props.onDraft("unfinished user draft"));
  let reply: any;
  await act(async () => { reply = await engine.assistantEditor.handle({ id: "tab-1", action: "tab", text: JSON.stringify({ workspace: "shared", pane: "p1", activate: false }), revision: null }); });
  expect(reply.ok).toBe(true);
  expect(reply.layout.panes[0].tabs).toHaveLength(2);
  expect(tree.root.findByType(Split).props.state.panes[0].workspace).toBeUndefined();
  await emitWorkspace("shared", { event: "session", generation: "g-shared" }, env("PROD"));
  const tab = (workspace: string) => tree.root.findAllByProps({ role: "tab" }).find(node => node.children.join("") === workspace)!;
  await act(async () => tab("shared").props.onClick({ stopPropagation() {} }));
  const visible = () => tree.root.findByProps({ "data-pane-id": "p1" }).findAllByProps({ className: "workspace-tab-view" }).find(view => !view.props.hidden)!;
  await act(async () => visible().findByType(Prompt).props.onSubmit(":calc 9 > shared"));
  expect(last()).toMatchObject({ text: ":calc 9 > shared", environments: { selected: "PROD" } });
  await act(async () => tab("workspace").props.onClick({ stopPropagation() {} }));
  expect(visible().findByType(Prompt)).toBe(originalPrompt);
  expect(originalPrompt.props.draft).toBe("unfinished user draft");
  expect(tree.root.findByType(ShellTerminal)).toBe(shell);
  await act(async () => { reply = await engine.assistantEditor.handle({ id: "tab-2", action: "tab", text: JSON.stringify({ workspace: "shared", pane: "p2", activate: false }), revision: null }); });
  expect(reply.ok).toBe(false);
  expect(reply.error).toContain("session pane");
  expect(open.mock.calls.filter(([, options]) => JSON.parse(String(options.body)).name === "shared")).toHaveLength(2);
});

it("refuses a late assistant tab open when its target pane changed", async () => {
  const open = mockWorkspaceOpen();
  await paneSubmit("p1", "/rsplit xterm");
  const engine = tree.root.findByType(ShellTerminal).props.engine as Engine;
  let finish!: (response: Response) => void;
  open.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  let pending!: Promise<any>;
  await act(async () => { pending = engine.assistantEditor.handle({ id: "late-tab", action: "tab", text: JSON.stringify({ workspace: "late", pane: "p1", activate: true }), revision: null }) as Promise<any>; });
  const split = tree.root.findByType(Split);
  await act(async () => split.props.onChange({ ...split.props.state, panes: split.props.state.panes.map((pane: { id: string }) => pane.id === "p1" ? { ...pane, title: "changed target" } : pane) }));
  await act(async () => finish(Response.json({ workspace: "late", generation: "g-late", identity: "identity-late" })));
  await expect(pending).resolves.toMatchObject({ ok: false, error: "Target pane changed; inspect layout before opening the tab again." });
  expect(tree.root.findByType(Split).props.state.panes[0].tabs).toBeUndefined();
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(1);
});

it.each(["settings", "graph"])("Escape closes /%s inside a workspace tab without closing the tab or discarding the origin draft", async screen => {
  mockWorkspaceOpen();
  const origin = panePrompt("p1");
  await act(async () => origin.props.onDraft("origin unfinished"));
  await paneSubmit("p1", "/tabx shared");
  const tabView = () => tree.root.findByProps({ "data-pane-id": "p1" })
    .findAllByProps({ className: "workspace-tab-view" }).find(view => !view.props.hidden)!;
  const shared = tabView().findByType(Prompt);
  await act(async () => shared.props.onDraft("shared unfinished"));
  await act(async () => shared.props.onSubmit(`/${screen}`));
  const layout = tree.root.findByType(Split).props.state;
  const draftBeforeEscape = shared.props.draft;
  const opened = tabView().find(node => typeof node.type === "string" && node.props["aria-label"] === `/${screen}`);
  expect(opened.props.tabIndex).toBe(-1);
  const key = { key: "Escape", defaultPrevented: false, preventDefault: vi.fn(), stopPropagation: vi.fn() };
  await act(async () => opened.props.onKeyDown(key));
  expect(key.stopPropagation).toHaveBeenCalledOnce();
  expect(tabView().findAll(node => typeof node.type === "string" && node.props["aria-label"] === `/${screen}`)).toHaveLength(0);
  expect(tree.root.findByType(Split).props.state).toEqual(layout);
  expect(tabView().findByType(Prompt)).toBe(shared);
  expect(shared.props.draft).toBe(draftBeforeEscape);
  const originTab = tree.root.findAllByProps({ role: "tab" }).find(node => node.children.join("") === "workspace")!;
  await act(async () => originTab.props.onClick({ stopPropagation() {} }));
  expect(tabView().findByType(Prompt)).toBe(origin);
  expect(origin.props.draft).toBe("origin unfinished");
  expect(submits()).toHaveLength(0);
});

it("captures the issuing environment, supports explicit overrides, and keeps admitted panes unchanged", async () => {
  const target = { environment: "DEV", revision: `sha256:${"a".repeat(64)}`, target: "remote" };
  const terminal = vi.spyOn(Engine.prototype, "terminal").mockClear().mockResolvedValue({ target });
  await from("/rsplit xterm");
  expect(terminal).toHaveBeenLastCalledWith({ action: "resolve", environment: "DEV" });
  expect(tree.root.findByType(ShellTerminal).props).toMatchObject({ target, allowTargetStart: true });
  await environment("PROD");
  expect(tree.root.findByType(ShellTerminal).props.target).toEqual(target);
  const explicit = { ...target, environment: "TEST", target: "api" };
  terminal.mockResolvedValue({ target: explicit });
  await from('/bsplit xterm target:api env:"TEST"');
  expect(terminal).toHaveBeenLastCalledWith({ action: "resolve", environment: "TEST", target: "api" });
  expect(tree.root.findAllByType(ShellTerminal)[1]!.props.target).toEqual(explicit);
  expect(submits()).toHaveLength(0);
});

it("uses the originating actor environment and lets explicit command arguments override it", async () => {
  await from("/rsplit xterm");
  const shell = tree.root.findByType(ShellTerminal);
  const target = { environment: "AGENT", revision: `sha256:${"b".repeat(64)}`, target: "local" };
  const terminal = vi.spyOn(Engine.prototype, "terminal").mockClear().mockResolvedValue({ target });
  await act(async () => shell.props.onCommand("/bsplit xterm", "AGENT"));
  expect(terminal).toHaveBeenLastCalledWith({ action: "resolve", environment: "AGENT" });
  await act(async () => shell.props.onCommand("/lsplit xterm env:OTHER", "AGENT"));
  expect(terminal).toHaveBeenLastCalledWith({ action: "resolve", environment: "OTHER" });
});

it("does not resolve at capacity or create a pane after failed or outdated resolution", async () => {
  const terminal = vi.spyOn(Engine.prototype, "terminal").mockClear().mockRejectedValue(new Error("Multiple targets; specify target:NAME."));
  await from("/rsplit xterm");
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(0);
  expect(text()).toContain("Multiple targets");
  let finish!: (value: unknown) => void;
  terminal.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  await from("/rsplit xterm");
  await emit({ event: "session", generation: "replacement" });
  await act(async () => finish({ target: null }));
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(0);
  await from("/split 4");
  terminal.mockClear();
  await from("/rsplit xterm");
  expect(terminal).not.toHaveBeenCalled();
});

it("opens terminal tabs from wesx and +, preserves mounts, and closes a hidden caller only", async () => {
  const api=vi.spyOn(Engine.prototype,"terminal").mockResolvedValue({target:null,forgotten:true});
  await from("/rsplit xterm");
  const first=tree.root.findByType(ShellTerminal), firstHistory=first.props.history, firstCommand=first.props.onCommand;
  await act(async()=>first.props.onDirectory("/synthetic/first"));
  await act(async()=>first.props.onCommand("/terminal-tab"));
  const [retained,second]=tree.root.findAllByType(ShellTerminal);
  expect(retained).toBe(first);
  expect(second!.props.cwd).toBe("/synthetic/first");
  expect(first.props.focused).toBe(false);
  expect(second!.props.focused).toBe(true);
  expect(panes().panes).toHaveLength(2);
  api.mockClear();
  await act(async()=>first.props.beforeStart()); // Hidden history still belongs to the layout.
  await act(async()=>first.props.onCommand("/close"));
  expect(tree.root.findByType(ShellTerminal)).toBe(second);
  expect(api).toHaveBeenCalledExactlyOnceWith({action:"forget",history:firstHistory});
  await act(async()=>tree.root.findByProps({"aria-label":"New terminal tab"}).props.onClick({stopPropagation(){}}));
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(2);
  expect(tree.root.findAllByType(ShellTerminal)[0]).toBe(second);
  await expect(firstCommand("/terminal-tab")).rejects.toThrow("source terminal tab has closed");
  await expect(firstCommand("/tabx unwanted pane:p1")).rejects.toThrow("source terminal tab has closed");
});

it("captures terminal-tab destinations and refuses unselected or stale resolution without altering tabs", async () => {
  const target={environment:"QA",revision:`sha256:${"a".repeat(64)}`,target:"remote"};
  const api=vi.spyOn(Engine.prototype,"terminal").mockResolvedValue({target:null,forgotten:true});
  await from("/rsplit xterm");
  const first=tree.root.findByType(ShellTerminal);
  const before=panes();
  await expect(first.props.onCommand("/terminal-tab",null)).rejects.toThrow("No environment selected");
  expect(panes()).toBe(before);
  api.mockResolvedValueOnce({target});
  await act(async()=>first.props.onCommand("/terminal-tab env:QA target:remote"));
  expect(tree.root.findAllByType(ShellTerminal)[1]!.props).toMatchObject({target,allowTargetStart:true});
  let finish!:(reply:unknown)=>void;
  api.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;}));
  let adding!:Promise<void>;
  await act(async()=>{adding=first.props.onCommand("/terminal-tab");});
  await act(async()=>tree.root.findAllByProps({role:"tab"})[0]!.props.onClick({stopPropagation(){}}));
  await act(async()=>{finish({target:null});await expect(adding).rejects.toThrow("source pane changed");});
  expect(tree.root.findAllByType(ShellTerminal)).toHaveLength(2);
});

it("opens /spec in the chosen destination and preserves explicit pane and popup fallback behavior", async () => {
  await emit({ event: "workspace-context", name: "api-import-lab", saved: [] });
  await submit("/settings results open window");
  await submit("/spec");
  expect(window.open).toHaveBeenLastCalledWith("#spec?workspace=api-import-lab", "_blank");
  expect(tree.root.findAllByType(SpecScreen)).toHaveLength(0);
  expect(tree.root.findAllByType(Session)).toHaveLength(1);
  await submit("/settings results open screen");
  vi.mocked(window.open).mockClear();
  await submit("/spec");
  expect(window.open).not.toHaveBeenCalled();
  expect(tree.root.findAllByType(SpecScreen)).toHaveLength(1);
  await act(async () => tree.root.findByType(SpecScreen).props.onClose());
  await submit("/settings results open window");
  vi.mocked(window.open).mockReturnValue(null);
  await submit("/spec");
  expect(tree.root.findAllByType(SpecScreen)).toHaveLength(1);
  await act(async () => tree.root.findByType(SpecScreen).props.onClose());
  window.__WES_DESKTOP__ = true;
  await submit("/spec");
  expect(tree.root.findAllByType(SpecScreen)).toHaveLength(0);
  await submit("/spec split");
  expect(tree.root.findAllByType(SpecScreen)).toHaveLength(1);
  expect(submits()).toHaveLength(0);
});

it("routes a refused Run again once to scoped Logs and its footer notice, never beneath the prompt", async () => {
  await submit(":calc 1"); await accept("one");
  const original = last();
  send.mockResolvedValueOnce(new Response("Stream pipelines require an explicit source restart", { status: 400 }));
  await act(async () => repeat());
  expect(applicationLog.snapshot()).toHaveLength(1);
  expect(applicationLog.snapshot()[0]).toMatchObject({ source: "Engine", operation: "Run again",
    generation: "g1", cell: original.cell, detail: "Stream pipelines require an explicit source restart" });
  expect(applicationLog.snapshot()[0]?.workspace).toBeTruthy();
  expect(tree.root.findByProps({ className: "logs-notice" }).children.join("")).toContain("explicit source restart");
  expect(tree.root.findAllByType(MonoLine).filter(line => line.props.className === "surface-trouble")
    .map(line => lineText(line.props.segments)).join("")).not.toContain("Engine refused");
});

it("records local result-opening errors in Logs with context and no persistent prompt error", async () => {
  await submit(":calc 1");
  await emit({ event: "planned", cell: last().cell, text: last().text, nodes: [] });
  await act(async () => session().props.actions(session().props.model.cells[0]).open());
  expect(applicationLog.snapshot()).toEqual([expect.objectContaining({ source: "Workspace", generation: "g1",
    message: "that cell made no result to open" })]);
  expect(tree.root.findByProps({ className: "logs-notice" }).children.join("")).toBe("that cell made no result to open");
});

it("does not open saved name-only tabs automatically; an explicit tab open commits their identity", async () => {
  const open = mockWorkspaceOpen();
  await paneSubmit("p1", "/tabx second");
  const {workspaceBindings: _, ...nameOnly} = tree.root.findByType(Split).props.state;
  act(() => tree.unmount()); Events.all.clear(); open.mockClear();
  window.localStorage.getItem = () => JSON.stringify({paneLayout:nameOnly});
  await act(async () => { tree = create(<SurfaceApp />); });
  expect(open).not.toHaveBeenCalled();
  expect(text()).toContain("confirm this saved workspace binding");
  // Reopen from the origin pane; tab selection itself does not grant a new identity.
  await act(async () => tree.root.findByType(Split).props.onChange({...nameOnly,panes:nameOnly.panes.map((pane:any)=>({...pane,workspace:undefined}))}));
  await paneSubmit("p1", "/tabx second");
  expect(tree.root.findByType(Split).props.state.workspaceBindings).toEqual({second:"identity-second"});
  expect(open.mock.calls.map(([, options]) => JSON.parse(String(options.body)).create)).toEqual([true,false]);
});

it("rejects identity-less open replies before changing the layout", async () => {
  const open = mockWorkspaceOpen();
  const before = tree.root.findByType(Split).props.state;
  open.mockResolvedValueOnce(Response.json({workspace:"second",generation:"g2"}));
  await paneSubmit("p1", "/tabx second");
  expect(tree.root.findByType(Split).props.state).toBe(before);
  expect(applicationLog.snapshot().at(-1)?.message).toContain("stable workspace identity");
});

it("withdraws an old workspace controller until each changed binding has been confirmed", async () => {
  const open = mockWorkspaceOpen();
  await paneSubmit("p1", "/rsplit second");
  await emitWorkspace("second", {event:"session",generation:"g-second"}, env("DEV"));
  expect(tree.root.findByProps({"data-pane-id":"p2"}).findAllByType(Session)).toHaveLength(1);
  const originPrompt = panePrompt("p1");
  for (const identity of ["replacement-second", "identity-second"]) {
    let confirm!: (response: Response) => void;
    open.mockResolvedValueOnce(Response.json({workspace:"second",generation:"g-second",identity}))
      .mockImplementationOnce(() => new Promise(resolve => {confirm=resolve;}));
    await act(async () => originPrompt.props.onSubmit("/tab second"));
    expect(tree.root.findByType(Split).props.state.workspaceBindings.second).toBe(identity);
    expect(tree.root.findByProps({"data-pane-id":"p2"}).findAllByType(Session)).toHaveLength(0);
    await act(async () => confirm(Response.json({workspace:"second",generation:"g-second",identity})));
    expect(tree.root.findByProps({"data-pane-id":"p2"}).findAllByType(Session)).toHaveLength(1);
  }
});


it.each(["command", "source button"])("opens a resultless import source through %s without changing the draft or executing", async trigger => {
  const source = ':import spec file:"/synthetic/specs/a-long-provider-description.json" endpoint:"https://example.test" as:synthetic';
  await emit({ event: "planned", cell: "import-cell", text: source, nodes: [], restored: true });
  await type(":calc 999");
  const click = () => trigger === "source button"
    ? tree.root.findByProps({ "aria-label":"source" }).props.onClick()
    : tree.root.findByProps({ className: "cell-source" }).props.onClick({ metaKey: true, button: 0, preventDefault() {}, stopPropagation() {} });
  await act(async () => click());
  expect(tree.root.findByType(SourceView).props.source).toBe(source);
  expect(tree.root.findAllByType(EditScreen)).toHaveLength(0);
  await act(async () => tree.root.findByType(PeekScreen).props.onClose());
  expect(tree.root.findByType(Prompt).props.draft).toBe(":calc 999");
  expect(submits()).toHaveLength(0);
  await submit("/settings results open window");
  const sourceActions = session().props.actions(session().props.model.cells[0]);
  await act(async () => sourceActions.openSource());
  expect(window.open).toHaveBeenLastCalledWith(expect.stringMatching(/^#source\/import-cell/), "_blank");
  expect(tree.root.findAllByType(PeekScreen)).toHaveLength(0);
  vi.mocked(window.open).mockReturnValueOnce(null);
  await act(async () => sourceActions.openSource());
  expect(tree.root.findByType(SourceView).props.source).toBe(source);
  expect(submits()).toHaveLength(0);
});

it("inspects the complete cell rather than its last pipeline node", async () => {
  const source = ':calc 1 > first\n:calc 2 > second';
  await emit(created("first", { command: ":calc 1" }), created("second", { command: ":calc 2" }),
    { event: "planned", cell: "pipeline", text: source, nodes: ["first", "second"], restored: true });
  await act(async () => session().props.actions(session().props.model.cells[0]).peek("source"));
  expect(tree.root.findByType(SourceView).props.source).toBe(source);
  expect(submits()).toHaveLength(0);
});

it("restores a cell's source in its workspace window and withdraws retired or reset source", async () => {
  act(() => tree.unmount());
  await act(async () => { tree = create(<OpenWindow route={{ cell: "import-cell", tab: "result", peek: "source", workspace: "source-lab" }} />); });
  expect(Events.all.has("/events?workspace=source-lab")).toBe(true);
  const source = ':import spec file:"/synthetic/spec.json" as:synthetic';
  const restore = () => emit({ event: "planned", cell: "import-cell", text: source, nodes: [], restored: true });
  await emit({ event: "session", generation: "source-g1" }, { event: "workspace-context", name: "source-lab", saved: [] });
  await restore();
  expect(tree.root.findByType(SourceView).props.source).toBe(source);
  expect(fetchValue).not.toHaveBeenCalled();
  expect(submits()).toHaveLength(0);
  await emit({ event: "work-retired", cells: ["import-cell"] });
  expect(tree.root.findByType(SourceView).props.source).toBe("");
  expect(lineText(tree.root.findByType(PeekScreen).props.subject)).toContain("no cell called import-cell");
  await restore();
  await emit({ event: "session", generation: "source-g2" });
  expect(tree.root.findByType(SourceView).props.source).toBe("");
  await restore();
  await emit({ event: "workspace-closed", workspace: "source-lab" });
  expect(tree.root.findByType(SourceView).props.source).toBe("");
});

it("shows stale summaries only in the footer and follows the focused workspace", async () => {
  mockWorkspaceOpen();
  await emit(created("root-stale"), { event: "node", constructionComplete: false, node: "root-stale", state: "stale" });
  const footer = () => lineText(tree.root.findByProps({ className: "split-keys" }).props.segments);
  expect(footer()).toBe("~ 1 result stale   /stale");
  expect(tree.root.findAllByProps({ className: "session-note" })).toHaveLength(0);
  await paneSubmit("p1", "/bsplitx second");
  expect(footer()).toBe("");
  await emitWorkspace("second", { event: "session", generation: "g-second" }, env("DEV"),
    created("second-a"), created("second-b"),
    { event: "node", constructionComplete: false, node: "second-a", state: "stale" }, { event: "node", constructionComplete: false, node: "second-b", state: "stale" });
  expect(footer()).toContain("~ 2 results stale   /stale");
  await act(async () => tree.root.findByProps({ "data-pane-id": "p1" }).props.onFocus());
  expect(footer()).toContain("~ 1 result stale   /stale");
  await act(async () => tree.root.findByProps({ "data-pane-id": "p2" }).props.onFocus());
  expect(footer()).toContain("~ 2 results stale   /stale");
  await emitWorkspace("second", { event: "node", constructionComplete: false, node: "second-a", state: "stale", staleReason: { code: "stream_updated", message: "Stream advanced" } },
    { event: "node", constructionComplete: false, node: "second-b", state: "running" });
  expect(footer()).toBe("");
  expect(tree.root.findAllByProps({ className: "session-note" })).toHaveLength(0);
});

it("opens the focused workspace graph from the header without changing drafts", async () => {
  mockWorkspaceOpen();
  const press = async () => act(async () => tree.root.findByProps({ "aria-label": "Graph" }).props.onClick({ stopPropagation() {} }));
  await type("unfinished draft");
  await press();
  expect(tree.root.findByType(GraphScreen).props.chrome).toBe("full");
  await act(async () => tree.root.findByType(GraphScreen).props.onClose());
  expect(tree.root.findByType(Prompt).props.draft).toBe("unfinished draft");
  await submit("/settings results open window");
  await press();
  expect(window.open).toHaveBeenLastCalledWith(expect.stringMatching(/^#graph/), "_blank");
  expect(tree.root.findAllByType(GraphScreen)).toHaveLength(0);
  await paneSubmit("p1", "/bsplitx graph-lab");
  await emitWorkspace("graph-lab", { event: "session", generation: "graph-g1" }, env("DEV"));
  await press();
  expect(window.open).toHaveBeenLastCalledWith("#graph?workspace=graph-lab", "_blank");
  vi.mocked(window.open).mockReturnValueOnce(null);
  await press();
  expect(tree.root.findByProps({ "data-pane-id": "p2" }).findByType(GraphScreen).props.chrome).toBe("pane");
  expect(submits()).toHaveLength(0);
});

it("shows snapshot metadata and refreshes through the guarded repeat action", async () => {
  fetchValue.mockResolvedValue(Response.json({ ...value("names snapshot"), provenance: {
    "snapshot.kind": "names", "snapshot.capturedAt": "2026-01-02T03:04:05Z",
  } }));
  await submit(":list names"); await accept("one", { repeatable: false });
  await emit(ready("one", "snapshot"));
  const original = last();
  expect(text()).toContain("Snapshot");
  await act(async () => button("Refresh").props.onClick());
  expect(text()).toContain("confirm repeat");
  expect(last()).toBe(original);
  await act(async () => button("confirm repeat").props.onClick());
  expect(last().repeat).toBe(original.cell);
});

it("navigates to the existing definition with either spelling and without executing", async () => {
  await emit(created("source-node", { name: "orders" }), { event: "planned", cell: "source-cell", text: ":calc 42 > orders", nodes: ["source-node"], restored: true });
  const before = session().props.model.cells.map((cell: { id: string }) => cell.id);
  await from("/goto orders");
  expect(session().props.jump).toMatchObject({ pane: "p1", cell: "source-cell" });
  const revision = session().props.jump.revision;
  await from("/goto $orders");
  expect(session().props.jump.revision).toBeGreaterThan(revision);
  expect(session().props.model.cells.map((cell: { id: string }) => cell.id)).toEqual(before);
  expect(submits()).toHaveLength(0);
  await from("/goto missing");
  expect(text()).toContain("knows no result called missing");
  expect(panes().panes).toHaveLength(1);
});

it("finds a definition owned by another session pane, including from a value pane", async () => {
  await from("/split right");
  await fromPane(1, ":calc 2 > orders"); const sent = last();
  await emit(created("source-node", { name: "orders" }), { event: "planned", cell: sent.cell, text: sent.text, nodes: ["source-node"] });
  await from("/rsplitx $orders");
  await act(async () => tree.root.findByType(PaneCommand).props.onCommand("/goto orders"));
  expect(panes().focused).toBe("p2");
  const source = tree.root.findAllByType(Session).find(session => session.props.definition?.pane === "p2")!;
  expect(source.props.jump).toMatchObject({ pane: "p2", cell: sent.cell });
  expect(submits()).toHaveLength(1);
});

it("opens wide values as retained tabs, reuses them, and isolates each inner tab", async () => {
  await emit(created("source-node", { name: "orders" }), { event: "planned", cell: "source-cell", text: ":calc 42 > orders", nodes: ["source-node"], restored: true }, ready("source-node", "value-handle"));
  await from("/tab $orders");
  expect(panes().panes[0].value).toBeUndefined();
  expect(panes().panes[0].tabs).toHaveLength(2);
  expect(text()).toContain("/tabx $orders to switch");
  await from("/tabx $orders");
  expect(panes().panes[0].value).toEqual({ node: "source-node", generation: "g1", label: "orders" });
  expect(tree.root.findByType(OpenScreen).props.value.data).toBe("synthetic result");
  await act(async () => tree.root.findByType(OpenScreen).props.onTab("json"));
  await from("/tabx related $orders");
  expect(panes().panes[0].tabs).toHaveLength(3);
  expect(tree.root.findByType(ComponentPane).props.binding.node).toBe("source-node");
  await from("/tabx $orders");
  expect(tree.root.findByType(OpenScreen).props.tab).toBe("json");
  expect(panes().panes[0].tabs).toHaveLength(3);
  expect(submits()).toHaveLength(0);
  expect(window.open).not.toHaveBeenCalled();
  await act(async () => tree.root.findByType(OpenScreen).props.onClose());
  expect(panes().panes[0].tabs).toHaveLength(2);
});

it("keeps a wide view on its original node after rebinding and refuses a replacement generation", async () => {
  await emit(created("old-node", { name: "orders" }), ready("old-node", "old-handle"));
  await from("/split $orders");
  await emit(created("old-node", { name: "" }), created("new-node", { name: "orders" }), ready("new-node", "new-handle"));
  expect(panes().panes[1].value.node).toBe("old-node");
  expect(text()).toContain("stays with the original node");
  await from("/tab $orders");
  expect(panes().panes[0].tabs[1].value.node).toBe("new-node");
  await emit({ event: "session", generation: "g2" }, created("old-node", { name: "orders" }), ready("old-node", "replacement-handle"));
  expect(tree.root.findAllByType(OpenScreen)).toHaveLength(0);
  expect(text()).toContain("previous workspace session");
  expect(submits()).toHaveLength(0);
});

it("fails missing values and ambiguous bare names without opening workspaces or changing layout", async () => {
  await emit(created("node-id", { name: "orders" }));
  const original = panes(); const count = requests.length;
  for (const line of ["/tab $missing", "/split $missing", "/goto missing", "/tab orders", "/split right orders"]) {
    await from(line); expect(panes()).toBe(original);
  }
  expect(text()).toContain("bare names open workspaces");
  expect(requests.slice(count)).toHaveLength(0);
  await from("/split $orders");
  expect(text()).toContain("Waiting for this node's output");
  expect(submits()).toHaveLength(0);
});

it("reveals the original acknowledged definition after its owning pane closes", async () => {
  await from("/split right");
  await fromPane(1, ":calc 2 > orders"); const sent = last();
  await emit(created("source-node", { name: "orders" }), { event: "planned", cell: sent.cell, text: sent.text, nodes: ["source-node"] });
  await fromPane(1, "/close");
  expect(panes().panes).toHaveLength(1);
  await from("/goto orders");
  expect(session().props.model.cells.map((cell: { id: string }) => cell.id)).toContain(sent.cell);
  expect(session().props.jump.cell).toBe(sent.cell);
  expect(submits()).toHaveLength(1);
});

it("reveals primary history in the remaining session when the primary pane was closed", async () => {
  await from(":calc 2 > orders"); const sent = last(); await accept("source-node", { name: "orders" });
  await from("/split right");
  await from("/close");
  expect(panes().panes.map((pane: { id: string }) => pane.id)).toEqual(["p2"]);
  await from("/goto orders");
  expect(session().props.model.cells.map((cell: { id: string }) => cell.id)).toContain(sent.cell);
  expect(session().props.jump).toMatchObject({ pane: "p2", cell: sent.cell });
  expect(submits()).toHaveLength(1);
});

it("resolves and completes only the issuing workspace even when aliases overlap", async () => {
  mockWorkspaceOpen();
  await emit(created("origin-node", { name: "orders" }), created("origin-only", { name: "originOnly" }),
    { event: "planned", cell: "origin-cell", text: ":calc 1 > orders", nodes: ["origin-node"], restored: true });
  await paneSubmit("p1", "/rsplitx second");
  await emitWorkspace("second", { event: "session", generation: "g-second" }, created("second-node", { name: "orders" }),
    { event: "planned", cell: "second-cell", text: ":calc 2 > orders", nodes: ["second-node"], restored: true });
  expect(panePrompt("p2").props.variables).toEqual(["orders"]);
  await paneSubmit("p2", "/goto originOnly");
  expect(panes().focused).toBe("p2");
  const second = tree.root.findAllByType(Session).find(session => session.props.definition?.workspace === "second")!;
  expect(second.props.jump).toBeUndefined();
  await paneSubmit("p2", "/tabx $orders");
  expect(panes().panes[1].value).toEqual({ node: "second-node", generation: "g-second", label: "orders" });
  await act(async () => tree.root.findByType(PaneCommand).props.onCommand("/goto orders"));
  expect(second.props.jump).toMatchObject({ pane: "p2", cell: "second-cell" });
  expect(panes().panes[1].value).toBeUndefined();
  expect(submits()).toHaveLength(0);
});


it("saves workspace UI dashboards and opens the same layout in tabs and panes without executing a source", async () => {
  await emit({event:'workspace-context',name:'layout-lab',saved:['layout-lab']},created('count-source',{name:'count'}),ready('count-source','count-value'));
  await from('/dashboard');
  const board={...addDashboardMember(newDashboard(),{id:'member-count',node:'count-source',generation:'g1',label:'$count'}),name:'overview'};
  await act(async()=>tree.root.findByType(DashboardHost).props.onSave(board));
  expect(tree.root.findByType(DashboardHost).props.saved[0].board.revision).toBe(1);
  await act(async()=>tree.root.findByType(DashboardHost).props.onClose());
  await from('/tabx $overview');
  expect(panes().panes[0].value).toMatchObject({node:board.id,dashboard:true});
  expect(tree.root.findByType(DashboardHost).props.boardId).toBe(board.id);
  await act(async()=>tree.root.findByType(PaneCommand).props.onCommand('/split $overview'));
  expect(panes().panes[1].value).toMatchObject({node:board.id,dashboard:true});
  expect(tree.root.findAllByType(DashboardHost)).toHaveLength(2);
  const hosts=tree.root.findAllByType(DashboardHost),stored=hosts[0]!.props.saved[0].board;
  await act(async()=>hosts[0]!.props.onSave({...stored,title:'Updated layout'}));
  expect(tree.root.findAllByType(DashboardHost).map(host=>host.props.saved[0].board.title)).toEqual(['Updated layout','Updated layout']);
  expect(()=>hosts[1]!.props.onSave(stored)).toThrow('changed');
  expect(submits()).toHaveLength(0);
});

it("should_LeaveThroughTheVisibleDashboardExit_When_SummonedOpenedInATabOrUnavailable", async () => {
  // Arrange
  await emit({event:'workspace-context',name:'layout-lab',saved:['layout-lab']},created('count-source',{name:'count'}),ready('count-source','count-value'));
  const exit=(label:string)=>tree.root.findAllByType('button').find(node=>node.props.className?.includes('dashboard-host-exit')&&node.children.join('')===label)!;
  await from('/dashboard');
  const board={...addDashboardMember(newDashboard(),{id:'member-count',node:'count-source',generation:'g1',label:'$count'}),name:'overview'};
  await act(async()=>tree.root.findByType(DashboardHost).props.onSave(board));
  // Act: the summoned library returns to the session.
  await act(async()=>exit('Back to session').props.onClick());
  // Assert
  expect(tree.root.findAllByType(DashboardHost)).toHaveLength(0);
  // Act: a dashboard view tab closes that tab only.
  await from('/tabx $overview');
  expect(panes().panes[0].value).toMatchObject({node:board.id,dashboard:true});
  await act(async()=>exit('Close dashboard').props.onClick());
  // Assert
  expect(panes().panes[0].value).toBeUndefined();
  expect(tree.root.findAllByType(DashboardHost)).toHaveLength(0);
  expect(panes().panes).toHaveLength(1);
  // Act: a summoned named board removed underneath still offers the way back.
  await from('/dashboard $overview');
  const host=tree.root.findByType(DashboardHost);
  await act(async()=>host.props.onRemove(host.props.saved[0].board));
  expect(JSON.stringify(tree.toJSON())).toContain('No dashboard named $overview');
  await act(async()=>exit('Back to session').props.onClick());
  // Assert
  expect(JSON.stringify(tree.toJSON())).not.toContain('No dashboard named');
  expect(submits()).toHaveLength(0);
});

it("should_ShowTheLiveDisplayInsteadOfAPermanentRead_When_AnActiveStreamIsOpenedInAValuePane", async () => {
  // Arrange
  const metadata = { revision: "1", epochs: [["logs-node", "r1"] as const], sources: [{ node: "logs-node", run: "r1", phase: "open" as const, counts: { omitted: "0", rejected: "0", windowItems: 1, accepted: "1" } }] };
  const liveView = vi.spyOn(Engine.prototype, "liveView").mockResolvedValue({ value: value("synthetic log line"), metadata });
  await emit(created("logs-node", { name: "logs", command: "docker logs follow container:\"orders-api\"", streamOutput: true, streamSource: true }),
    { event: "planned", cell: "logs-cell", text: "docker logs follow > logs", nodes: ["logs-node"] },
    { event: "node", constructionComplete: false, node: "logs-node", state: "ready" } as Event, ready("logs-node", "logs-handle"));
  // Act
  await from("/tabx $logs");
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 250)); });
  // Assert
  const open = tree.root.findByType(OpenScreen);
  expect(open.props.live).toBeDefined();
  expect(text()).not.toContain("reading result…");
  expect(text()).toContain("synthetic log line");
  expect(liveView.mock.calls.every(([node, generation]) => node === "logs-node" && generation === "g1")).toBe(true);
  expect(fetchValue).not.toHaveBeenCalledWith("logs-handle");
  expect(submits()).toHaveLength(0);
});

it("should_KeepTheStoredLastValue_When_AStoppedStreamIsOpenedInAValuePane", async () => {
  // Arrange
  const liveView = vi.spyOn(Engine.prototype, "liveView");
  await emit(created("logs-node", { name: "logs", streamOutput: true, streamSource: true }),
    { event: "stopped", node: "logs-node", state: "cancelled", source: "logs-node", run: "r1", type: "Text", handle: "last-handle", bytes: 1, provenance: {}, cautions: [], kept: false } as Event);
  // Act
  await from("/tabx $logs");
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 250)); });
  // Assert
  expect(tree.root.findByType(OpenScreen).props.live).toBeUndefined();
  expect(fetchValue).toHaveBeenCalledWith("last-handle");
  expect(text()).toContain("stream stopped · last value");
  expect(liveView).not.toHaveBeenCalled();
});
