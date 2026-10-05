import { jsonLanguage } from "@codemirror/lang-json";
import { validationFor, type DraftDiagnostic, type DraftValidation } from "./draft-api";

/** Move a user-reviewed extraction problem into metadata without rewriting contract bytes. */
export function keepAsAdvisory(text: string, validation: DraftValidation, problem: DraftDiagnostic): string | undefined {
  const index = /^#\/problems\/(0|[1-9][0-9]*)$/.exec(problem.target)?.[1];
  if (problem.code !== "DRAFT_UNRESOLVED" || index === undefined || !validationFor(text, validation)
    || !validation.diagnostics.some(d => d.code === problem.code && d.target === problem.target && d.message === problem.message)) return;
  const tree = jsonLanguage.parser.parse(text);
  let invalid = false;
  tree.iterate({ enter: node => { if (node.type.isError) invalid = true; } });
  if (invalid) return;
  const root = tree.topNode.getChild("Object");
  if (!root) return;
  const named = (name: string) => root.getChildren("Property").filter(p => {
    const key = p.getChild("PropertyName");
    return key && JSON.parse(text.slice(key.from, key.to)) === name;
  });
  const problems = named("problems"), diagnostics = named("diagnostics");
  if (problems.length !== 1 || diagnostics.length > 1) return;
  const array = problems[0]!.getChild("Array");
  if (!array) return;
  const items = [];
  for (let child = array.firstChild; child; child = child.nextSibling) {
    if (!["[", "]", ","].includes(child.name)) items.push(child);
  }
  const at = Number(index), item = items[at];
  if (!item || item.name !== "Object") return;
  const value = JSON.parse(text.slice(item.from, item.to));
  if (typeof value.message !== "string" || value.message !== problem.message || typeof value.target !== "string") return;
  const note = JSON.stringify(`${value.target}: ${value.message}`);
  const next = items[at + 1], previous = items[at - 1];
  const edits = [{ from: previous ? previous.to : item.from, to: next && !previous ? next.from : item.to, insert: "" }];
  if (diagnostics.length) {
    const notes = diagnostics[0]!.getChild("Array");
    if (!notes) return;
    const entries: unknown = JSON.parse(text.slice(notes.from, notes.to));
    if (!Array.isArray(entries) || entries.some(n => typeof n !== "string")) return;
    edits.push({ from: notes.to - 1, to: notes.to - 1, insert: `${entries.length ? ", " : ""}${note}` });
  } else {
    edits.push({ from: root.to - 1, to: root.to - 1, insert: `,\n  "diagnostics": [${note}]\n` });
  }
  return edits.sort((a, b) => b.from - a.from).reduce((source, edit) => source.slice(0, edit.from) + edit.insert + source.slice(edit.to), text);
}
