import { expect, it } from "vitest";
import type { WorkspaceNode } from "../workspace";
import { lineText } from "./MonoLine";
import { evidenceLabel, grouped, progressRows, PROGRESS_ROWS } from "./record-progress";

/* Structural nodes for the label rules; counters follow the declared progress contract, not a capture. */
const base: WorkspaceNode = { id: "a", command: ":scan source:$raw", dependsOn: [], state: "running", provenance: {}, cautions: [], kept: false, run: "r1" };
const counters = { committedPosition: "18446744073709551615", readPosition: "18446744073709551615", extent: "18446744073709551615", unit: "records" as const,
  inputRecords: "0", outputRecords: "0", work: "1", workAllowance: "1", workLimit: "1000000000", heldCharge: "0", highWaterCharge: "0", heldLimit: "1", outputCharge: "0", outputLimit: "1" };

it("labels evidence by kind and never calls an incomplete analysis a stream", () => {
  expect(evidenceLabel({ ...base, state: "cancelled", evidence: { kind: "stopped_stream", source: "a", run: "r1" } })).toBe("stream stopped · last value");
  expect(evidenceLabel({ ...base, state: "failed", evidence: { kind: "incomplete", source: "a", run: "r1" } })).toBe("incomplete · committed partial result, not a completed run");
  expect(evidenceLabel(base)).toBeUndefined();
});

it("lets a terminal phase speak only for the engine state it belongs to", () => {
  const phaseOf = (state: WorkspaceNode["state"], phase: "processing" | "committing" | "complete" | "stopped" | "cancelled") =>
    lineText(progressRows({ ...base, state, progress: { run: "r1", value: { kind: "records", phase, counters } } })![0]!).split(" · ")[0];
  expect(phaseOf("ready", "complete")).toBe("complete");
  expect(phaseOf("failed", "stopped")).toBe("stopped");
  expect(phaseOf("cancelled", "cancelled")).toBe("cancelled");
  expect(phaseOf("running", "processing")).toBe("processing");
  // Never a success before publication, and never a stop on a ready node.
  expect(phaseOf("running", "complete")).toBe("last reported complete");
  expect(phaseOf("stale", "complete")).toBe("last reported complete");
  expect(phaseOf("failed", "complete")).toBe("last reported complete");
  expect(phaseOf("ready", "stopped")).toBe("last reported stopped");
  expect(phaseOf("failed", "processing")).toBe("last reported processing");
  // Committing is still work in progress: shown while running, and never as the settled outcome.
  expect(phaseOf("running", "committing")).toBe("committing");
  expect(phaseOf("ready", "committing")).toBe("last reported committing");
  expect(phaseOf("cancelled", "committing")).toBe("last reported committing");
});

it("groups exact integers beyond a JavaScript number without rounding", () => {
  expect(grouped("18446744073709551615")).toBe("18,446,744,073,709,551,615");
  expect(grouped("0")).toBe("0");
  expect(grouped("999")).toBe("999");
});

it("keeps three rows in every phase and says equal committed and read positions without a pending note", () => {
  for (const phase of ["reading", "processing", "finishing", "committing", "complete", "stopped", "cancelled"] as const) {
    const rows = progressRows({ ...base, progress: { run: "r1", value: { kind: "records", phase, counters } } })!;
    expect(rows).toHaveLength(PROGRESS_ROWS);
    expect(lineText(rows[0]!)).not.toContain("read, not committed");
    expect(lineText(rows[0]!)).toContain("committed through 18,446,744,073,709,551,615 of 18,446,744,073,709,551,615 records");
  }
  const withheld = progressRows({ ...base, progress: { run: "r1", value: { kind: "records", phase: "reading", counters: null } } })!;
  expect(withheld).toHaveLength(PROGRESS_ROWS);
  expect(progressRows({ ...base, run: undefined, progress: { run: "r1", value: { kind: "records", phase: "reading", counters: null } } })).toBeUndefined();
});
