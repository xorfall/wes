import { describe, expect, it } from "vitest";
import { create } from "react-test-renderer";
import { decodeEvent, decodeProgress } from "./protocol-decode";
import { apply, emptyWorkspace, type Workspace, type WorkspaceNode } from "./workspace";
import { lineText } from "./surface/MonoLine";
import { progressRows, PROGRESS_ROWS } from "./surface/record-progress";
import { RecordProgress } from "./surface/RecordProgress";

/*
 * Independently authored synthetic writer statuses: invented sequences, counts and limits for a
 * recording run. They describe no real source, writer or store.
 */

const writer = (over: Record<string, unknown> = {}) => ({
  state: "recording", first: "101", acceptedThrough: "160", committedThrough: "150", pending: "10", rejected: "0", termination: null,
  chargedBytes: "8192", chargedWork: "60", bytesLimit: "1048576", workLimit: "100000", ...over,
});
const wire = (recording: unknown = writer(), phase = "processing") => ({ kind: "recording", phase, counters: null, recording });

describe("recording progress decoding", () => {
  it.each([
    ["prepared", "reading"], ["recording", "processing"], ["draining", "committing"],
    ["stopped", "complete"], ["incomplete", "stopped"], ["unconfirmed", "stopped"],
  ])("accepts the %s writer state only with its own phase (%s)", (state, phase) => {
    expect(decodeProgress(wire(writer({ state }), phase))).toEqual({ kind: "recording", phase, counters: null, recording: writer({ state }) });
  });

  it("keeps counts beyond a JavaScript number exact and an unknown pending count unknown", () => {
    const status = writer({ first: "1", committedThrough: "9007199254740993", acceptedThrough: "18446744073709551615", pending: null,
      rejected: "9007199254740995", termination: "overloaded", chargedBytes: "18446744073709551615", state: "incomplete" });
    expect(decodeProgress(wire(status, "stopped"))).toMatchObject({ recording: status });
  });

  it("reads an absent writer status as withheld, never as zero", () => {
    const decoded = decodeProgress({ kind: "recording", phase: "processing", counters: null });
    expect(decoded).toEqual({ kind: "recording", phase: "processing", counters: null });
    expect(decoded).not.toHaveProperty("recording");
  });

  it("leaves the finite analysis wire exactly as it was", () => {
    expect(decodeProgress({ kind: "records", phase: "reading", counters: null })).toEqual({ kind: "records", phase: "reading", counters: null });
    expect(() => decodeProgress({ kind: "records", phase: "reading", counters: null, recording: writer() })).toThrow();
  });

  it.each([
    ["counters beside a writer status", { ...wire(), counters: {} }],
    ["a null writer status", { ...wire(), recording: null }],
    ["a surplus progress field", { ...wire(), live: true }],
    ["a surplus writer field", wire({ ...writer(), queue: "3" })],
    ["a missing writer field", wire((({ chargedWork: _w, ...rest }) => rest)(writer()))],
    ["a state that disagrees with its phase", wire(writer({ state: "draining" }), "processing")],
    ["an unknown state", wire(writer({ state: "following" }))],
    ["an unknown end", wire(writer({ termination: "paused" }))],
    ["committed beyond accepted", wire(writer({ committedThrough: "161", pending: null }))],
    ["committed before the first sequence", wire(writer({ committedThrough: "99", pending: null }))],
    ["a pending count that disagrees", wire(writer({ pending: "9" }))],
    ["a first sequence of zero", wire(writer({ first: "0" }))],
    ["a count as a JSON number", wire(writer({ rejected: 0 }))],
    ["a leading zero", wire(writer({ chargedWork: "060" }))],
    ["a count past u64", wire(writer({ bytesLimit: "18446744073709551616" }))],
  ])("refuses %s", (_name, raw) => {
    expect(() => decodeProgress(raw)).toThrow();
  });
});

describe("recording progress display", () => {
  const node = (over: Partial<WorkspaceNode> = {}, progress: unknown = wire()): WorkspaceNode => ({
    id: "n1", name: "recording", command: ":synthetic", dependsOn: [], state: "running", provenance: {}, cautions: [], kept: false, run: "r1",
    progress: { run: "r1", value: decodeProgress(progress) }, ...over,
  });
  const rows = (value: WorkspaceNode) => progressRows(value)!.map(lineText);

  it("says writer state, committed and accepted separately, pending, rejected, the end and charges against captured limits", () => {
    expect(rows(node())).toEqual([
      "recording · committed 101–150 · accepted through 160",
      "10 pending · 0 rejected · no end reported",
      "logical charge · value 8,192 of cap 1,048,576 · work 60 of cap 100,000",
    ]);
    const ended = rows(node({ state: "failed" }, wire(writer({ state: "incomplete", acceptedThrough: "150", pending: null, rejected: "3", termination: "write_failed" }), "stopped")));
    expect(ended[0]).toBe("incomplete · committed 101–150 · accepted through 150");
    expect(ended[1]).toBe("pending unknown · 3 rejected · ended: a write failed");
  });

  it("makes no finite extent, held-memory or earned-allowance claim and keeps three rows", () => {
    for (const progress of [wire(), wire(writer({ committedThrough: "100", acceptedThrough: "100", pending: "0" })), { kind: "recording", phase: "processing", counters: null }]) {
      const said = rows(node({}, progress));
      expect(said).toHaveLength(PROGRESS_ROWS);
      // Whole words and phrases of the finite-analysis claims; "withheld" is the withheld status, not a held charge.
      for (const line of said) expect(line).not.toMatch(/\b(?:extent|held|high-water|earned|read through|memory|RSS)\b/i);
    }
    expect(rows(node({}, wire(writer({ committedThrough: "100", acceptedThrough: "100", pending: "0" }))))[0])
      .toBe("recording · nothing committed yet · from sequence 101 · accepted through 100");
  });

  it("says a withheld status is withheld", () => {
    // Without a status there is no writer state to name; the shared phase stands in, and no count is drawn.
    expect(rows(node({}, { kind: "recording", phase: "processing", counters: null }))).toEqual([
      "processing · writer status withheld · the source is not public",
      "accepted —", "charge —",
    ]);
  });

  it("labels a working state on a settled node as last reported, and a stopped writer only on its own node state", () => {
    expect(rows(node({ state: "stale" }))[0]).toMatch(/^last reported recording/);
    expect(rows(node({ state: "ready" }, wire(writer({ state: "stopped", termination: "manual" }), "complete")))[0]).toMatch(/^stopped ·/);
    expect(rows(node({ state: "running" }, wire(writer({ state: "stopped", termination: "manual" }), "complete")))[0]).toMatch(/^last reported stopped/);
  });

  it("shows nothing for another run's report and forgets progress when access is withdrawn", () => {
    expect(progressRows(node({ run: "r2" }))).toBeUndefined();
    const created = { event: "created", node: "n1", name: "recording", command: ":synthetic", dependsOn: [], dependencyLifetime: "continuous", interactive: false, run: "r1" };
    const running = [created, { event: "node", node: "n1", state: "running" }].reduce<Workspace>((workspace, event) => apply(workspace, decodeEvent(event)), emptyWorkspace);
    // The current run, as the engine names it; a report is drawn only for that run.
    const withRun = { ...running, nodes: running.nodes.map(item => ({ ...item, run: "r1" })) };
    const reported = apply(withRun, decodeEvent({ event: "node-progress", node: "n1", run: "r1", progress: wire() }));
    expect(progressRows(reported.nodes[0])).toHaveLength(PROGRESS_ROWS);
    const withdrawn = apply(reported, decodeEvent({ event: "result-access", node: "n1", readable: false }));
    expect(progressRows(withdrawn.nodes[0])).toBeUndefined();
  });

  it("names the block as recording progress with the writer's own note", () => {
    const tree = create(<RecordProgress node={node()} />);
    const group = tree.root.findByProps({ role: "group" });
    expect(group.props["aria-label"]).toBe("recording progress");
    expect(group.props["aria-description"]).toMatch(/not a stored receipt/);
    tree.unmount();
  });
});
