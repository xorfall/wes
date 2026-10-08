import { describe, expect, it } from "vitest";
import { parseExactJson } from "./exact-json";
import { decodeEvent } from "./protocol-decode";

/*
 * Structural wire inputs written from the declared Rust serializers (driver/progress.rs Counters with
 * camelCase, projection.rs evidence frames). Malformed variants exist only to prove refusal.
 */
const counters = {
  committedPosition: 10, readPosition: 14, extent: 100, unit: "bytes", inputRecords: 3, outputRecords: 2,
  work: 900, workAllowance: 1000, workLimit: 5000, heldCharge: 64, highWaterCharge: 96, heldLimit: 4096, outputCharge: 32, outputLimit: 1024,
};
const progress = (body: unknown) => ({ event: "node-progress", node: "n", run: "r", progress: body });
const evidence = (extra: Record<string, unknown> = {}) => ({ event: "evidence", node: "n", state: "failed", kind: "incomplete", source: "n", run: "r",
  type: "ScanResult", handle: "h", bytes: 3, provenance: {}, cautions: [], kept: false, private: false, ...extra });

describe("the strict progress decoder", () => {
  it("reads counters as exact decimal text, including values beyond a JavaScript number", () => {
    const wire = `{"event":"node-progress","node":"n","run":"r","progress":{"kind":"records","phase":"processing","counters":${JSON.stringify(counters).replace('"extent":100', '"extent":18446744073709551615')}}}`;
    const event = decodeEvent(parseExactJson(wire));
    expect(event).toMatchObject({ event: "node-progress", run: "r", progress: { phase: "processing", counters: { extent: "18446744073709551615", committedPosition: "10", unit: "bytes" } } });
  });

  it("accepts withheld counters and a missing run as stated, not as zero", () => {
    expect(decodeEvent({ event: "node-progress", node: "n", run: null, progress: { kind: "records", phase: "reading", counters: null } }))
      .toEqual({ event: "node-progress", node: "n", run: null, progress: { kind: "records", phase: "reading", counters: null } });
    // The engine reports committing while it makes outputs durable; it decodes like any working phase.
    expect(decodeEvent({ event: "node-progress", node: "n", run: "r", progress: { kind: "records", phase: "committing", counters: null } }))
      .toEqual({ event: "node-progress", node: "n", run: "r", progress: { kind: "records", phase: "committing", counters: null } });
  });

  it.each([
    ["an unknown phase", { kind: "records", phase: "resuming", counters: null }],
    ["an unknown kind", { kind: "bytes", phase: "reading", counters: null }],
    ["a missing counter", { kind: "records", phase: "reading", counters: { ...counters, workLimit: undefined } }],
    ["an extra counter", { kind: "records", phase: "reading", counters: { ...counters, rss: 1 } }],
    ["a negative counter", { kind: "records", phase: "reading", counters: { ...counters, work: -1 } }],
    ["a fractional counter", { kind: "records", phase: "reading", counters: { ...counters, work: 1.5 } }],
    ["a text counter", { kind: "records", phase: "reading", counters: { ...counters, work: "900" } }],
    ["an unknown unit", { kind: "records", phase: "reading", counters: { ...counters, unit: "lines" } }],
    ["an extra progress field", { kind: "records", phase: "reading", counters: null, percent: 5 }],
  ])("refuses %s", (_, body) => {
    expect(() => decodeEvent(JSON.parse(JSON.stringify(progress(body))))).toThrow(/Malformed engine event/);
  });
});

describe("the evidence decoder", () => {
  it("keeps the kind, the terminal state and the failure record", () => {
    const error = { id: "e", code: "CAL006", message: "scan work limit reached", causeId: "", issues: [] };
    expect(decodeEvent(evidence({ error, reason: error.message }))).toMatchObject({ event: "evidence", kind: "incomplete", state: "failed", error, reason: error.message });
    expect(decodeEvent(evidence({ kind: "stopped_stream", state: "cancelled" }))).toMatchObject({ kind: "stopped_stream", state: "cancelled" });
  });

  it.each([
    ["an unknown kind", { kind: "partial" }],
    ["an unknown state", { state: "incomplete" }],
    ["a missing run", { run: "" }],
    ["a missing handle", { handle: undefined }],
    ["non-text provenance", { provenance: { a: 1 } }],
    ["a malformed error", { error: { code: 6 } }],
  ])("refuses %s", (_, extra) => {
    expect(() => decodeEvent(JSON.parse(JSON.stringify(evidence(extra))))).toThrow(/Malformed engine event/);
  });

  it("refuses the retired stopped event instead of reading it as some evidence", () => {
    expect(() => decodeEvent({ event: "stopped", node: "n", state: "cancelled", source: "n", run: "r", type: "Text", handle: "h", bytes: 1, provenance: {}, cautions: [], kept: false }))
      .toThrow(/retired/);
  });

  it("passes other events through unchanged", () => {
    const ready = { event: "ready", node: "n", type: "Text", handle: "h", bytes: 1, provenance: {}, cautions: [], kept: false };
    expect(decodeEvent(ready)).toBe(ready);
  });
});
