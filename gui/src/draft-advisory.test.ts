import { expect, it } from "vitest";
import { keepAsAdvisory } from "./draft-advisory";
import { sha256Hex, type DraftDiagnostic, type DraftValidation } from "./draft-api";

const problem = (message: string) => ({ target: "#/operations/0", message });
function checked(text: string, at = 0) {
  const d: DraftDiagnostic = { severity: "error", code: "DRAFT_UNRESOLVED", target: `#/problems/${at}`, message: JSON.parse(text).problems[at].message, fix: "Resolve", from: 0, to: 0, line: 1 };
  const v: DraftValidation = { hash: sha256Hex(text), valid: false, diagnostics: [d], preview: JSON.parse(text) };
  return { d, v };
}

it.each([0, 1, 2])("moves only problem %s and preserves unrelated bytes and precise numbers", at => {
  const doc = `{"types":{"Bound":{"base":"Int","max":9007199254740993}}, "problems": ${JSON.stringify([problem("one"), problem("two"), problem("three")])}, "diagnostics": ["existing"]}`;
  const { d, v } = checked(doc, at);
  const next = keepAsAdvisory(doc, v, d)!;
  expect(next).toContain('"types":{"Bound":{"base":"Int","max":9007199254740993}}');
  expect(JSON.parse(next).problems).toEqual(JSON.parse(doc).problems.filter((_: unknown, i: number) => i !== at));
  expect(JSON.parse(next).diagnostics).toEqual(["existing", `#/operations/0: ${d.message}`]);
});

it.each([{}, { diagnostics: [] }])("creates or appends advisory metadata for the last problem (%#)", extra => {
  const doc = JSON.stringify({ ...extra, problems: [problem('simulated "write" 🚀')] });
  const { d, v } = checked(doc);
  expect(JSON.parse(keepAsAdvisory(doc, v, d)!)).toEqual({ diagnostics: ['#/operations/0: simulated "write" 🚀'], problems: [] });
});

it("refuses stale checks, structural errors, mismatched entries and malformed advisory containers", () => {
  const doc = JSON.stringify({ problems: [problem("simulated")] });
  const { d, v } = checked(doc);
  expect(keepAsAdvisory(doc + " ", v, d)).toBeUndefined();
  expect(keepAsAdvisory(doc, v, { ...d, code: "DRAFT_STATUS" })).toBeUndefined();
  expect(keepAsAdvisory(doc, v, { ...d, message: "changed" })).toBeUndefined();
  expect(keepAsAdvisory(doc, { ...v, diagnostics: [] }, d)).toBeUndefined();
  for (const diagnostics of [null, "note", [42]]) {
    const source = JSON.stringify({ problems: [problem("simulated")], diagnostics });
    const { d, v } = checked(source);
    expect(keepAsAdvisory(source, v, d)).toBeUndefined();
  }
});
