import { describe, expect, it } from "vitest";
import { arrangeResult, referencesOf, reconcileCells, retireCells, newCell, running, unanswered, repeating, submissionFailed, refusedBeforeAdmission, planned } from "./cells";
import { EngineRefusal } from "./engine-diagnostics";

const named = (names: Record<string, string>) => (node: string) => names[node] ?? "";

it("keeps independent result tiers and heights through a repeat, clamps sizes and rejects unknown targets", () => {
  const cell = { ...newCell("synthetic"), nodes: ["a", "b", "c"] };
  const first = arrangeResult(cell, "a", { view: "expanded", rows: 18 });
  const second = arrangeResult(first, "b", { view: "collapsed", rows: 900 });
  expect(second.results).toEqual({ a: { view: "expanded", rows: 18 }, b: { view: "collapsed", rows: 40 } });
  expect(arrangeResult(second, "unknown", { view: "expanded" })).toBe(second);
  expect(arrangeResult(second, "a", { rows: NaN })).toBe(second);
  expect(repeating(second, false).results).toEqual(second.results);
  expect(arrangeResult(second, "a", { rows: null }).results?.a).toEqual({ view: "expanded", rows: undefined });
});

describe("what a cell says it made", () => {
  /** The command already ends in the name; a chip two words later is the same fact twice. */
  it("should_SayNothing_When_TheCommandAlreadyBindsTheName", () => {
    expect(referencesOf(":read $bars > mum", ["id1"], named({ id1: "mum" }))).toEqual([]);
  });

  it("should_SayNothing_When_TheNameIsBoundWithNoSpace", () => {
    expect(referencesOf("sh run cmd:\"x\" >mum", ["id1"], named({ id1: "mum" }))).toEqual([]);
  });

  /** Nowhere else to read it: an unnamed result is still reachable, by node id. */
  it("should_GiveTheNodeId_When_TheCommandNamedNothing", () => {
    expect(referencesOf("sh run cmd:\"echo x\"", ["id1002"], named({}))).toEqual(["$id1002"]);
  });

  /** A cell rebuilt from a session, or sent by another client: the text is not yours to have read. */
  it("should_SayIt_When_TheNameIsNotInTheText", () => {
    expect(referencesOf("", ["id1"], named({ id1: "bars" }))).toEqual(["$bars"]);
  });

  it("should_SayEachOne_When_ACellMadeSeveralNodes", () => {
    const made = referencesOf("something > a", ["id1", "id2"], named({ id1: "a", id2: "b" }));

    expect(made).toEqual(["$b"]);
  });

  /** '>' inside an argument is not a binding, and a name that merely appears is not one either. */
  it("should_StillSayIt_When_TheNameAppearsWithoutBeingBound", () => {
    expect(referencesOf("sh run cmd:\"echo mum\"", ["id1"], named({ id1: "mum" }))).toEqual(["$mum"]);
  });

  it("should_NotMatchAProperPrefix_When_AnotherNameStartsTheSameWay", () => {
    expect(referencesOf("something > mummy", ["id1"], named({ id1: "mum" }))).toEqual(["$mum"]);
  });
});


it("keeps a received reply when its HTTP request reports a late failure", () => {
  const sent = newCell(":import spec url:\"http://example.invalid/docs\"");
  const answered = { ...sent, state: "answered" as const, diagnostics: [], nodes: [] };
  expect(unanswered(answered, sent.lastRun)).toBe(answered);
  expect(unanswered(sent, sent.lastRun).state).toBe("unanswered");
});

it("ignores a stale request failure after the cell starts a new attempt", () => {
  const sent = newCell(":help");
  const next = running(sent);
  expect(unanswered(next, sent.lastRun)).toBe(next);
  expect(unanswered(next, next.lastRun).state).toBe("unanswered");
});

describe("what a failed submission request means for its cell", () => {
  const notStarted = new EngineRefusal(400, "SBX004: pre-execution validation failed\n", "Submit command", "not-started");

  it("should_RecordADurableRefusal_When_TheEngineSaysTheSubmissionNeverStarted", () => {
    // Arrange
    const sent = { ...newCell("sandbox s { }"), nodes: ["previous"], pinned: true };
    // Act
    const refused = submissionFailed(sent, sent.lastRun, notStarted);
    // Assert
    expect(refused).toMatchObject({ state: "answered", submissionRefusal: "SBX004: pre-execution validation failed", nodes: ["previous"], pinned: true, id: sent.id });
    expect(refusedBeforeAdmission(refused)).toBe(true);
  });

  it("should_LeaveTheOutcomeUnknown_When_TheFailureDoesNotSayNothingStarted", () => {
    // Arrange
    const sent = newCell(":help");
    // Act / Assert
    for (const failure of [new EngineRefusal(400, "invalid", "Submit command"), new EngineRefusal(500, "boom", "Submit command"), new Error("network"), "timeout"]) {
      const result = submissionFailed(sent, sent.lastRun, failure);
      expect(result.state).toBe("unanswered");
      expect(result.submissionRefusal).toBeUndefined();
    }
  });

  it("should_KeepTheSseAnswerOrTheNewerAttempt_When_ARefusalArrivesLate", () => {
    // Arrange
    const sent = newCell(":help");
    const answered = { ...sent, state: "answered" as const };
    const next = running(sent);
    // Act / Assert
    expect(submissionFailed(answered, sent.lastRun, notStarted)).toBe(answered);
    expect(submissionFailed(next, sent.lastRun, notStarted)).toBe(next);
  });

  it("should_ClearTheRefusal_When_AFreshAttemptStartsOrAPlanArrives", () => {
    // Arrange
    const sent = newCell("sandbox s { }");
    const shown = submissionFailed(sent, sent.lastRun, notStarted);
    const event = { event: "planned", cell: shown.lastRun, text: shown.text, nodes: ["n1"], diagnostics: [] } as unknown as Parameters<typeof planned>[1];
    // Act
    const [acknowledged] = planned([shown], event);
    // Assert
    expect(shown.submissionRefusal).toBeDefined();
    expect(running(shown).submissionRefusal).toBeUndefined();
    expect(repeating(shown, false).submissionRefusal).toBeUndefined();
    expect(acknowledged!.submissionRefusal).toBeUndefined();
    expect(acknowledged!.nodes).toEqual(["n1"]);
  });

  it("should_KeepALocalFirstSubmissionRefusal_When_AReconnectSnapshotDoesNotKnowIt", () => {
    // Arrange
    const sent = newCell("sandbox s { }");
    const refused = submissionFailed(sent, sent.lastRun, notStarted);
    // Act / Assert
    expect(reconcileCells([refused], [])).toEqual([refused]);
  });
});

it("retires whole work groups including node-free attempts while retaining unrelated draft identity and arrangement", () => {
  const kept = { ...newCell("draft"), pinned: true, view: "collapsed" as const, height: 200 };
  const repeated = { ...newCell("repeat"), lastRun: "repeat", workRoot: "root", attempts: ["root", "repeat"] };
  const diagnostic = { ...newCell("bad source"), lastRun: "diagnostic", nodes: [] };
  const result = retireCells([kept, repeated, diagnostic], ["root", "repeat", "diagnostic"]);
  expect(result).toEqual([kept]);
  expect(result[0]).toBe(kept);
});

it("reconciles missed retirement on reconnect while preserving a local unconfirmed submission", () => {
  const pending = newCell("not acknowledged");
  const gone = { ...newCell("deleted"), state: "answered" as const, lastRun: "gone" };
  const kept = { ...newCell("kept"), state: "answered" as const, workRoot: "original", lastRun: "repeat", pinned: true };
  expect(reconcileCells([pending, gone, kept], ["original", "repeat"])).toEqual([pending, kept]);
});
