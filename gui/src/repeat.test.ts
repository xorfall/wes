import { expect, it } from "vitest";
import { newCell, planned, repeating, unanswered } from "./cells";
import type { Event } from "./protocol";

const event = (cell: string, repeatOf?: string): Extract<Event, { event: "planned" }> => ({
  event: "planned", cell, text: "catalog echo value:hello", nodes: ["id1"], repeatOf,
});

it("eighty acknowledged attempts are one work item on initial observation and reconnect", () => {
  let cells = planned([], event("original"));
  for (let i = 1; i < 80; i++) cells = planned(cells, event(`repeat-${i}`, "original"));
  expect(cells).toHaveLength(1);
  expect(cells[0]).toMatchObject({ id: "original", originAttempt: "original", lastRun: "repeat-79", nodes: ["id1"] });
  expect(cells[0]!.attempts).toHaveLength(80);
  cells = planned(cells, event("original"));
  for (let i = 1; i < 80; i++) cells = planned(cells, event(`repeat-${i}`, "original"));
  expect(cells).toHaveLength(1);
  expect(cells[0]!.attempts).toHaveLength(80);
  expect(cells[0]!.lastRun).toBe("repeat-79");
});

it("pending repeat retains the work identity and arrangements without accepting old callbacks", () => {
  const initial = { ...newCell("x"), state: "answered" as const, nodes: ["id1"], pinned: true, height: 320 };
  const next = repeating(initial, true);
  expect(next).toMatchObject({ id: initial.id, originAttempt: initial.lastRun, pinned: true, height: 320, nodes: ["id1"] });
  expect(planned([next], event(initial.lastRun))[0]).toBe(next);
  expect(unanswered(next, initial.lastRun)).toBe(next);
  const fork = newCell(initial.text);
  expect(fork.id).not.toBe(initial.id);
  expect(fork.originAttempt).toBeUndefined();
});

it("a refused repeat keeps the existing node visible and is not a new work item", () => {
  const initial = planned([], event("original"))[0]!;
  const next = repeating(initial, true);
  const cells = planned([next], { ...event(next.lastRun, "original"), nodes: [], failure: "definition changed" });
  expect(cells).toHaveLength(1);
  expect(cells[0]).toMatchObject({ nodes: ["id1"], state: "answered" });
});

it("revisions stay in one arranged cell and delayed or refused revisions preserve the latest definition", async () => {
  const { revising } = await import("./cells");
  const initial = { ...planned([], event("original"))[0]!, pinned: true, height: 340 };
  const pending = revising(initial, "catalog echo value:new");
  expect(pending.text).toBe(initial.text);
  const revised = { ...event(pending.lastRun), workOf: "original", revisionOf: "original", revisionAccepted: true,
    text: pending.revisionText!, nodes: ["id2"] };
  let cells = planned([pending], revised);
  expect(cells[0]).toMatchObject({ id: "original", pinned: true, height: 340, originAttempt: pending.lastRun, text: revised.text, nodes: ["id2"] });
  expect(planned(cells, event("original"))).toBe(cells);
  cells = planned(cells, { ...event("refused"), workOf: "original", revisionOf: pending.lastRun, revisionAccepted: false, nodes: [], text: "bad source" });
  expect(cells[0]).toMatchObject({ originAttempt: pending.lastRun, text: revised.text, nodes: ["id2"], revisionText: "bad source" });
  const repeat = repeating(cells[0]!, true);
  expect(repeat.revisionOf).toBeUndefined();
  expect(repeat.originAttempt).toBe(pending.lastRun);
});
