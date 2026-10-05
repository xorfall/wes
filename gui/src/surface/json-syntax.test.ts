import { EditorView } from "@codemirror/view";
import { expect, it, vi } from "vitest";
import { EditorState } from "@codemirror/state";
import { ensureSyntaxTree, getIndentation } from "@codemirror/language";
import { highlightTree } from "@lezer/highlight";
import { draftExtensions, draftMarks, markRange, setDraftMarks } from "./draft-editor-wiring";
import { specExtensions } from "./spec-source-wiring";
import { jsonColors } from "./json-syntax";

const draft = () => draftExtensions({ onChange: vi.fn(), onSave: vi.fn(), onCheck: vi.fn(), onCaret: vi.fn(), typeNames: () => [] });
const spec = () => specExtensions(vi.fn(), vi.fn(), vi.fn());
function colors(state: EditorState) {
  const spans: [string, string][] = [];
  highlightTree(ensureSyntaxTree(state, state.doc.length, 100)!, jsonColors,
    (from, to, classes) => spans.push([state.sliceDoc(from, to), classes]));
  return spans;
}

it.each([draft, spec])("colors JSON keys and values through the actual editor extensions (%#)", extensions => {
  const doc = '{"method":"GET","count":-1.2e3,"required":false,"other":true,"body":null}';
  const state = EditorState.create({ doc, extensions: extensions() });
  expect(colors(state)).toEqual(expect.arrayContaining([
    ['"method"', "mono-param"], ['"GET"', "mono-provider"],
    ["-1.2e3", "mono-literal"], ["false", "mono-meta"], ["true", "mono-meta"], ["null", "mono-ref"],
    ["{", "mono-dim"],
  ]));
  expect(state.doc.toString()).toBe(doc);
});

it.each([draft, spec])("keeps escaped strings intact and recolors incomplete edits (%#)", extensions => {
  const escaped = JSON.stringify('value with "quotes" and \\slash');
  let state = EditorState.create({ doc: `{"text":${escaped},"required":`, extensions: extensions() });
  expect(colors(state)).toContainEqual([escaped, "mono-provider"]);
  state = state.update({ changes: { from: state.doc.length, insert: "false}" } }).state;
  expect(colors(state)).toContainEqual(["false", "mono-meta"]);
  const from = state.doc.toString().indexOf("false");
  state = state.update({ changes: { from, to: from + 5, insert: '"false"' } }).state;
  expect(colors(state)).toContainEqual(['"false"', "mono-provider"]);
  expect(colors(state)).not.toContainEqual(["false", "mono-meta"]);
});

it("preserves draft diagnostic offsets and indentation with syntax coloring enabled", () => {
  const doc = '{\n  "responses": []\n}';
  const from = doc.indexOf('"responses"');
  let state = EditorState.create({ doc, extensions: draft() });
  state = state.update({ effects: setDraftMarks.of({ marks: [{ from, to: from + 11, severity: "error", message: "Missing response" }] }) }).state;
  expect(colors(state)).toContainEqual(['"responses"', "mono-param"]);
  expect(getIndentation(state, doc.indexOf('  "'))).toBe(2);
  state = state.update({ changes: { from, insert: " " } }).state;
  expect(markRange(state, 0)).toEqual({ from: from + 1, to: from + 12 });
  expect(state.field(draftMarks).stale).toBe(true);
  expect(colors(state)).toContainEqual(['"responses"', "mono-param"]);
});


it("keeps captured spec inspection noneditable with syntax colors", () => {
  const state = EditorState.create({ doc: '{"provider":"fixture"}', extensions: specExtensions(vi.fn(), vi.fn(), vi.fn(), true) });
  expect(state.readOnly).toBe(true);
  expect(state.facet(EditorView.editable)).toBe(false);
  expect(colors(state)).toContainEqual(['"provider"', "mono-param"]);
  expect(EditorState.create({extensions:spec()}).readOnly).toBe(false);
});
