import { describe, expect, it } from "vitest";
import { decodeEvent } from "./protocol-decode";
import { apply, emptyWorkspace, type Workspace, type WorkspaceNode } from "./workspace";
import { newCell, type Cell } from "./cells";
import { lineText } from "./surface/MonoLine";
import { lifetimeActive, progressRows } from "./surface/record-progress";
import { recordingActions } from "./surface/RecordingControls";
import { identityOf, joined, runActiveOf, verdictOf } from "./surface/session-model";
import { stateOf as graphState } from "./surface/graph-model";
import { runAvailability } from "./surface/Cell";
import { resumeRefusal } from "./surface/ScanReceiptDetails";
import type { ScanReceipt } from "./surface/scan-receipt";

/*
 * Independently authored synthetic runs of owned-lifetime work: invented node ids, run identities
 * and commands. They name no real source, recording or store.
 */

const RUN = "00000000-0000-4000-8000-0000000000a1";
const LATER = "00000000-0000-4000-8000-0000000000a2";
const created = (over: Record<string, unknown> = {}) => ({
  event: "created", node: "n4", name: "followed", command: ":synthetic", dependsOn: [], dependencyLifetime: "continuous",
  interactive: false, run: RUN, lifetimeActive: true, ...over,
});
const ready = { event: "node", node: "n4", state: "ready" };
const scanProgress = (run = RUN) => ({ event: "node-progress", node: "n4", run, progress: { kind: "records", phase: "processing", counters: null } });
const run = (...events: unknown[]): Workspace => events.reduce<Workspace>((workspace, event) => apply(workspace, decodeEvent(event)), emptyWorkspace);
const node = (...events: unknown[]): WorkspaceNode => run(...events).nodes[0]!;
const cell = (ids: readonly string[]): Cell => ({ ...newCell(":synthetic"), state: "answered", nodes: [...ids] });
const NOW = new Date("2026-01-01T00:00:00Z");
const fields = (subject: WorkspaceNode) => verdictOf("default", cell([subject.id]), [subject], NOW);
const verdict = (subject: WorkspaceNode) => lineText(joined(fields(subject)));
const stateWord = (subject: WorkspaceNode) => verdict(subject).split(" · ")[0];
const first = (subject: WorkspaceNode) => lineText(progressRows(subject)![0]!);
/** As the session model passes it: activity from the actual nodes, never from the verdict's words. */
const availability = (...subjects: WorkspaceNode[]) => runAvailability("default",
  verdictOf("default", cell(subjects.map(it => it.id)), subjects, NOW), subjects.map(it => identityOf(it)),
  runActiveOf(cell(subjects.map(it => it.id)), subjects));

describe("lifetimeActive on created", () => {
  it.each([[true], [false], [null]])("accepts %s", (value) => {
    expect(decodeEvent(created({ lifetimeActive: value }))).toMatchObject({ lifetimeActive: value });
  });

  it("keeps an absent field absent and the rest of created as the engine wrote it", () => {
    const plain = (({ lifetimeActive: _l, ...rest }) => rest)(created());
    expect(decodeEvent(plain)).toEqual(plain);
  });

  it.each([["text", "true"], ["a number", 1], ["an object", { active: true }], ["an array", [true]]])("refuses %s", (_name, value) => {
    expect(() => decodeEvent(created({ lifetimeActive: value }))).toThrow();
  });

  it("decodes it beside a recording control without letting either stand in for the other", () => {
    const control = { active: true, statusAvailable: true, stopAvailable: false, discardAvailable: false };
    expect(decodeEvent(created({ recordingControl: control, lifetimeActive: false }))).toMatchObject({ recordingControl: control, lifetimeActive: false });
    expect(() => decodeEvent(created({ recordingControl: control, lifetimeActive: "yes" }))).toThrow();
  });
});

describe("a ready scan whose run is still open", () => {
  const open = () => node(created(), ready, scanProgress());

  it("is current, says analyzing, draws running and can be cancelled but not repeated", () => {
    const subject = open();
    expect(subject.state).toBe("ready");
    expect(lifetimeActive(subject)).toBe(true);
    expect(first(subject)).toMatch(/^processing · /);
    expect(stateWord(subject)).toBe("analyzing");
    expect(graphState(subject, undefined)).toBe("running");
    expect(availability(subject).working).toBe(true);
  });

  it("offers no recording command and no continuation while open", () => {
    expect(recordingActions(open())).toBeUndefined();
    expect(resumeRefusal({ durableResume: true } as ScanReceipt, open())).toBe("the analysis is still running");
  });

  it("says only active before its first report, and is still cancellable rather than repeatable", () => {
    const subject = node(created(), ready);
    expect(lifetimeActive(subject)).toBe(true);
    expect(stateWord(subject)).toBe("active");
    expect(progressRows(subject)).toBeUndefined();
    expect(availability(subject).working).toBe(true);
  });

  it("makes a cell of several nodes cancellable through the ordinary cell action", () => {
    const finite = node({ ...created({ node: "n5", lifetimeActive: null }) }, { event: "node", node: "n5", state: "ready" });
    const open = node(created(), ready, scanProgress());
    expect(stateWord(open)).toBe("analyzing");
    expect(availability(open, finite).working).toBe(true);
    expect(availability(finite).working).toBe(false);
  });

  it("never lets a displayed word grant or withhold lifecycle actions", () => {
    const finished = node(created({ lifetimeActive: false }), ready, scanProgress());
    const said = (word: string) => [{ slot: "state" as const, segments: [{ text: word, role: "mono-meta" as const }], keep: true }];
    for (const word of ["analyzing", "recording", "active", "waiting", "running"]) {
      // The model says nothing is active: no word makes it cancellable.
      expect(runAvailability("default", said(word), [identityOf(finished)], false).working).toBe(false);
    }
    // The model says it is active: an "ok" word does not make it repeatable.
    expect(runAvailability("default", said("ok"), [identityOf(open())], true).working).toBe(true);
    // Static fixtures without the semantic signal read glyphs, still never words.
    expect(runAvailability("default", said("analyzing"), [identityOf(finished)]).working).toBe(false);
  });
});

describe("ending, staleness, withdrawal and restore", () => {
  it("clears when the engine announces the run finished", () => {
    const subject = node(created(), ready, scanProgress(), created({ lifetimeActive: false }));
    expect(lifetimeActive(subject)).toBe(false);
    expect(first(subject)).toMatch(/^last reported processing/);
    expect(stateWord(subject)).toBe("ok");
    expect(graphState(subject, undefined)).toBe("ok");
    expect(availability(subject).working).toBe(false);
  });

  it("clears on null, an absent field, a failure or a cancellation", () => {
    const plain = (({ lifetimeActive: _l, ...rest }) => rest)(created());
    for (const after of [created({ lifetimeActive: null }), plain]) expect(lifetimeActive(node(created(), ready, after, ready))).toBe(false);
    expect(lifetimeActive(node(created(), ready, { event: "failed", node: "n4", reason: "synthetic failure" }))).toBe(false);
    expect(lifetimeActive(node(created(), ready, { event: "cancelled", node: "n4", code: "cancelled", reason: "synthetic cancel" }))).toBe(false);
  });

  it("never lets an open lifetime or a report from an older run speak for the current one", () => {
    // The node moved to a later run that the engine has not called open.
    const moved = node(created(), ready, scanProgress(), created({ run: LATER, lifetimeActive: false }), ready);
    expect(lifetimeActive(moved)).toBe(false);
    // A late report for the earlier run is not this run's.
    const late = node(created(), ready, created({ run: LATER }), ready, scanProgress(RUN));
    expect(lifetimeActive(late)).toBe(true);
    expect(progressRows(late)).toBeUndefined();
    expect(stateWord(late)).toBe("active");
    // Even if the node's run changed by other means, a lifetime said for another run is none.
    expect(lifetimeActive({ ...node(created(), ready), run: LATER })).toBe(false);
  });

  it("drops the open lifetime and its progress on withdrawal, and a late report does not bring them back", () => {
    const withdrawn = node(created(), ready, scanProgress(), { event: "result-access", node: "n4", readable: false }, scanProgress());
    expect(withdrawn.openLifetime).toBeUndefined();
    expect(withdrawn.progress).toBeUndefined();
    expect(lifetimeActive(withdrawn)).toBe(false);
    expect(stateWord(withdrawn)).not.toBe("analyzing");
    expect(graphState(withdrawn, undefined)).not.toBe("running");
    // A later announcement for the same withdrawn run does not reopen it either.
    expect(lifetimeActive(node(created(), ready, { event: "result-access", node: "n4", readable: false }, created()))).toBe(false);
  });

  it("is inert when restored: a reopened projection is never open, whatever its progress said", () => {
    for (const restored of [false, null]) {
      const subject = node(created({ lifetimeActive: restored }), ready, scanProgress());
      expect(lifetimeActive(subject)).toBe(false);
      expect(first(subject)).toMatch(/^last reported processing/);
      expect(availability(subject).working).toBe(false);
    }
  });
});

describe("a recording announced with both signals", () => {
  const control = { active: true, statusAvailable: true, stopAvailable: true, discardAvailable: false };
  const recordingProgress = { event: "node-progress", node: "n4", run: RUN, progress: { kind: "recording", phase: "processing", counters: null } };

  it("says recording and keeps its own status/stop authority on the exact run", () => {
    const subject = node(created({ recordingControl: control }), ready, recordingProgress);
    expect(stateWord(subject)).toBe("recording");
    expect(recordingActions(subject)).toEqual({ status: true, stop: true, discard: false, run: RUN });
    expect(graphState(subject, undefined)).toBe("running");
    // An open recording is not repeated or branched; the ordinary cancel is offered as generic
    // owned-run cancellation, while Stop stays its own reviewed command on the exact run.
    expect(availability(subject).working).toBe(true);
  });

  it("keeps finished-writer status authority after its lifetime closes", () => {
    const finished = { active: false, statusAvailable: true, stopAvailable: false, discardAvailable: false };
    const subject = node(created({ recordingControl: control }), ready, recordingProgress, created({ recordingControl: finished, lifetimeActive: false }));
    expect(lifetimeActive(subject)).toBe(false);
    expect(recordingActions(subject)).toEqual({ status: true, stop: false, discard: false, run: RUN });
    expect(stateWord(subject)).toBe("ok");
  });
});
