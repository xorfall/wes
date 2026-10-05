import { describe, expect, it } from "vitest";
import { EditorState } from "@codemirror/state";
import { insertNewlineAndIndent } from "@codemirror/commands";
import { indentUnit } from "@codemirror/language";
import { calcIndentation } from "./calc-editor";
import { INDENT } from "./calc-mode";
import { bundledPackage, readLanguage } from "./language";

const language = readLanguage(bundledPackage, "engine");
const unit = " ".repeat(INDENT);

function pressEnter(doc: string, at = doc.length): string {
  const state = EditorState.create({ doc, selection: { anchor: at }, extensions: [indentUnit.of(unit), calcIndentation(language)] });
  let next = state;
  insertNewlineAndIndent({ state, dispatch: (transaction) => { next = transaction.state; } });
  return next.doc.toString();
}

describe("indentation in the editor", () => {
  it("should_IndentTheNewLine_When_EnterFollowsAnOpeningBrace", () => {
    // Arrange / Act
    const after = pressEnter(":calc {");
    // Assert: the first line under `{` is already one level in
    expect(after).toBe(`:calc {\n${unit}`);
  });

  it("should_KeepTheLevel_When_EnterFollowsAnIndentedLine", () => {
    // Arrange / Act
    const after = pressEnter(`:calc {\n${unit}let a = 1;`);
    // Assert
    expect(after).toBe(`:calc {\n${unit}let a = 1;\n${unit}`);
  });

  it("should_StepBackOut_When_EnterFollowsAClosingBrace", () => {
    // Arrange / Act
    const after = pressEnter(`:calc {\n${unit}return 1;\n}`);
    // Assert
    expect(after).toBe(`:calc {\n${unit}return 1;\n}\n`);
  });
});
