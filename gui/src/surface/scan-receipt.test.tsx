import { describe, expect, it } from "vitest";
import { act, create } from "react-test-renderer";
import { parseExactJson } from "../exact-json";
import type { StoredValue } from "../protocol";
import { lineText } from "./MonoLine";
import { receiptHeadline, receiptRows, scanReceiptOf } from "./scan-receipt";
import { ScanReceiptDetails } from "./ScanReceiptDetails";

/*
 * Explicitly synthetic. The encoding follows the engine's ScanResult receipt as the value codec
 * writes it: Options as {kind:"none"} / {kind:"some",value}, integers as JSON numbers (exact
 * lexemes beyond a safe number), `limits` as a nested record. Identities, revisions and counts here
 * are invented; the type shape is a minimal stand-in carrying only the names the reader checks.
 */
const none = { kind: "none" };
const some = (value: unknown) => ({ kind: "some", value });
const receipt = (over: Record<string, unknown> = {}) => ({
  status: "stopped", position: 1, readPosition: 2, extent: 3, positionUnit: "records", inputChargeUnit: "logical_charge",
  inputCharge: 128, inputRecords: 1, outputCharge: 128, outputRecords: 1, work: 17100, measuredWork: 16000, workAllowance: 16524288,
  outstandingWork: some(1048576), durationChargedMs: 58912, durationOutstandingMs: some(1200),
  heldCharge: 2000, highWaterCharge: 5000, finishApplied: false, sourceComplete: none,
  failureCode: some("CAL005"), failureMessage: some("division by zero"), exhausted: none, rejectedStart: none, rejectedEnd: none,
  analysisId: "analysis-synthetic", attempt: some("synthetic-attempt-2"), previousAttempt: some("synthetic-attempt-1"), transitionRevision: "sha256:transition-synthetic", finishRevision: none,
  profile: "TypedRecords", profileRevision: "sha256:profile-synthetic",
  sourceNode: some("node-synthetic"), sourceRun: some("run-synthetic"), sourceRevision: some(1), sourcePort: some("data"), sourcePath: [],
  durableResume: false,
  limits: { work: 64000000, inputCharge: 16777216, inputRecords: 250000, heldCharge: 134217728, outputCharge: 16777216,
    outputRecords: 100000, recordWork: 1000000, recordCharge: 8388608, stateCharge: 1048576, contextCharge: 1048576, durationMs: 60000 },
  ...over,
});
const INT = { kind: "primitive", name: "INT" } as const;
const scanResult = (data: unknown, name = "ScanResult", meta: StoredValue["meta"] = undefined): StoredValue => ({
  type: { kind: "record", name, fields: [{ name: "state", type: INT }, { name: "outputs", type: { kind: "list", element: INT } },
    { name: "receipt", type: { kind: "record", name: "ScanReceipt", fields: [] } }] },
  data, provenance: {}, ...(meta ? { meta } : {}),
});
const stopped = scanResult({ state: 1, outputs: [1], receipt: receipt() });
const said = (value: StoredValue) => receiptRows(scanReceiptOf(value)!).map(lineText);

describe("the analysis receipt", () => {
  it("reads a stopped analysis's receipt with unknown producer completion and separate committed and read positions", () => {
    const read = scanReceiptOf(stopped)!;
    expect(read).toMatchObject({ status: "stopped", position: "1", readPosition: "2", extent: "3", sourceComplete: undefined,
      failureCode: "CAL005", exhausted: undefined, sourceRevision: "1", limits: { work: "64000000", durationMs: "60000" } });
    const rows = said(stopped);
    expect(rows).toContain("outcome stopped · finish not applied");
    expect(rows).toContain("selected extent 3 records · producer completion unknown");
    expect(rows).toContain("committed through 1 · read through 2 records · read, not committed");
    expect(rows).toContain("work charged 17,100 · measured 16,000 · prepaid reservation 1,048,576");
    expect(rows).toContain("earned allowance 16,524,288 · absolute cap 64,000,000");
    expect(rows).toContain("duration charged 58,912 ms elapsed · prepaid reservation 1,200 ms · limit 60,000 ms");
    expect(rows).toContain("checkpoint attempt synthetic-attempt-2 · previous attempt synthetic-attempt-1");
    expect(rows).toContain("logical held charge 2,000 · high-water 5,000 · cap 134,217,728 · not memory use or stored bytes");
    expect(rows).toContain("failure CAL005 · division by zero");
    expect(rows).toContain("source node-synthetic data · run run-synthetic · revision 1");
    expect(rows).toContain("finish definition none");
    // A deterministic stop keeps its durable attempt even though the engine does not offer resuming.
    expect(rows).toContain("durable checkpoint yes");
    expect(rows.join("\n")).not.toMatch(/resume|dataset|RSS|KiB|MiB|%/i);
    expect(lineText(receiptHeadline(scanReceiptOf(stopped)!))).toBe("analysis receipt · stopped · committed through 1 of 3 records");
  });

  it("says the exhausted dimension and the original rejected byte range only when the engine supplied them", () => {
    const value = scanResult({ state: 0, outputs: [], receipt: receipt({ positionUnit: "bytes", inputChargeUnit: "raw_bytes",
      exhausted: some("held_charge"), rejectedStart: some(10), rejectedEnd: some(20), failureCode: some("CAL006"), failureMessage: some("limit") }) });
    const rows = said(value);
    expect(rows).toContain("limit reached held charge");
    expect(rows).toContain("rejected original bytes 10–20 (half-open)");
    expect(rows).toContain("1 records in · input raw bytes 128 · 1 outputs · output logical charge 128");
    expect(said(stopped).some(row => row.startsWith("limit reached") || row.startsWith("rejected"))).toBe(false);
  });

  it.each([
    ["duration", "limit reached duration"],
    ["work_allowance", "limit reached earned work allowance"],
    ["work", "limit reached work absolute cap"],
    ["synthetic_future_dimension", "limit reached synthetic_future_dimension"],
  ])("names the exhausted dimension %s in words, and an unlisted one as written", (dimension, row) => {
    expect(said(scanResult({ state: 0, outputs: [], receipt: receipt({ exhausted: some(dimension) }) }))).toContain(row);
  });

  it("says a memory analysis has no attempt, no predecessor and no reservation, never zero or an invented one", () => {
    const memory = scanResult({ state: 1, outputs: [1], receipt: receipt({
      attempt: none, previousAttempt: none, outstandingWork: none, durationOutstandingMs: none }) });
    const read = scanReceiptOf(memory)!;
    expect(read).toMatchObject({ attempt: undefined, previousAttempt: undefined, outstandingWork: undefined, durationOutstandingMs: undefined,
      measuredWork: "16000", durationChargedMs: "58912" });
    const rows = said(memory);
    expect(rows).toContain("checkpoint attempt none · previous attempt none");
    expect(rows).toContain("durable checkpoint none");
    expect(rows).toContain("work charged 17,100 · measured 16,000 · prepaid reservation none");
    expect(rows).toContain("duration charged 58,912 ms elapsed · prepaid reservation none · limit 60,000 ms");
    expect(rows.join("\n")).not.toMatch(/reservation 0|unknown work|lost/);
  });

  it.each([
    ["a refusing stop", { status: "stopped", durableResume: false }],
    ["a permitting cancellation", { status: "cancelled", durableResume: true }],
  ])("says the durable checkpoint from the attempt alone, for %s", (_, over) => {
    expect(said(scanResult({ state: 1, outputs: [1], receipt: receipt(over) }))).toContain("durable checkpoint yes");
  });

  it("says a first durable attempt's predecessor as none rather than deriving one", () => {
    const first = scanResult({ state: 1, outputs: [1], receipt: receipt({ attempt: some("synthetic-attempt-1"), previousAttempt: none }) });
    expect(said(first)).toContain("checkpoint attempt synthetic-attempt-1 · previous attempt none");
  });

  it("reads exact integers beyond a JavaScript number without rounding", () => {
    const wire = JSON.stringify(receipt()).replace('"extent":3', '"extent":9223372036854775807');
    const value = scanResult({ state: 1, outputs: [1], receipt: parseExactJson(wire) });
    expect(scanReceiptOf(value)?.extent).toBe("9223372036854775807");
  });

  it("reads the new work and duration counts exactly up to the largest u64", () => {
    const max = "18446744073709551615";
    const wire = JSON.stringify(receipt())
      .replace('"measuredWork":16000', `"measuredWork":${max}`)
      .replace('"outstandingWork":{"kind":"some","value":1048576}', `"outstandingWork":{"kind":"some","value":${max}}`)
      .replace('"durationChargedMs":58912', `"durationChargedMs":${max}`)
      .replace('"durationOutstandingMs":{"kind":"some","value":1200}', `"durationOutstandingMs":{"kind":"some","value":${max}}`);
    const value = scanResult({ state: 1, outputs: [1], receipt: parseExactJson(wire) });
    expect(scanReceiptOf(value)).toMatchObject({ measuredWork: max, outstandingWork: max, durationChargedMs: max, durationOutstandingMs: max });
    expect(said(value)).toContain("duration charged 18,446,744,073,709,551,615 ms elapsed · prepaid reservation 18,446,744,073,709,551,615 ms · limit 60,000 ms");
    const over = parseExactJson(wire.replace(`"measuredWork":${max}`, '"measuredWork":18446744073709551616'));
    expect(scanReceiptOf(scanResult({ state: 1, outputs: [1], receipt: over }))).toBeUndefined();
  });

  const without = (field: string) => Object.fromEntries(Object.entries(receipt()).filter(([key]) => key !== field));
  it.each(["attempt", "previousAttempt", "measuredWork", "outstandingWork", "durationChargedMs", "durationOutstandingMs"])(
    "gives no summary when the mandatory %s is missing", (field) => {
      expect(scanReceiptOf(scanResult({ state: 1, outputs: [1], receipt: without(field) }))).toBeUndefined();
    });

  it.each([
    ["a bare attempt text", { attempt: "synthetic-attempt-2" }],
    ["a numeric attempt", { attempt: some(2) }],
    ["null for the previous attempt", { previousAttempt: null }],
    ["a some without its value", { previousAttempt: { kind: "some" } }],
    ["an Option with an extra key", { outstandingWork: { kind: "none", value: 0 } }],
    ["a bare reservation number", { outstandingWork: 1048576 }],
    ["a reservation as decimal text", { outstandingWork: some("1048576") }],
    ["measured work as decimal text", { measuredWork: "16000" }],
    ["a fractional charged duration", { durationChargedMs: 1.5 }],
    ["a negative reserved duration", { durationOutstandingMs: some(-1) }],
    ["an Option for the measured work", { measuredWork: some(16000) }],
    ["an unknown Option kind", { durationOutstandingMs: { kind: "unknown" } }],
  ])("gives no summary for %s", (_, over) => {
    expect(scanReceiptOf(scanResult({ state: 1, outputs: [1], receipt: receipt(over) }))).toBeUndefined();
  });

  it.each([
    ["another record name", scanResult({ state: 1, outputs: [], receipt: receipt() }, "Summary")],
    ["metadata of another contract", scanResult({ state: 1, outputs: [], receipt: receipt() }, "ScanResult",
      { version: 1, contract: { name: "Other", digest: "sha256:synthetic" }, truncated: false, fields: {} })],
    ["null where an Option is encoded", scanResult({ state: 1, outputs: [], receipt: receipt({ sourceComplete: null }) })],
    ["an extra receipt field", scanResult({ state: 1, outputs: [], receipt: receipt({ percent: 33 }) })],
    ["a missing limit", scanResult({ state: 1, outputs: [], receipt: receipt({ limits: { work: 1 } }) })],
    ["an unknown status", scanResult({ state: 1, outputs: [], receipt: receipt({ status: "resumable" }) })],
    ["a negative count", scanResult({ state: 1, outputs: [], receipt: receipt({ work: -1 }) })],
  ])("gives no summary for %s, leaving the generic value view alone", (_, value) => {
    expect(scanReceiptOf(value)).toBeUndefined();
  });

  it("reads without changing the value or its metadata", () => {
    const meta = { version: 1 as const, contract: { name: "ScanResult", digest: "sha256:synthetic" }, truncated: false, fields: {} };
    const value = scanResult({ state: 1, outputs: [1], receipt: receipt() }, "ScanResult", meta);
    const before = JSON.stringify(value);
    expect(scanReceiptOf(value)?.status).toBe("stopped");
    expect(JSON.stringify(value)).toBe(before);
  });

  it("draws a closed disclosure for a receipt and nothing for any other value", () => {
    let tree!: ReturnType<typeof create>;
    act(() => { tree = create(<><ScanReceiptDetails value={stopped} /><ScanReceiptDetails value={{ type: INT, data: 1, provenance: {} }} /></>); });
    const details = tree.root.findAllByType("details");
    expect(details).toHaveLength(1);
    expect(details[0]!.props.open).toBe(false);
    act(() => tree.unmount());
  });
});
