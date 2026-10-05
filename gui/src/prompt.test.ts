import { describe, expect, it } from "vitest";
import { atHistoryBoundary, moveRecall, recallEntries } from "./prompt";
import { expand, focusOn, UNFOCUSED, type Focus } from "./focus";

describe("restored workspace command recall", () => {
  it("hydrates from replay, walks to the oldest and restores the unfinished draft", () => {
    expect(moveRecall([], UNFOCUSED, "a", "draft", undefined, -1).text).toBe("draft");
    const commands = [":list providers", ":inspect $prices", ":inspect $prices"];
    const latest = moveRecall(commands, UNFOCUSED, "a", "draft", undefined, -1);
    expect(latest.text).toBe(":inspect $prices");
    const oldest = moveRecall(commands, UNFOCUSED, "a", latest.text, latest.recall, -1);
    expect(oldest.text).toBe(":list providers");
    expect(moveRecall(commands, UNFOCUSED, "a", oldest.text, oldest.recall, -1).text).toBe(oldest.text);
    const forward = moveRecall(commands, UNFOCUSED, "a", oldest.text, oldest.recall, 1);
    const draft = moveRecall(commands, UNFOCUSED, "a", forward.text, forward.recall, 1);
    expect(draft).toEqual({ text: "draft" });
    expect(moveRecall(commands, UNFOCUSED, "a", draft.text, draft.recall, 1)).toEqual(draft);
  });
  it("freezes the walk during incremental replay but discards it on workspace change", () => {
    const first = moveRecall(["one", "two"], UNFOCUSED, "a", "draft", undefined, -1);
    expect(moveRecall(["one", "two", "three"], UNFOCUSED, "a", first.text, first.recall, -1).text).toBe("one");
    expect(moveRecall(["new"], UNFOCUSED, "b", "", first.recall, -1).text).toBe("new");
  });
  it("reconstructs focused fragments without double expansion", () => {
    const focus = focusOn('sh run cmd:"_"') as Focus;
    expect(recallEntries(['http request url:x', 'sh run cmd:"ls -l"', ':list nodes'], focus)).toEqual(["ls -l", ":list nodes"]);
    const typed = recallEntries(['sh run cmd:"ls -l"'], focus)[0]!;
    expect(expand(focus, typed)).toBe('sh run cmd:"ls -l"');
    expect(recallEntries([':package load path:"types.yaml"'], focusOn(':package load path:_') as Focus)).toEqual(['"types.yaml"']);
    expect(recallEntries(["", "a", "a", "b", "a"])).toEqual(["a", "b", "a"]);
    expect(recallEntries(Array.from({length: 600}, (_, n) => String(n)))).toHaveLength(500);
  });
  it("uses only outer multiline boundaries and never hijacks a selection", () => {
    expect(atHistoryBoundary("one\ntwo", 1, 1, -1)).toBe(true);
    expect(atHistoryBoundary("one\ntwo", 5, 5, -1)).toBe(false);
    expect(atHistoryBoundary("one\ntwo", 1, 1, 1)).toBe(false);
    expect(atHistoryBoundary("one\ntwo", 5, 5, 1)).toBe(true);
    expect(atHistoryBoundary("one", 0, 3, -1)).toBe(false);
  });
});
