import { expect, it } from "vitest";
import { emptyCatalogue } from "../vocabulary";
import { read } from "./commands";
import { promptCompletion, suggestionLine, suggestionSource } from "./prompt-complete";
import { lineText } from "./MonoLine";

it("preserves raw alias source, including quotes, newlines and a trailing split word", () => {
  for (const command of ['/alias say = :calc { return "_"; }', "/alias script = :calc {\n return _;\n}", "/alias show = :list split", "/unalias show", "/alias"]) {
    expect(read(command)).toEqual({ kind: "aliases", command });
  }
});

it("completes personal commands and aliases with honest source labels", () => {
  const aliases = { twice: ":calc { return 2 * (_); }", fetch: ":calc 1" };
  const complete = (line: string, catalogue = emptyCatalogue) => promptCompletion({ line, caret: line.length, catalogue, aliases, names: [] });
  expect(complete("/al").items).toEqual([{ text: "/alias", kind: "client", separate: true }]);
  expect(complete("/un").items).toEqual([{ text: "/unalias", kind: "client", separate: true }]);
  const suggestion = complete("tw").items[0]!;
  expect(suggestion.kind).toBe("alias");
  expect(lineText(suggestionLine(suggestion, true))).toContain("personal alias");
  expect(lineText(suggestionSource(["alias"]))).toBe("from your personal aliases");
  expect(lineText(suggestionSource(["alias", "provider"]))).toBe("from this workspace and your personal aliases");
  expect(lineText(suggestionSource(["client"]))).toBe("from the client's commands");
  const changed = { ...emptyCatalogue, templates: [{ name: "fetch", body: "", parameters: [] }] };
  expect(complete("fet", changed).items.some(item => item.kind === "alias")).toBe(false);
  // A name that now collides still needs to be removable.
  expect(complete("/unalias fe", changed)).toEqual({ from: 9, items: [
    { text: "fetch", kind: "alias", detail: ":calc 1", separate: true },
  ] });
});
