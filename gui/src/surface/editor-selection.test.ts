import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { EditorState, type Transaction } from "@codemirror/state";
import { selectAll, undo } from "@codemirror/commands";
import { EditorView, getDrawSelectionConfig } from "@codemirror/view";
import { calcExtensions, sourceExtensions } from "./calc-editor";
import { bundledPackage, readLanguage } from "./language";
import { variables } from "./cascade.test-support";

const language = readLanguage(bundledPackage, "bundled");
const extensions = () => calcExtensions({
  language, names: [], onChange() {}, onRun() {}, onRunAgain() {}, onLeave() {},
});

describe("the managed editor selection", () => {
  it("restores a visible themed cursor border above the Surface class reset", () => {
    const css = readFileSync(new URL("./editor.css", import.meta.url), "utf8");
    // Three classes outrank reset.css's class + attribute selector regardless of load order.
    expect(css).toMatch(/\.wes-terminal \.cm-editor \.cm-cursor,\s*\.wes-terminal \.cm-editor \.cm-dropCursor\s*\{\s*border-left: var\(--stroke-thin\) solid var\(--mono-ref\);\s*\}/);
    for (const palette of ["paper", "ink", "white"] as const) {
      for (const density of ["normal", "dense"] as const) {
        const tokens = variables({ palette, density });
        expect(parseFloat(tokens.get("--stroke-thin")!)).toBeGreaterThan(0);
        expect(tokens.get("--mono-ref")).toMatch(/^#[0-9A-F]{6}$/);
      }
    }
  });

  it("uses the managed renderer for editing and preserves native read-only selection", () => {
    const editing = EditorState.create({ extensions: extensions() });
    expect(getDrawSelectionConfig(editing).drawRangeCursor).toBe(false);
    expect(editing.facet(EditorView.editable)).toBe(true);
    expect(editing.readOnly).toBe(false);
    const reading = EditorState.create({ extensions: sourceExtensions(language) });
    expect(getDrawSelectionConfig(reading).drawRangeCursor).toBe(true);
    expect(reading.facet(EditorView.editable)).toBe(false);
  });

  it("keeps one collapsed selection while rapid deletions cross tokens and empty lines", () => {
    const source = "version: 1\n\nproviders:\n  demo: {}\n\n";
    let state = EditorState.create({ doc: source, selection: { anchor: source.length }, extensions: extensions() });
    const prefix = "version: 1";
    while (state.doc.length > prefix.length) {
      const head = state.selection.main.head;
      state = state.update({
        changes: { from: head - 1, to: head }, selection: { anchor: head - 1 },
        userEvent: "delete.backward",
      }).state;
      expect(state.selection.ranges).toHaveLength(1);
      expect(state.selection.main.empty).toBe(true);
      expect(state.selection.main.head).toBe(state.doc.length);
      expect(getDrawSelectionConfig(state).drawRangeCursor).toBe(false);
    }
    expect(state.doc.toString()).toBe(prefix);
    expect(state.doc.lines).toBe(1);
  });

  it("preserves selected-text replacement, composition transactions and undo", () => {
    const source = ":calc {\n  return 1;\n}";
    let state = EditorState.create({ doc: source, extensions: extensions() });
    const target = { get state() { return state; }, dispatch(transaction: Transaction) { state = transaction.state; } };
    expect(selectAll(target)).toBe(true);
    expect(state.sliceDoc(state.selection.main.from, state.selection.main.to)).toBe(source);
    state = state.update(state.replaceSelection("ö"), { userEvent: "input.type.compose" }).state;
    expect(state.doc.toString()).toBe("ö");
    expect(state.selection.ranges).toHaveLength(1);
    expect(state.selection.main.head).toBe(1);
    expect(undo(target)).toBe(true);
    expect(state.doc.toString()).toBe(source);
    expect(state.sliceDoc(state.selection.main.from, state.selection.main.to)).toBe(source);
  });
});
