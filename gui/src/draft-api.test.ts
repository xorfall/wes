import { describe, expect, it } from "vitest";
import { cursorAt, draftProvenance, evidencePointerAt, isManual, offsetOfPointer, orderProblems, readPreview, sha256Hex, validationFor, type DraftDiagnostic } from "./draft-api";

const text = `{
  "draftVersion": 1,
  "provider": "items",
  "types": { "Item": { "base": "Record", "fields": { "naïve/ünïcode": { "type": "Text", "optional": false } } } },
  "operations": [
    { "path": ["listItems"], "method": "GET", "route": "/items", "summary": "Lïst ☕ items", "auth": [], "parameters": [], "responses": [] },
    { "path": ["createItem"], "method": "POST", "route": "/items", "auth": [], "parameters": [], "responses": [{ "status": null, "mediaType": null }] }
  ],
  "problems": [{ "target": "#/operations/0", "message": "unresolved requirement" }]
}`;

describe("exact-text identity", () => {
  it("should_HashUtf8Bytes_When_KnownVectorsAreGiven", () => {
    expect(sha256Hex("")).toBe("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    expect(sha256Hex("abc")).toBe("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    expect(sha256Hex("a".repeat(1000))).toBe("41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
    expect(sha256Hex("é")).toBe(sha256Hex("é"));
    expect(sha256Hex("é")).not.toBe(sha256Hex("é"));
  });

  it("should_AcceptAValidation_When_ItsHashNamesExactlyTheseBytes", () => {
    const validation = { hash: `sha256:${sha256Hex(text).toUpperCase()}`, valid: true, diagnostics: [], preview: null };
    expect(validationFor(text, validation)).toBe(true);
    expect(validationFor(`${text} `, validation)).toBe(false);
    expect(validationFor(text, { ...validation, hash: 42 as unknown as string })).toBe(false);
    expect(validationFor(text, undefined)).toBe(false);
  });
});

describe("tolerant JSON shape", () => {
  it("should_NameTheContainerAndSlot_When_TheCaretIsInsideAnOperation", () => {
    const at = text.indexOf('"GET"') + 1;
    expect(cursorAt(text, at)).toEqual({ path: ["operations", 0, "method"], position: "value", inString: true });
    const keyAt = text.indexOf('"route"') + 2;
    expect(cursorAt(text, keyAt)).toMatchObject({ path: ["operations", 0], position: "key" });
    const second = text.indexOf('"status"') + 1;
    expect(cursorAt(text, second)).toMatchObject({ path: ["operations", 1, "responses", 0], position: "key" });
  });

  it("should_StillAnswer_When_TheTextDoesNotParse", () => {
    const broken = '{ "operations": [ { "method": "G';
    expect(cursorAt(broken, broken.length)).toEqual({ path: ["operations", 0, "method"], position: "value", inString: true });
    expect(cursorAt("", 0).position).toBe("none");
  });

  it("should_FindExactUtf16Offsets_When_TextHasNonAsciiCharacters", () => {
    expect(offsetOfPointer(text, "#/operations/1")).toBe(text.indexOf('{ "path": ["createItem"]'));
    expect(offsetOfPointer(text, "#/types/Item/fields/naïve~1ünïcode")).toBe(text.indexOf('{ "type": "Text"'));
    // A missing member anchors at its nearest existing parent.
    expect(offsetOfPointer(text, "#/operations/0/responses/0")).toBe(text.indexOf("[]", text.indexOf('"responses"')));
    expect(offsetOfPointer(text, "#/nothing")).toBe(0);
  });

  it("should_TrimTheCaretPointer_When_EvidenceIsRecordedPerFactNotPerCharacter", () => {
    expect(evidencePointerAt(text, text.indexOf('"mediaType"') + 3)).toBe("#/operations/1/responses/0");
    expect(evidencePointerAt(text, text.indexOf('"optional"') + 3)).toBe("#/types/Item/fields/naïve~1ünïcode");
    expect(evidencePointerAt(text, 3)).toBeUndefined();
  });
});

describe("the preview", () => {
  it("should_ReadAMissingResponseAndUnknownFacts_When_ThePreviewIsIncomplete", () => {
    const preview = readPreview(JSON.parse(text))!;
    expect(preview.operations.map(o => [o.name, o.method, o.responses.length])).toEqual([["listItems", "GET", 0], ["createItem", "POST", 1]]);
    expect(preview.operations[1]!.responses[0]).toEqual({ status: null, mediaType: null, type: undefined });
    expect(preview.problems).toEqual([{ target: "#/operations/0", message: "unresolved requirement" }]);
  });

  it("should_NeverThrow_When_ThePreviewHasTheWrongShape", () => {
    for (const bad of [null, 1, "x", [], { operations: "no" }, { operations: [null, 3, { path: 7, responses: [null] }] }, { types: [] }]) {
      expect(() => readPreview(bad)).not.toThrow();
    }
    expect(readPreview([])).toBeUndefined();
    const responses = readPreview({operations:[{responses:[{type:42},{type:false},{type:null}]}]})!.operations[0]!.responses;
    expect(responses.map(r=>r.type)).toEqual([undefined, undefined, null]);
    expect(readPreview({ operations: [null] })!.operations[0]!.name).toBe("operation 0");
  });
});

describe("problems and evidence", () => {
  it("should_OrderErrorsBeforeWarningsAndKeepTheBackendIndex_When_Numbering", () => {
    const d = (severity: "error" | "warning", target: string): DraftDiagnostic => ({ severity, code: "c", target, message: "m", fix: "f", from: 0, to: 1, line: 1 });
    const ordered = orderProblems([d("warning", "#/operations/0/auth"), d("error", "#/operations/1/responses"), d("error", "#/types/Item/fields/id")], readPreview(JSON.parse(text)));
    expect(ordered.map(p => [p.label, p.index, p.operation, p.field])).toEqual([["E1", 1, "createItem", "responses"], ["E2", 2, "Item", "id"], ["W1", 0, "listItems", "auth"]]);
  });

  it("should_LetTheBackendStatusWin_When_TheSourceClaimsToBeCurrent", () => {
    const source = { provenance: { version: 1, status: "current", entries: [] } };
    expect(draftProvenance({ source, status: "stale", manualTargets: [] })!.status).toBe("stale");
    expect(draftProvenance({ source: { provenance: { version: 1, status: "stale", entries: [] } }, status: "current", manualTargets: [] })!.status).toBe("current");
    expect(draftProvenance({ source: {}, status: "current", manualTargets: [] })).toBeUndefined();
  });

  it("should_MatchManualTargetsUpAndDownThePointer_When_AskingWhoSuppliedAFact", () => {
    const manual = ["#/operations/0/responses/0"];
    expect(isManual(manual, "#/operations/0/responses/0")).toBe(true);
    expect(isManual(manual, "#/operations/0/responses/0/status")).toBe(true);
    expect(isManual(manual, "#/operations/0/responses")).toBe(true);
    expect(isManual(manual, "#/operations/0/responses/01")).toBe(false);
    expect(isManual(manual, "#/operations/1/responses/0")).toBe(false);
  });
});
