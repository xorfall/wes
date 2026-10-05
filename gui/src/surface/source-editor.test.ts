import { describe, expect, it } from "vitest";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { sourceExtensions } from "./calc-editor";
import { bundledPackage, readLanguage } from "./language";

const language = readLanguage(bundledPackage, "bundled");
const source = [":calc {", "\tconst x = 1;", "", ...Array.from({ length: 30 }, (_, n) => `  // line ${n}`), "  return x;", "} > x"].join("\n");

describe("the read-only calculation editor", () => {
  it("retains every source line and rejects insertion, deletion, replacement and paste transactions", () => {
    const state = EditorState.create({ doc: source, extensions: sourceExtensions(language) });
    expect(state.doc.toString()).toBe(source);
    expect(state.doc.lines).toBe(35);
    expect(state.readOnly).toBe(true);
    expect(state.facet(EditorView.editable)).toBe(false);
    for (const changes of [{ from: 0, insert: "typed" }, { from: 0, to: source.length },
      { from: 10, to: 12, insert: "replacement" }]) {
      expect(state.update({ changes, userEvent: "input.paste" }).state.doc.toString()).toBe(source);
    }
    const selected = state.update({ selection: { anchor: 0, head: source.length } }).state;
    expect(selected.sliceDoc(selected.selection.main.from, selected.selection.main.to)).toBe(source);
  });

  it("has no execution or repeat bindings", () => {
    const state = EditorState.create({ doc: source, extensions: sourceExtensions(language) });
    const keys = state.facet(keymap).flat().map(binding => binding.key);
    expect(keys).not.toContain("Mod-r");
    // Any built-in editing command is still blocked by readOnly and the change filter.
    expect(state.facet(EditorView.contentAttributes)).toContainEqual(expect.objectContaining({
      "aria-readonly": "true", "aria-label": "Command source",
    }));
  });
});
