import { describe, expect, it } from "vitest";
import { EditorState } from "@codemirror/state";
import { insertNewlineAndIndent } from "@codemirror/commands";
import { indentRange, indentUnit } from "@codemirror/language";
import { CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import { DRAFT_INDENT, draftCompletion, draftIndentAt, draftIndentation, draftMarks, markRange, setDraftMarks } from "./draft-editor-wiring";

const unit = " ".repeat(DRAFT_INDENT);

function complete(doc: string, explicit: boolean, typeNames: readonly string[] = ["ItemPage", "Item"]): CompletionResult | null {
  const pos = doc.indexOf("|");
  const text = doc.replace("|", "");
  const state = EditorState.create({ doc: text, selection: { anchor: pos } });
  return draftCompletion(() => typeNames)(new CompletionContext(state, pos, explicit)) as CompletionResult | null;
}
const labels = (result: CompletionResult | null) => result?.options.map(o => o.label) ?? [];

describe("draft completion", () => {
  it("should_OfferHttpMethods_When_TheCaretIsInAMethodValue", () => {
    expect(labels(complete('{"operations":[{"method": "P|"}]}', false))).toEqual(expect.arrayContaining(['"POST"', '"PUT"', '"PATCH"']));
  });

  it("should_OfferNativeAndDraftTypeNames_When_TheCaretIsInATypeValue", () => {
    const result = complete('{"operations":[{"responses":[{"type": "|"}]}]}', true);
    expect(labels(result)).toEqual(expect.arrayContaining(['"Text"', '"Int"', '"Unknown"', '"List<>"', '"Union<,>"', '"ItemPage"', '"Item"', "null"]));
    const field = complete('{"types":{"Item":{"base":"Record","fields":{"id":{"type": "T|"}}}}}', false);
    expect(labels(field)).toContain('"Text"');
  });

  it("should_OfferGrammarKeysByContainer_When_TheCaretIsInKeyPosition", () => {
    expect(labels(complete('{"operations":[{"responses":[{ "|', true))).toEqual(['"status"', '"mediaType"', '"type"']);
    expect(labels(complete('{"operations":[{ "ro|', false))).toContain('"route"');
    expect(labels(complete('{ "|', true))).toEqual(['"draftVersion"', '"provider"', '"types"', '"operations"', '"problems"', '"diagnostics"']);
  });

  it("should_OfferStatusAndMediaOnlyWhenAsked_When_NothingHasBeenTyped", () => {
    // Nothing opens by itself: an empty status slot stays silent until ⌘␣.
    expect(complete('{"operations":[{"responses":[{"status": |}]}]}', false)).toBeNull();
    expect(labels(complete('{"operations":[{"responses":[{"status": |}]}]}', true))).toEqual(["200", "201", "202", "204", "null"]);
    expect(labels(complete('{"operations":[{"responses":[{"mediaType": |}]}]}', true))).toEqual(['"application/json"', "null"]);
    // A typed prefix is the user's own choice of where to look.
    expect(labels(complete('{"operations":[{"responses":[{"status": 20|}]}]}', false))).toContain("201");
  });

  it("should_ReplaceTheWholeQuotedToken_When_TheClosingQuoteIsAlreadyThere", () => {
    const result = complete('{"operations":[{"method": "G|ET"}]}', false)!;
    const doc = '{"operations":[{"method": "GET"}]}';
    expect(result.from).toBe(doc.indexOf('"GET"'));
    expect(result.to).toBe(doc.indexOf('"GET"') + 5);
  });

  it("should_OfferNothing_When_TheSlotIsFreeText", () => {
    expect(complete('{"operations":[{"summary": "Li|"}]}', true)).toBeNull();
  });
});

describe("draft indentation", () => {
  const pressEnter = (doc: string, at = doc.length) => {
    const state = EditorState.create({ doc, selection: { anchor: at }, extensions: [indentUnit.of(unit), draftIndentation] });
    let next = state;
    insertNewlineAndIndent({ state, dispatch: tr => { next = tr.state; } });
    return next.doc.toString();
  };

  it("should_IndentOneLevel_When_EnterFollowsAnOpeningBracket", () => {
    expect(pressEnter('{\n  "operations": [')).toBe(`{\n  "operations": [\n${unit}${unit}`);
  });

  it("should_OpenABlankLineAndKeepTheCloser_When_EnterIsBetweenBrackets", () => {
    const doc = '{\n  "responses": []\n}';
    const at = doc.indexOf("[]") + 1;
    expect(pressEnter(doc, at)).toBe(`{\n  "responses": [\n${unit}${unit}\n${unit}]\n}`);
  });

  it("should_StepOut_When_ALineBeginsWithAClosingBracket", () => {
    const doc = '{\n  "a": [\n    1\n    ]\n}';
    expect(draftIndentAt(doc, doc.indexOf("    ]"))).toBe(DRAFT_INDENT);
    expect(draftIndentAt('{ "a": "{[" ', 3)).toBe(DRAFT_INDENT);
  });

  it("should_ReindentEveryLineWithoutTouchingValues_When_Formatting", () => {
    const doc = '{\n"a": [\n{ "b": "  keep  {" },\n      2\n],\n        "c": null\n}';
    const state = EditorState.create({ doc, extensions: [indentUnit.of(unit), draftIndentation] });
    const formatted = state.update({ changes: indentRange(state, 0, doc.length) }).state.doc.toString();
    expect(formatted).toBe('{\n  "a": [\n    { "b": "  keep  {" },\n    2\n  ],\n  "c": null\n}');
  });
});

describe("backend marks", () => {
  it("should_TurnStaleAndFollowTheText_When_TheDocumentIsEditedAfterAResult", () => {
    const doc = '{ "responses": [] }';
    let state = EditorState.create({ doc, extensions: [draftMarks] });
    const from = doc.indexOf('"responses"');
    state = state.update({ effects: setDraftMarks.of({ marks: [{ from, to: from + 11, severity: "error", message: "no success status" }] }) }).state;
    expect(state.field(draftMarks).stale).toBe(false);
    expect(markRange(state, 0)).toEqual({ from, to: from + 11 });
    state = state.update({ changes: { from: 0, insert: "  " } }).state;
    expect(state.field(draftMarks).stale).toBe(true);
    expect(markRange(state, 0)).toEqual({ from: from + 2, to: from + 13 });
    expect(markRange(state, 3)).toBeUndefined();
  });

  it("should_ClampOffsets_When_AResultNamesPositionsBeyondTheText", () => {
    let state = EditorState.create({ doc: "{", extensions: [draftMarks] });
    state = state.update({ effects: setDraftMarks.of({ marks: [{ from: 5, to: 9, severity: "error", message: "unexpected end" }] }) }).state;
    expect(markRange(state, 0)).toEqual({ from: 1, to: 1 });
  });
});
