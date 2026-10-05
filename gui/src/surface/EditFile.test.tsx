import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { EditFileScreen } from "./screens/EditFile";
import { topLine } from "./session-model";
import { sessionContext } from "./session-fixture";
import { schema } from "./yaml-schema-fixture.test-support";
import type { YamlSchema } from "./yaml-schema";

const schemaLoader = vi.hoisted(() => vi.fn());
vi.mock("./yaml-schema-loader", () => ({ loadYamlSchema: schemaLoader }));

const editor = vi.hoisted(() => ({
  change: (_source: string) => {},
  save: () => {},
  run: () => {},
  takeFocus: true,
  source: "",
  context: "",
  made: vi.fn(), destroyed: vi.fn(), updated: vi.fn(),
}));
vi.mock("./yaml-editor", () => ({ makeYamlEditor: (_parent: unknown, source: string, wiring: { onSave: () => void; onRun: () => void; context: string }, change: (source: string) => void, takeFocus: boolean) => {
  editor.change = change; editor.save = wiring.onSave; editor.run = wiring.onRun; editor.takeFocus = takeFocus; editor.source = source; editor.context = wiring.context;
  editor.made(wiring);
  return { destroy() { editor.destroyed(); } };
}, updateYamlSchema: (_view: unknown, schema: YamlSchema) => editor.updated(schema) }));
beforeEach(() => {
  vi.unstubAllGlobals();
  schemaLoader.mockReset().mockResolvedValue(schema);
  editor.made.mockClear(); editor.destroyed.mockClear(); editor.updated.mockClear();
});

describe("Cmd+R submits the current unsaved document", () => {
  const runText = (tree: ReactTestRenderer) => tree.root.findAllByType("pre")
    .filter(pre => String(pre.props.className).includes("edit-run-status")).map(textOf).join("");

  it("sends no operation for local syntax errors and retains text", async () => {
    const onRun = vi.fn().mockResolvedValue("loaded");
    const tree = draw({ onRun, generation: "g", initialSource: "types: [" });
    await act(async () => {});
    await act(async () => editor.run());
    expect(onRun).not.toHaveBeenCalled();
    expect(runText(tree)).toBeTruthy();
    expect(editor.destroyed).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it.each(["env", "types"] as const)("sends one %s action without saving or blocking unknown types", async context => {
    const onRun = vi.fn().mockResolvedValue("backend accepted");
    const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
    const tree = draw({ context, onRun, generation: "g" });
    await act(async () => {});
    act(() => editor.change("types:\n  Local: {base: UnknownYet}"));
    await act(async () => editor.run());
    expect(onRun).toHaveBeenCalledExactlyOnceWith("types:\n  Local: {base: UnknownYet}", expect.objectContaining({ generation: "g", name: "", origin: expect.stringMatching(/^editor:/) }));
    expect(fetch).not.toHaveBeenCalled();
    expect(runText(tree)).toBe("backend accepted");
    expect(statusText(tree)).toBe("not saved yet");
    act(() => tree.unmount());
  });

  it("shows backend semantic errors without closing, saving or remounting the buffer", async () => {
    const onClose = vi.fn();
    const onRun = vi.fn().mockRejectedValue(new Error("Unknown base type: Missing"));
    const tree = draw({ onRun, onClose, generation: "g", initialSource: "types: {Local: {base: Missing}}" });
    await act(async () => {});
    await act(async () => editor.run());
    expect(runText(tree)).toBe("Unknown base type: Missing");
    expect(onClose).not.toHaveBeenCalled(); expect(editor.destroyed).not.toHaveBeenCalled();
    expect(editor.source).toBe("types: {Local: {base: Missing}}");
    act(() => tree.unmount());
  });

  it("serializes repeated shortcuts and ignores the old response after edits", async () => {
    let finish!: (message: string) => void;
    const onRun = vi.fn(() => new Promise<string>(resolve => { finish = resolve; }));
    const tree = draw({ onRun, generation: "g", initialSource: "types: {}" });
    await act(async () => {});
    await act(async () => { editor.run(); editor.run(); });
    expect(onRun).toHaveBeenCalledOnce();
    act(() => editor.change("types: {New: {base: Text}}"));
    await act(async () => finish("old success"));
    expect(runText(tree)).toBe("");
    act(() => tree.unmount());
  });

  it("keeps captured environment/base and refuses a replacement workspace", async () => {
    const onRun = vi.fn().mockResolvedValue("planned");
    const props = { top, context: "env" as const, vocabulary: { names: [] }, onClose() {}, onRun,
      generation: "g", environmentContext: { selected: "DEV", revisions: { DEV: "v1" } } };
    const tree = draw(props);
    await act(async () => {});
    act(() => tree.root.findByProps({ "aria-label": "Base directory for local inputs" }).props.onChange({ target: { value: "/synthetic/base" } }));
    await act(async () => tree.update(<EditFileScreen {...props} environmentContext={{ selected: "OTHER", revisions: {} }} />));
    await act(async () => editor.run());
    expect(onRun.mock.calls[0]).toEqual(["", expect.objectContaining({ base: "/synthetic/base", environments: props.environmentContext })]);
    await act(async () => tree.update(<EditFileScreen {...props} generation="other" />));
    await act(async () => editor.run());
    expect(onRun).toHaveBeenCalledOnce(); expect(runText(tree)).toContain("Workspace changed");
    act(() => tree.unmount());
  });
});

const top = topLine({ ...sessionContext, connection: "connected" });

function textOf(node: { children: readonly unknown[] }): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : textOf(child as { children: readonly unknown[] })))
    .join("");
}

function draw(props: Partial<Parameters<typeof EditFileScreen>[0]> = {}): ReactTestRenderer {
  let tree: ReactTestRenderer | undefined;
  act(() => {
    tree = create(
      <EditFileScreen
        top={top}
        context="types"
        vocabulary={{ names: ["Text", "Int"] }}
        onClose={() => undefined}
        {...props}
      />,
      { createNodeMock: element => element.props.className === "edit-code" ? {} : null },
    );
  });
  return tree!;
}

const statusText = (tree: ReactTestRenderer) =>
  tree.root.findAllByType("pre").filter((pre) => String(pre.props.className).includes("edit-file-status")).map(textOf).join("");

describe("the file-buffer screen /edit env|types|views opens", () => {
  it.each([["MacIntel", "⌘"], ["Win32", "⌃"]])("should_NameTheEditorsOwnModifier_When_ThePlatformIs %s", (platform, mod) => {
    vi.stubGlobal("navigator", { platform });
    const footer = (props: Partial<Parameters<typeof EditFileScreen>[0]>) => {
      const tree = draw(props);
      const said = tree.root.findAllByType("pre").filter((pre) => String(pre.props.className).includes("screen-footer")).map(textOf).join("");
      act(() => tree.unmount());
      return said;
    };
    expect(footer({})).toBe(`esc back to the session, the half-typed line intact   ${mod}S save`);
    expect(footer({ context: "env", onRun: async () => "planned" })).toBe(`esc back to the session, the half-typed line intact   ${mod}S save · ${mod}R plan`);
  });

  it("should_StartUnsaved_When_NoNameOrSourceWasGiven", () => {
    const tree = draw();
    expect(statusText(tree)).toBe("not saved yet");
    act(() => tree.unmount());
  });

  it("should_RefuseToSave_When_NoNameHasBeenTyped", async () => {
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);
    const tree = draw();
    await act(async () => tree.root.findByProps({ className: "cell-action" }).props.onClick());
    expect(fetch).not.toHaveBeenCalled();
    expect(statusText(tree)).toBe("name it first");
    act(() => tree.unmount());
    vi.unstubAllGlobals();
  });

  it("should_PostTheContextNameAndContent_When_TheSaveActionRuns", async () => {
    const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ path: "/home/.wes/edit/types/monitor.yaml" }) });
    vi.stubGlobal("fetch", fetch);
    const tree = draw({ context: "types", initialName: "monitor", initialSource: "types: {}" });
    await act(async () => tree.root.findByProps({ className: "cell-action" }).props.onClick());
    expect(fetch).toHaveBeenCalledWith(
      "/edit-files",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ context: "types", name: "monitor", content: "types: {}" }),
      }),
    );
    expect(statusText(tree)).toBe("saved to /home/.wes/edit/types/monitor.yaml");
    act(() => tree.unmount());
    vi.unstubAllGlobals();
  });

  it("should_ReportTheEnginesOwnMessage_When_TheSaveIsRejected", async () => {
    const fetch = vi.fn().mockResolvedValue({ ok: false, status: 400, text: async () => "invalid file name" });
    vi.stubGlobal("fetch", fetch);
    const tree = draw({ initialName: "../escape" });
    await act(async () => tree.root.findByProps({ className: "cell-action" }).props.onClick());
    expect(statusText(tree)).toBe("invalid file name");
    act(() => tree.unmount());
    vi.unstubAllGlobals();
  });

  it("should_TypeTheNameIntoTheTextbox_When_SomebodyEditsIt", () => {
    const tree = draw();
    const input = tree.root.findByProps({ className: "edit-file-name" });
    act(() => input.props.onChange({ target: { value: "orders" } }));
    expect(tree.root.findByProps({ className: "edit-file-name" }).props.value).toBe("orders");
    act(() => tree.unmount());
  });
});


it("marks edits unsaved and never calls a newer revision saved when a pending save finishes", async () => {
  let finish!: (response: unknown) => void;
  const fetch = vi.fn().mockImplementation(() => new Promise(resolve => { finish = resolve; }));
  vi.stubGlobal("fetch", fetch);
  const tree = draw({ initialName: "one", initialSource: "types: {}" });
  await act(async () => {});
  await act(async () => editor.save());
  expect(statusText(tree)).toBe("saving…");
  await act(async () => { editor.change("types: {A: Int}"); editor.save(); });
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(statusText(tree)).toBe("not saved yet");
  await act(async () => finish({ ok: true, json: async () => ({ path: "/edit/types/one.yaml" }) }));
  expect(statusText(tree)).toBe("not saved yet");
  await act(async () => editor.save());
  expect(JSON.parse(fetch.mock.calls[1]![1].body)).toMatchObject({ content: "types: {A: Int}" });
  await act(async () => finish({ ok: true, json: async () => ({ path: "/edit/types/one.yaml" }) }));
  expect(statusText(tree)).toContain("saved to");
  act(() => tree.root.findByProps({ className: "edit-file-name" }).props.onChange({ target: { value: "two" } }));
  expect(statusText(tree)).toBe("not saved yet");
  act(() => tree.unmount());
});

it("replaces a reused buffer and routes its keyboard save to its current context without stealing pane focus", async () => {
  const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ path: "/edit/env/new.yaml" }) });
  vi.stubGlobal("fetch", fetch);
  const tree = draw({ context: "types", initialName: "old", initialSource: "types: {}" });
  await act(async () => {});
  await act(async () => tree.update(<EditFileScreen top={top} context="env" initialName="new" vocabulary={{ names: [] }} onClose={() => {}} chrome="pane" />));
  expect(tree.root.findByProps({ className: "edit-file-name" }).props.value).toBe("new");
  expect(editor.source).toBe("");
  expect(editor.takeFocus).toBe(false);
  await act(async () => editor.save());
  expect(JSON.parse(fetch.mock.calls[0]![1].body)).toEqual({ context: "env", name: "new", content: "" });
  act(() => tree.unmount());
});


it("does not apply a delayed save error body to a newer edit", async () => {
  let errorBody!: (body: string) => void;
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: false, status: 500, text: () => new Promise(resolve => { errorBody = resolve; }) }));
  const tree = draw({ initialName: "one" });
  await act(async () => {});
  await act(async () => editor.save());
  act(() => editor.change("types: {}"));
  await act(async () => errorBody("old failure"));
  expect(statusText(tree)).toBe("not saved yet");
  act(() => tree.unmount());
});


it("passes the selected env schema into the live editor", async () => {
  const tree = draw({ context: "env", initialSource: "version: 1\npa" });
  await act(async () => {});
  expect(editor.context).toBe("env");
  expect(editor.source).toBe("version: 1\npa");
  act(() => tree.unmount());
});

it("applies a late schema without remounting or losing typed text and dirty-save status", async () => {
  let ready!: (schema: YamlSchema) => void;
  schemaLoader.mockImplementationOnce(() => new Promise(resolve => { ready = resolve; }));
  const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ path: "/synthetic/edit/env/demo.yaml" }) });
  vi.stubGlobal("fetch", fetch);
  const tree = draw({ context: "env", initialName: "demo" });
  await act(async () => {});
  act(() => editor.change("version: 1\npackage: typed"));
  await act(async () => ready(schema));
  expect(editor.made).toHaveBeenCalledTimes(1);
  expect(editor.updated).toHaveBeenCalledExactlyOnceWith(schema);
  expect(editor.destroyed).not.toHaveBeenCalled();
  expect(statusText(tree)).toBe("not saved yet");
  expect(tree.root.findAllByProps({ className: "edit-schema-status" })).toHaveLength(0);
  await act(async () => editor.save());
  expect(JSON.parse(fetch.mock.calls[0]![1].body).content).toBe("version: 1\npackage: typed");
  act(() => tree.unmount());
});

it("ignores and aborts old schema responses after switching buffers or unmounting", async () => {
  const requests: { resolve: (schema: YamlSchema) => void; signal: AbortSignal }[] = [];
  schemaLoader.mockImplementation(signal => new Promise(resolve => requests.push({ resolve, signal })));
  const tree = draw({ context: "env", initialName: "old" });
  await act(async () => {});
  await act(async () => tree.update(<EditFileScreen top={top} context="types" initialName="new" vocabulary={{ names: [] }} onClose={() => {}} />));
  expect(requests[0]!.signal.aborted).toBe(true);
  await act(async () => requests[0]!.resolve(schema));
  expect(editor.updated).not.toHaveBeenCalled();
  act(() => tree.unmount());
  expect(requests[1]!.signal.aborted).toBe(true);
  await act(async () => requests[1]!.resolve(schema));
  expect(editor.updated).not.toHaveBeenCalled();
});

it("keeps editing and saving usable after schema failure and retries on reopening", async () => {
  schemaLoader.mockRejectedValueOnce(new Error("offline"));
  const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ path: "/synthetic/saved.yaml" }) });
  vi.stubGlobal("fetch", fetch);
  const tree = draw({ initialName: "one" });
  await act(async () => {});
  expect(textOf(tree.root.findByProps({ className: "mono-line edit-schema-status" }))).toBe("Completion schema unavailable");
  act(() => editor.change("typed while offline"));
  await act(async () => editor.save());
  expect(JSON.parse(fetch.mock.calls[0]![1].body).content).toBe("typed while offline");
  act(() => tree.unmount());
  const reopened = draw({ initialName: "one" });
  await act(async () => {});
  expect(schemaLoader).toHaveBeenCalledTimes(2);
  act(() => reopened.unmount());
});

it("reopened document runs preserve captured source origin and base without rewriting the buffer", async () => {
  const onRun = vi.fn().mockResolvedValue("planned");
  const source = "version: 1\r\npackage: synthetic\n";
  const tree = draw({ context: "env", onRun, generation: "g", initialSource: source,
    initialBase: '/synthetic/with "quotes"', initialOrigin: "editor:captured", environmentContext: { selected: "DEV", revisions: { DEV: "revision" } } });
  await act(async () => {});
  await act(async () => editor.run());
  expect(onRun).toHaveBeenCalledExactlyOnceWith(source, expect.objectContaining({ base: '/synthetic/with "quotes"', origin: "editor:captured", environments: { selected: "DEV", revisions: { DEV: "revision" } } }));
  expect(editor.source).toBe(source);
  act(() => tree.unmount());
});

it("lists current environment documents and opens the selected captured source or New", async () => {
  const loadEnvironments = vi.fn().mockResolvedValue([{ name: "work", environments: ["dev", "prod"], source: "version: 1\npackage: work\nenvironments: {dev: {}, prod: {}}", origin: "environment-editor/revision/work" }]);
  const tree = draw({ context: "env", generation: "g", loadEnvironments });
  await act(async () => {});
  expect(editor.made).not.toHaveBeenCalled();
  const document = tree.root.findByProps({ className: "environment-document" });
  expect(textOf(document)).toBe("workdev, prod");
  await act(async () => document.props.onClick());
  expect(editor.source).toContain("package: work");
  act(() => tree.unmount());
  const fresh = draw({ context: "env", generation: "g", loadEnvironments });
  await act(async () => {});
  await act(async () => fresh.root.findAllByType("button").find(button => textOf(button) === "New")!.props.onClick());
  expect(editor.source).toBe("");
  act(() => fresh.unmount());
});
