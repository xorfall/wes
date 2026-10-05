import { schema } from "./yaml-schema-fixture.test-support";
import { describe, expect, it, vi } from "vitest";
import { CompletionContext, startCompletion, completionStatus } from "@codemirror/autocomplete";
import { EditorState, type TransactionSpec } from "@codemirror/state";
import { EditorView, getDrawSelectionConfig, keymap } from "@codemirror/view";
import { undo, redo } from "@codemirror/commands";
import { yamlCompletionSource, yamlExtensions, yamlNewline, updateYamlSchema, yamlModeReader } from "./yaml-editor";
import type { YamlContext } from "./yaml-schema";

function editor(doc: string, context: YamlContext = "env") {
  const wiring = { context, schema, vocabulary: { names: ["Text", "Int", "Invoice"] }, onSave: vi.fn(), onRun: vi.fn(), onLeave: vi.fn() };
  let state = EditorState.create({ doc, selection: { anchor: doc.length }, extensions: yamlExtensions(wiring, () => {}) });
  const view = { get state() { return state; }, dispatch(...specs: TransactionSpec[]) { state = state.update(...specs).state; } } as EditorView;
  return { view, wiring };
}

describe("the actual YAML CodeMirror extension", () => {
  it("reconfigures a late schema while preserving document, selection and undo history", async () => {
    const wiring = { context: "env" as const, vocabulary: { names: [] }, onSave() {}, onLeave() {} };
    let state = EditorState.create({ doc: "version: 1\n", extensions: yamlExtensions(wiring, () => {}) });
    const view = { get state() { return state; }, dispatch(...specs: TransactionSpec[]) { state = state.update(...specs).state; } } as EditorView;
    view.dispatch({ changes: { from: state.doc.length, insert: "pa" }, selection: { anchor: state.doc.length + 2 }, userEvent: "input.type" });
    const before = state;
    expect(yamlCompletionSource(wiring)(new CompletionContext(state, state.doc.length, true))).toBeNull();
    updateYamlSchema(view, schema);
    expect(state.doc).toBe(before.doc);
    expect(state.selection).toBe(before.selection);
    expect(yamlCompletionSource(wiring)(new CompletionContext(state, state.doc.length, true))).toMatchObject({ options: [expect.objectContaining({ label: "package" })] });
    expect(undo(view)).toBe(true);
    expect(state.doc.toString()).toBe("version: 1\n");
  });

  it("reuses one mode for gutter/paint/lint and refreshes it when live type names change", () => {
    const { view, wiring } = editor("types:\n  Row:\n    base: Later", "types");
    const read = yamlModeReader(wiring.vocabulary, "types");
    const first = read(view.state);
    expect(first.diagnostics).toHaveLength(1);
    for (let index = 0; index < 100; index++) expect(read(view.state)).toBe(first);
    wiring.vocabulary.names.push("Later");
    expect(read(view.state).diagnostics).toEqual([]);
  });
  it.each([false, true])("offers package through the automatic/explicit source (explicit=%s)", async explicit => {
    const { view, wiring } = editor("version: 1\npa");
    const result = await yamlCompletionSource(wiring)(new CompletionContext(view.state, view.state.doc.length, explicit));
    expect(result).toMatchObject({ from: 11, to: 13, options: [expect.objectContaining({ label: "package", apply: "package: " })] });
    view.dispatch({ changes: { from: result!.from, to: result!.to, insert: result!.options[0]!.apply as string } });
    expect(view.state.doc.toString()).toBe("version: 1\npackage: ");
    expect(wiring.onSave).not.toHaveBeenCalled();
  });
  it("uses the complete replacement range without duplicating an existing colon", async () => {
    const { view, wiring } = editor("version: 1\npackag: demo");
    const result = await yamlCompletionSource(wiring)(new CompletionContext(view.state, 13, true));
    expect(result).toMatchObject({ from: 11, to: 17 });
    expect(result!.options[0]!.apply).toBe("package");
    view.dispatch({ changes: { from: result!.from, to: result!.to, insert: result!.options[0]!.apply as string } });
    expect(view.state.doc.toString()).toBe("version: 1\npackage: demo");
  });
  it("opens map/list shapes after accepting their key and retains undo/redo", async () => {
    for (const [doc, expected] of [["environments:\n  dev:\n    imp", "environments:\n  dev:\n    imports: \n      "], ["environments:\n  dev:\n    hide:\n      imp", "environments:\n  dev:\n    hide:\n      imports: \n        - "]]) {
      const { view, wiring } = editor(doc!);
      const result = await yamlCompletionSource(wiring)(new CompletionContext(view.state, view.state.doc.length, true));
      const insert = result!.options[0]!.apply as string;
      view.dispatch({ changes: { from: result!.from, to: result!.to, insert }, selection: { anchor: result!.from + insert.length }, userEvent: "input.complete" });
      const completed = view.state.doc.toString();
      expect(yamlNewline("env")(view)).toBe(true);
      expect(view.state.doc.toString()).toBe(expected);
      expect(undo(view)).toBe(true);
      expect(view.state.doc.toString()).toBe(completed);
      expect(redo(view)).toBe(true);
      expect(view.state.doc.toString()).toBe(expected);
    }
  });
  it("retains keyboard commands, dynamic types and one managed caret", async () => {
    const { view, wiring } = editor("types:\n  Row:\n    base: I", "types");
    wiring.vocabulary.names.push("Item");
    const result = await yamlCompletionSource(wiring)(new CompletionContext(view.state, view.state.doc.length, true));
    expect(result!.options.map(o => o.label)).toEqual(["Int", "Invoice", "Item", "Iter"]);
    const bindings = view.state.facet(keymap).flat();
    expect(bindings.map(b => b.key)).toEqual(expect.arrayContaining(["Ctrl-Space", "Mod-s", "Escape", "Shift-Enter", "Enter"]));
    expect(getDrawSelectionConfig(view.state).drawRangeCursor).toBe(false);
    bindings.find(b => b.key === "Mod-s")!.run!(view);
    bindings.find(b => b.key === "Mod-r")!.run!(view);
    bindings.filter(b => b.key === "Escape").some(b => b.run?.(view));
    expect(wiring.onSave).toHaveBeenCalledOnce();
    expect(wiring.onRun).toHaveBeenCalledOnce();
    expect(wiring.onLeave).toHaveBeenCalledOnce();
  });
  it("dismisses completion before leaving the buffer on Escape", () => {
    const { view, wiring } = editor("pa");
    startCompletion(view);
    expect(completionStatus(view.state)).toBe("pending");
    const escape = () => view.state.facet(keymap).flat().filter(b => b.key === "Escape").some(b => b.run?.(view));
    escape();
    expect(wiring.onLeave).not.toHaveBeenCalled();
    expect(completionStatus(view.state)).toBe(null);
    escape();
    expect(wiring.onLeave).toHaveBeenCalledOnce();
  });
  it("splits existing lines without replacing text and replaces selected text predictably", () => {
    const { view } = editor("package: synthetic");
    view.dispatch({ selection: { anchor: 12 } });
    yamlNewline("env")(view);
    expect(view.state.doc.toString()).toBe("package: syn\nthetic");
    view.dispatch({ selection: { anchor: 9, head: 12 } });
    yamlNewline("env")(view);
    expect(view.state.doc.toString()).toBe("package: \n\nthetic");
  });
});
