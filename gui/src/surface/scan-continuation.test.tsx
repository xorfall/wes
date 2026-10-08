import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { ReactNode } from "react";
import type { Engine } from "../engine";
import { parseExactJson } from "../exact-json";
import type { StoredValue, TypeShape } from "../protocol";
import { decodeEvent } from "../protocol-decode";
import { apply, emptyWorkspace, type Workspace, type WorkspaceNode } from "../workspace";
import { newCell } from "../cells";
import { lineText } from "./MonoLine";
import { ComposeContext, type Composer } from "./dataset-management";
import { boundRow, continuationDetailRows, continuationRows, CONTINUE, DETAILS, REASON_WORDS, REVIEW_CHANGED } from "./ScanContinuationDetails";
import { continuationCommand, continuationPreviewOf, continueCommand, literalTotal, REASONS, requestedTotals, TOTALS } from "./scan-continuation";
import { ScanReceiptDetails } from "./ScanReceiptDetails";
import { datasetWithdrawals } from "./render/dataset-source";
import { OpenScreen } from "./screens/Open";
import { Inspector } from "./Inspector";
import { readSession } from "./session-model";

vi.mock("./Cell", async (original) => ({ ...(await original<typeof import("./Cell")>()), useBlockReport: () => () => undefined }));

/*
 * Explicitly synthetic reviews. The encoding follows the engine's native ScanContinuationPreview as the
 * value codec writes it: Options as {kind:"none"} / {kind:"some",value}, Int as JSON numbers (exact
 * lexemes beyond a safe number), position and outputCount as decimal text. Identities, digests, node
 * ids and counts are invented and name no real analysis, store or source.
 */
const none = { kind: "none" };
const some = (value: unknown) => ({ kind: "some", value });
/** Spelled independently of the client's set, as the engine's ContinuationReason::name writes them. */
const NATIVE_REASONS = ["not_latest", "active_writer", "finished", "deterministic_stop", "cumulative_stop", "incomplete_source", "lowered",
  "above_ceiling", "frozen_above_ceiling", "unchanged", "ineffective_raise", "no_headroom"];
const BASIS = `sha256:${"a".repeat(64)}`, NEWER = `sha256:${"c".repeat(64)}`, BUDGET = `sha256:${"b".repeat(64)}`;
const ANALYSIS = "00000000-0000-4000-8000-00000000a001", ATTEMPT = "00000000-0000-4000-8000-00000000a002";

const bound = (key: string, over: Record<string, unknown> = {}) =>
  ({ key, current: 1000, issuanceCeiling: 2000, activeCeiling: 4000, requested: 1000, status: "unchanged", ...over });
const preview = (over: Record<string, unknown> = {}) => ({
  version: 1, node: "id12", analysis: ANALYSIS, run: "00000000-0000-4000-8000-00000000a003", attempt: ATTEMPT,
  basis: BASIS, budgetDigest: BUDGET, budgetIssuedAttempt: ATTEMPT, latest: true, activeWriter: false, lifecycle: "open",
  // Stopped at its work total: Resume under the same totals is refused; Continue raising work is not.
  stop: some("work"), canResume: false, resumeReason: some("cumulative_stop"), canContinue: true, continueReason: none,
  measuredWork: 990, chargedWork: 1000, outstandingWork: 0, chargedAfterInterruption: 1000,
  durationChargedMs: 1200, durationOutstandingMs: 0, durationAfterInterruptionMs: 1200,
  allowanceBefore: 1000, allowanceAfter: 2000, authorizedWork: 0, newWorkGrant: 1000, position: "41", outputCount: "7",
  bounds: TOTALS.map(key => bound(key, key === "work" ? { requested: 2000, status: "raised" } : {})),
  frozen: { stepRevision: "sha256:step-synthetic", finishRevision: none, profileRevision: "sha256:profile-synthetic", sourceDigest: "sha256:source-synthetic",
    memory: 11, recordWork: 12, scratch: 13, recordOutputs: 14, startup: 15, rate: 16 },
  ...over,
});

const TEXT: TypeShape = { kind: "primitive", name: "TEXT" }, INT: TypeShape = { kind: "primitive", name: "INT" }, BOOL: TypeShape = { kind: "primitive", name: "BOOL" };
const opt = (element: TypeShape): TypeShape => ({ kind: "option", element });
const record = (name: string, fields: readonly (readonly [string, TypeShape])[]): TypeShape => ({ kind: "record", name, fields: fields.map(([name, type]) => ({ name, type })) });
const BOUND = (name = "ScanContinuationBound") => record(name, [["key", TEXT], ["current", INT], ["issuanceCeiling", INT], ["activeCeiling", INT], ["requested", INT], ["status", TEXT]]);
const FROZEN = (name = "ScanContinuationFrozen") => record(name, [["stepRevision", TEXT], ["finishRevision", opt(TEXT)], ["profileRevision", TEXT], ["sourceDigest", TEXT],
  ["memory", INT], ["recordWork", INT], ["scratch", INT], ["recordOutputs", INT], ["startup", INT], ["rate", INT]]);
const type = (name = "ScanContinuationPreview", element = BOUND(), frozen = FROZEN()) => record(name, [
  ["version", INT], ["node", TEXT], ["analysis", TEXT], ["run", TEXT], ["attempt", TEXT], ["basis", TEXT], ["budgetDigest", TEXT], ["budgetIssuedAttempt", TEXT],
  ["latest", BOOL], ["activeWriter", BOOL], ["lifecycle", TEXT], ["stop", opt(TEXT)], ["canResume", BOOL], ["resumeReason", opt(TEXT)],
  ["canContinue", BOOL], ["continueReason", opt(TEXT)], ...["measuredWork", "chargedWork", "outstandingWork", "chargedAfterInterruption", "durationChargedMs",
    "durationOutstandingMs", "durationAfterInterruptionMs", "allowanceBefore", "allowanceAfter", "authorizedWork", "newWorkGrant"].map(name => [name, INT] as const),
  ["position", TEXT], ["outputCount", TEXT], ["bounds", { kind: "list", element }], ["frozen", frozen]]);
const value = (data: unknown, shape: TypeShape = type(), meta: StoredValue["meta"] = undefined): StoredValue => ({ type: shape, data, provenance: {}, ...(meta ? { meta } : {}) });
const shown = value(preview());
const CONTINUE_COMMAND = `:scan continue $id12 basis:"${BASIS}" work:2000 input:1000 records:1000 output:1000 outputs:1000 duration:1000`;

const textOf = (item: ReactTestInstance): string => item.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
const button = (tree: ReactTestRenderer, label: string) => tree.root.findAll(item => item.type === "button" && textOf(item) === label)[0]!;
const buttons = (tree: ReactTestRenderer, label: string) => tree.root.findAll(item => item.type === "button" && textOf(item) === label);
const input = (tree: ReactTestRenderer, label: string) => tree.root.findByProps({ "aria-label": label, className: "scan-continuation-input" });
const edit = (tree: ReactTestRenderer, label: string, text: string) => act(() => input(tree, label).props.onChange({ currentTarget: { value: text } }));
const composer = (taken: readonly string[] = []) => { const compose = vi.fn(); return { compose, value: { compose, taken: new Set(taken) } satisfies Composer }; };
const hosting = (over: Partial<WorkspaceNode> = {}): WorkspaceNode => ({ id: "id40", name: "bounds", command: ":synthetic", dependsOn: [], state: "ready", provenance: {}, cautions: [], kept: false, ...over });

const trees: ReactTestRenderer[] = [];
afterEach(() => { act(() => trees.splice(0).forEach(tree => tree.unmount())); vi.restoreAllMocks(); });
function draw(content: ReactNode, prompt?: Composer): ReactTestRenderer {
  let tree!: ReactTestRenderer;
  act(() => { tree = create(prompt ? <ComposeContext.Provider value={prompt}>{content}</ComposeContext.Provider> : <>{content}</>); });
  trees.push(tree);
  return tree;
}

describe("the continuation review projection", () => {
  it("reads the engine's review with exact counts, its own node and its closed reasons", () => {
    const read = continuationPreviewOf(shown)!;
    expect(read).toMatchObject({ node: "id12", basis: BASIS, budgetDigest: BUDGET, canContinue: true,
      canResume: false, resumeReason: "cumulative_stop", stop: "work", position: "41", outputCount: "7", newWorkGrant: "1000" });
    // A permitted Continue carries no reason: the native none is omitted, not kept as an explicit undefined.
    expect(read).not.toHaveProperty("continueReason");
    expect(read.frozen.finishRevision).toBeUndefined();
    expect(requestedTotals(read)).toEqual({ work: "2000", input: "1000", records: "1000", output: "1000", outputs: "1000", duration: "1000" });
  });

  it("keeps the current total, the historical issuance ceiling and the active ceiling apart", () => {
    const read = continuationPreviewOf(shown)!;
    const [work, , , , , duration] = read.bounds;
    expect(lineText(boundRow(work!))).toBe("work · current 1,000 · requested 2,000 · active ceiling 4,000 · raised");
    expect(lineText(boundRow(duration!))).toBe("duration ms · current 1,000 · requested 1,000 · active ceiling 4,000 · unchanged");
    // The issuance ceiling is history, said apart from the active ceiling and never as it.
    const detail = continuationDetailRows(read).map(lineText);
    expect(detail).toContain("issuance ceilings when the current totals were issued · not the active ceiling");
    expect(detail).toContain("work · issuance ceiling 2,000");
    expect(detail).toContain("duration ms · issuance ceiling 2,000");
    expect(detail.join("\n")).not.toContain("active ceiling 4,000");
    const above = continuationPreviewOf(value(preview({ bounds: TOTALS.map(key => bound(key, key === "records" ? { requested: 5000, status: "above_ceiling" } : {})),
      canContinue: false, continueReason: some("above_ceiling") })))!;
    expect(above).toMatchObject({ canContinue: false, continueReason: "above_ceiling" });
    expect(lineText(boundRow(above.bounds[2]!))).toBe("input records · current 1,000 · requested 5,000 · active ceiling 4,000 · above active ceiling");
  });

  const I64_MAX = "9223372036854775807", I64_OVER = "9223372036854775808";
  const U64_MAX = "18446744073709551615", U64_OVER = "18446744073709551616";
  it("reads native Int fields exactly up to the largest i64, beyond a JavaScript number", () => {
    const wire = JSON.stringify(preview()).replace('"activeCeiling":4000,"requested":2000', `"activeCeiling":${I64_MAX},"requested":${I64_MAX}`)
      .replace('"measuredWork":990', `"measuredWork":${I64_MAX}`).replace('"rate":16', `"rate":${I64_MAX}`);
    const read = continuationPreviewOf(value(parseExactJson(wire)))!;
    expect(read.bounds[0]).toMatchObject({ activeCeiling: I64_MAX, requested: I64_MAX });
    expect(read.measuredWork).toBe(I64_MAX);
    expect(read.frozen.rate).toBe(I64_MAX);
    expect(continueCommand(read.node, read.basis, requestedTotals(read), "continued")).toContain(`work:${I64_MAX} `);
  });

  // Shape safety only: the bound is the language's Int, not any eligibility rule.
  it.each([
    ["a top-level count", '"measuredWork":990'],
    ["a bound row", '"activeCeiling":4000,"requested":2000'],
    ["a frozen setting", '"rate":16'],
  ])("gives no review for %s one past the native Int, or at the largest u64", (_, field) => {
    for (const digits of [I64_OVER, U64_MAX]) {
      const wire = JSON.stringify(preview()).replace(field, field.replace(/\d+$/, digits));
      expect(wire).toContain(digits);
      expect(continuationPreviewOf(value(parseExactJson(wire)))).toBeUndefined();
    }
  });

  it("reads position and output count as u64 decimal text, not as native Ints", () => {
    const read = continuationPreviewOf(value(preview({ position: U64_MAX, outputCount: U64_MAX })))!;
    expect(read).toMatchObject({ position: U64_MAX, outputCount: U64_MAX });
    expect(continuationPreviewOf(value(preview({ position: U64_OVER })))).toBeUndefined();
    expect(continuationPreviewOf(value(preview({ outputCount: U64_OVER })))).toBeUndefined();
  });

  const without = (field: string) => Object.fromEntries(Object.entries(preview()).filter(([key]) => key !== field));
  it.each(["node", "basis", "budgetDigest", "canContinue", "continueReason", "authorizedWork", "newWorkGrant", "bounds", "frozen", "version"])(
    "gives no review when %s is missing", field => {
      expect(continuationPreviewOf(value(without(field)))).toBeUndefined();
    });

  it.each([
    ["an extra field", { authority: "copied" }],
    ["another version", { version: 2 }],
    ["a non-canonical basis", { basis: `sha256:${"A".repeat(64)}` }],
    ["a short bounds digest", { budgetDigest: "sha256:b" }],
    ["continue offered beside its own refusal", { canContinue: true, continueReason: some("unchanged") }],
    ["continue refused without a reason", { canContinue: false, continueReason: none }],
    ["resume offered beside its own refusal", { canResume: true }],
    ["a reason outside the engine's closed set", { canContinue: false, continueReason: some("raise_the_global_limit") }],
    ["an unknown lifecycle", { lifecycle: "resumable" }],
    ["a bare stop", { stop: "work" }],
    ["a negative count", { chargedWork: -1 }],
    ["a count as decimal text", { allowanceAfter: "2000" }],
    ["a position as a number", { position: 41 }],
    ["a non-canonical position", { position: "041" }],
    ["five totals", { bounds: TOTALS.slice(0, 5).map(key => bound(key)) }],
    ["totals out of the engine's order", { bounds: [...TOTALS].reverse().map(key => bound(key)) }],
    ["a bound status outside the closed set", { bounds: TOTALS.map(key => bound(key, { status: "recommended" })) }],
    ["a bound with an extra field", { bounds: TOTALS.map(key => bound(key, { note: "x" })) }],
    ["a frozen field missing", { frozen: { memory: 1 } }],
  ])("gives no review for %s", (_, over) => {
    expect(continuationPreviewOf(value(preview(over)))).toBeUndefined();
  });

  it("closes the reasons on exactly the engine's twelve names, each with its own words", () => {
    expect([...REASONS]).toEqual(NATIVE_REASONS);
    expect(Object.keys(REASON_WORDS).sort()).toEqual([...NATIVE_REASONS].sort());
    expect(new Set(Object.values(REASON_WORDS)).size).toBe(NATIVE_REASONS.length);
  });

  it.each(NATIVE_REASONS)("reads the engine's %s as either refusal", reason => {
    expect(continuationPreviewOf(value(preview({ canResume: false, resumeReason: some(reason) }))))
      .toMatchObject({ canResume: false, resumeReason: reason });
    expect(continuationPreviewOf(value(preview({ canContinue: false, continueReason: some(reason) }))))
      .toMatchObject({ canContinue: false, continueReason: reason });
  });

  it.each(["cumulative", "deterministic", "Cumulative_stop", "cumulative-stop", "work"])("refuses %s as a near miss of a native name", reason => {
    expect(continuationPreviewOf(value(preview({ resumeReason: some(reason) })))).toBeUndefined();
    expect(continuationPreviewOf(value(preview({ canContinue: false, continueReason: some(reason) })))).toBeUndefined();
  });

  it.each([
    ["another record name", value(preview(), type("ScanContinuationSummary"))],
    ["another bound row type", value(preview(), type(undefined, BOUND("Bound")))],
    ["another frozen type", value(preview(), type(undefined, undefined, FROZEN("Frozen")))],
    ["metadata of another contract", value(preview(), type(), { version: 1, contract: { name: "Other", digest: `sha256:${"d".repeat(64)}` }, truncated: false, fields: {} })],
    ["a generic record of the same fields", value(preview(), { kind: "unknown" })],
  ])("leaves %s to the generic value view", (_, other) => {
    expect(continuationPreviewOf(other)).toBeUndefined();
    expect(draw(<ScanReceiptDetails value={other} node={hosting()} open />).toJSON()).toBeNull();
  });

  it("accepts metadata naming the native contract and changes nothing it reads", () => {
    const meta = { version: 1 as const, contract: { name: "ScanContinuationPreview", digest: `sha256:${"d".repeat(64)}` }, truncated: false, fields: {} };
    const read = value(preview(), type(), meta);
    const before = JSON.stringify(read);
    expect(continuationPreviewOf(read)?.basis).toBe(BASIS);
    expect(JSON.stringify(read)).toBe(before);
  });
});

describe("command spelling", () => {
  const totals = { work: "2000", input: "1000", records: "1000", output: "1000", outputs: "1000", duration: "1000" };
  it("spells all six literal totals in the engine's order", () => {
    expect(continuationCommand("id12", totals, "bounds")).toBe(":scan continuation $id12 work:2000 input:1000 records:1000 output:1000 outputs:1000 duration:1000 > bounds");
    expect(continueCommand("id12", BASIS, totals, "continued")).toBe(`${CONTINUE_COMMAND} > continued`);
  });
  it("admits only canonical positive totals within i64, a canonical basis and a nameable node", () => {
    for (const bad of ["0", "-1", "01", "1.5", "1e3", " 1", "9223372036854775808", ""]) {
      expect(literalTotal(bad)).toBe(false);
      expect(continuationCommand("id12", { ...totals, outputs: bad }, "bounds")).toBeUndefined();
    }
    expect(literalTotal("9223372036854775807")).toBe(true);
    for (const node of ["id-12", "$id12", "id12 > x", ""]) expect(continueCommand(node, BASIS, totals, "continued")).toBeUndefined();
    expect(continueCommand("id12", "sha256:a", totals, "continued")).toBeUndefined();
  });
  it("checks spelling only and leaves the engine's own total caps and ceilings to the review", () => {
    // Above the engine's work and duration caps and any active ceiling, yet spellable: only a review can refuse them.
    const beyond = { ...totals, work: "1000000001", duration: "86400001", records: "9223372036854775807" };
    expect(continuationCommand("id12", beyond, "bounds"))
      .toBe(":scan continuation $id12 work:1000000001 input:1000 records:9223372036854775807 output:1000 outputs:1000 duration:86400001 > bounds");
  });
});

describe("the continuation review", () => {
  it("prepares exactly the shown basis and totals against the engine's node, never the hosting result", () => {
    const { compose, value: prompt } = composer(["continued"]);
    const tree = draw(<ScanReceiptDetails value={shown} node={hosting()} open />, prompt);
    expect(compose).not.toHaveBeenCalled();
    act(() => button(tree, CONTINUE).props.onClick());
    expect(compose.mock.calls).toEqual([[`${CONTINUE_COMMAND} > continued2`]]);
    expect(compose.mock.calls[0]![0]).not.toContain("$bounds");
    expect(compose.mock.calls[0]![0]).not.toContain("$id40");
    const text = textOf(tree.root);
    expect(text).toContain("issuance ceiling 2,000");
    expect(text).toContain("work charged after interruption 1,000 · the whole unconfirmed reservation, once");
    expect(text).toContain("ordinary resume under the latest granted bounds refused · stopped at a cumulative total · the same totals would stop there again");
    expect(text).toContain("not preemption");
  });

  it("puts the decision first and every identity, issuance ceiling, debit and frozen setting after it", () => {
    const tree = draw(<ScanReceiptDetails value={shown} node={hosting()} open />, composer().value);
    const whole = textOf(tree.root.findByProps({ className: "scan-receipt scan-continuation" }));
    const more = textOf(tree.root.findByProps({ className: "scan-continuation-more" }));
    const order = ["continue with the requested bounds permitted", "ordinary resume under the latest granted bounds refused",
      "stopped by work absolute cap · committed position 41 · 7 outputs", "work · current 1,000 · requested 2,000 · active ceiling 4,000 · raised",
      "duration ms · current 1,000 · requested 1,000 · active ceiling 4,000 · unchanged", REVIEW_CHANGED, CONTINUE, DETAILS];
    const at = order.map(part => whole.indexOf(part));
    expect(at.every(index => index >= 0)).toBe(true);
    expect([...at].sort((a, b) => a - b)).toEqual(at);
    // Nothing before the disclosure names a digest, an identity, a historical ceiling, a debit or a frozen knob.
    const decision = whole.slice(0, whole.indexOf(DETAILS));
    for (const later of ["sha256:", ANALYSIS, ATTEMPT, "issuance", "frozen", "measured", "allowance", "unconfirmed"]) {
      expect(decision).not.toContain(later);
      expect(more).toContain(later);
    }
    // The basis binds the prepared command; it is not presented as a fact to read.
    expect(whole).not.toContain(BASIS);
    expect(more).toContain("not preemption");
  });

  it("withdraws Continue as soon as any total is edited, and offers only a new review", () => {
    const { compose, value: prompt } = composer(["bounds"]);
    const tree = draw(<ScanReceiptDetails value={shown} node={hosting()} open />, prompt);
    expect(button(tree, REVIEW_CHANGED).props.disabled).toBe(true);
    edit(tree, "requested input records", "3000");
    expect(button(tree, CONTINUE).props.disabled).toBe(true);
    expect(textOf(tree.root)).toContain("bounds edited · review them first");
    expect(textOf(tree.root)).toContain("edited · not reviewed");
    act(() => button(tree, CONTINUE).props.onClick());
    expect(compose).not.toHaveBeenCalled();
    act(() => button(tree, REVIEW_CHANGED).props.onClick());
    expect(compose.mock.calls).toEqual([[":scan continuation $id12 work:2000 input:1000 records:3000 output:1000 outputs:1000 duration:1000 > bounds2"]]);
    // Preparing the review applies nothing: Continue stays withdrawn until the new review arrives.
    expect(button(tree, CONTINUE).props.disabled).toBe(true);
  });

  it("re-enables Continue only for a new review, with that review's own basis and totals", () => {
    const { compose, value: prompt } = composer();
    const tree = draw(<ScanReceiptDetails value={shown} node={hosting()} open />, prompt);
    edit(tree, "requested work", "3000");
    const next = value(preview({ basis: NEWER, bounds: TOTALS.map(key => bound(key, key === "work" ? { requested: 3000, status: "raised" } : {})) }));
    act(() => tree.update(<ComposeContext.Provider value={prompt}><ScanReceiptDetails value={next} node={hosting()} open /></ComposeContext.Provider>));
    expect(input(tree, "requested work").props.value).toBe("3000");
    act(() => button(tree, CONTINUE).props.onClick());
    expect(compose.mock.calls).toEqual([[`:scan continue $id12 basis:"${NEWER}" work:3000 input:1000 records:1000 output:1000 outputs:1000 duration:1000 > continued`]]);
  });

  it("discards edits on Escape before the surface may close, and refuses totals the engine would not parse", () => {
    const tree = draw(<ScanReceiptDetails value={shown} node={hosting()} open />, composer().value);
    edit(tree, "requested output charge", "01");
    expect(button(tree, REVIEW_CHANGED).props.disabled).toBe(true);
    expect(textOf(tree.root)).toContain("output charge: a positive whole number is required");
    expect(input(tree, "requested output charge").props["aria-invalid"]).toBe(true);
    const event = { key: "Escape", defaultPrevented: false, preventDefault: vi.fn(), stopPropagation: vi.fn() };
    act(() => tree.root.findByProps({ "aria-label": "Continuation bounds" }).props.onKeyDown(event));
    expect(event.preventDefault).toHaveBeenCalled();
    expect(input(tree, "requested output charge").props.value).toBe("1000");
    expect(button(tree, CONTINUE).props.disabled).toBe(false);
  });

  it.each(NATIVE_REASONS)("follows the engine's %s refusal and never second-guesses it from counts", reason => {
    const { compose, value: prompt } = composer();
    const words = REASON_WORDS[reason as keyof typeof REASON_WORDS];
    // Counters that would look permitting are irrelevant: the engine's reason decides.
    const tree = draw(<ScanReceiptDetails value={value(preview({ canContinue: false, continueReason: some(reason), allowanceAfter: 999999, outputCount: "0" }))} node={hosting()} open />, prompt);
    expect(button(tree, CONTINUE).props.disabled).toBe(true);
    expect(textOf(tree.root)).toContain(`continue with the requested bounds refused · ${words}`);
    expect(textOf(tree.root)).not.toMatch(/raise the (global|workspace) limit|increase the setting/i);
    act(() => button(tree, CONTINUE).props.onClick());
    expect(compose).not.toHaveBeenCalled();
    const resume = draw(<ScanReceiptDetails value={value(preview({ resumeReason: some(reason) }))} node={hosting()} open />, prompt);
    expect(textOf(resume.root)).toContain(`ordinary resume under the latest granted bounds refused · ${words}`);
  });

  it("keeps a stop at a cumulative total apart from a deterministic one", () => {
    const at = (resumeReason: string) => continuationRows(continuationPreviewOf(value(preview({ resumeReason: some(resumeReason) })))!)
      .map(lineText).find(row => row.startsWith("ordinary resume"));
    expect(at("cumulative_stop")).toBe("ordinary resume under the latest granted bounds refused · stopped at a cumulative total · the same totals would stop there again");
    expect(at("deterministic_stop")).toBe("ordinary resume under the latest granted bounds refused · stopped by its own code or a per-record limit · raising totals cannot change that");
    // Resume's cumulative stop leaves the engine's Continue verdict for raised totals untouched.
    expect(button(draw(<ScanReceiptDetails value={shown} node={hosting()} open />, composer().value), CONTINUE).props.disabled).toBe(false);
  });

  it("offers Continue whenever the engine permits it, without checking the counters itself", () => {
    const { compose, value: prompt } = composer();
    const tree = draw(<ScanReceiptDetails value={value(preview({ allowanceAfter: 0, measuredWork: 999999999 }))} node={hosting()} open />, prompt);
    act(() => button(tree, CONTINUE).props.onClick());
    expect(compose).toHaveBeenCalledTimes(1);
  });

  it("says the engine's resume verdict even when output records look exhausted or free", () => {
    // Output records already at the outputs total, yet the engine permits: the client does not refuse.
    const full = textOf(draw(<ScanReceiptDetails value={value(preview({ stop: none, canResume: true, resumeReason: none, outputCount: "1000" }))} node={hosting()} open />).root);
    expect(full).toContain("ordinary resume under the latest granted bounds permitted");
    // Output records well below it, yet the engine finds no headroom: the client does not permit.
    const free = textOf(draw(<ScanReceiptDetails value={value(preview({ stop: none, resumeReason: some("no_headroom"), outputCount: "0" }))} node={hosting()} open />).root);
    expect(free).toContain("ordinary resume under the latest granted bounds refused · no headroom remains under these totals");
  });

  it("lets a new review judge totals above the engine's own caps instead of refusing them here", () => {
    const { compose, value: prompt } = composer();
    const tree = draw(<ScanReceiptDetails value={shown} node={hosting()} open />, prompt);
    edit(tree, "requested work", "1000000001");
    edit(tree, "requested duration ms", "86400001");
    expect(input(tree, "requested work").props["aria-invalid"]).toBe(false);
    act(() => button(tree, REVIEW_CHANGED).props.onClick());
    expect(compose.mock.calls).toEqual([[":scan continuation $id12 work:1000000001 input:1000 records:1000 output:1000 outputs:1000 duration:86400001 > bounds"]]);
  });

  it.each([
    ["no session prompt", undefined, hosting(), {}, "continuation commands are prepared in the session"],
    ["a node id no command can name", composer().value, hosting(), { node: "id-12" }, "the original analysis has no node id a command can name"],
    ["no unused result name", composer(["continued", ...Array.from({ length: 998 }, (_, at) => `continued${at + 2}`)]).value, hosting(), {}, "no unused result name is available"],
    ["a review that is no longer current", composer().value, hosting({ state: "stale" }), {}, "this review is not current · review again"],
  ] as const)("disables Continue with %s and says why", (_, prompt, node, over, why) => {
    const tree = draw(<ScanReceiptDetails value={value(preview(over))} node={node} open />, prompt);
    expect(button(tree, CONTINUE).props.disabled).toBe(true);
    expect(textOf(tree.root)).toContain(why);
    act(() => button(tree, CONTINUE).props.onClick());
  });

  it("offers the same actions in a closed (Inspector) and an open (Open details) disclosure", () => {
    for (const open of [false, true]) {
      const { compose, value: prompt } = composer();
      const tree = draw(<ScanReceiptDetails value={shown} node={hosting()} open={open} />, prompt);
      expect(tree.root.findByProps({ className: "scan-receipt scan-continuation" }).props.open).toBe(open);
      // The subordinate facts start closed in either host.
      expect(tree.root.findByProps({ className: "scan-continuation-more" }).props.open).toBeUndefined();
      expect(buttons(tree, CONTINUE)).toHaveLength(1);
      expect(buttons(tree, REVIEW_CHANGED)).toHaveLength(1);
      act(() => button(tree, CONTINUE).props.onClick());
      expect(compose.mock.calls).toEqual([[`${CONTINUE_COMMAND} > continued`]]);
    }
  });

  const rerender = (tree: ReactTestRenderer, prompt: Composer, stored: StoredValue | undefined, node: WorkspaceNode) =>
    act(() => tree.update(<ComposeContext.Provider value={prompt}><ScanReceiptDetails value={stored} node={node} open /></ComposeContext.Provider>));
  const editedReview = (prompt: Composer) => {
    const tree = draw(<ScanReceiptDetails value={shown} node={hosting()} open />, prompt);
    edit(tree, "requested work", "3000");
    // The precondition is a drawn review with an edit in progress, not merely some number on screen.
    expect(input(tree, "requested work").props.value).toBe("3000");
    expect(buttons(tree, CONTINUE)).toHaveLength(1);
    expect(textOf(tree.root)).toContain("issuance ceiling");
    return tree;
  };

  it.each([
    ["the hosting node's access is withdrawn", (prompt: Composer, tree: ReactTestRenderer) => rerender(tree, prompt, shown, hosting({ accessWithdrawn: true }))],
    ["the value is gone", (prompt: Composer, tree: ReactTestRenderer) => rerender(tree, prompt, undefined, hosting())],
    ["the projection becomes malformed", (prompt: Composer, tree: ReactTestRenderer) => rerender(tree, prompt, value(preview({ basis: "sha256:x" })), hosting())],
  ] as const)("clears every number, editor and action at once when %s", (_, withdraw) => {
    const { compose, value: prompt } = composer();
    const tree = editedReview(prompt);
    withdraw(prompt, tree);
    expect(tree.toJSON()).toBeNull();
    expect(tree.root.findAll(item => item.type === "button" || item.type === "input")).toHaveLength(0);
    expect(compose).not.toHaveBeenCalled();
  });

  it("starts a review drawn again after clearing from its own totals, never the discarded edit", () => {
    const { compose, value: prompt } = composer();
    const tree = editedReview(prompt);
    rerender(tree, prompt, value(preview({ basis: "sha256:x" })), hosting());
    rerender(tree, prompt, shown, hosting());
    expect(input(tree, "requested work").props.value).toBe("2000");
    expect(textOf(tree.root)).not.toContain("edited · not reviewed");
    act(() => button(tree, CONTINUE).props.onClick());
    expect(compose.mock.calls).toEqual([[`${CONTINUE_COMMAND} > continued`]]);
  });
});

describe("the same review in /open details and the Inspector", () => {
  const created = { event: "created", node: "id40", name: "bounds", command: ":scan continuation $id12 > bounds", dependsOn: [], dependencyLifetime: "captured", interactive: false };
  const ready = { event: "ready", node: "id40", type: "ScanContinuationPreview", handle: "h40", bytes: 2, provenance: {}, cautions: [], kept: false };
  const run = (...events: unknown[]): Workspace => events.reduce<Workspace>((workspace, event) => apply(workspace, decodeEvent(event)), emptyWorkspace);

  it("prepares the same Continue in Open details, and withdraws every number there", () => {
    const { compose, value: prompt } = composer();
    const node = run(created, ready).nodes[0]!;
    const stored = { handle: "h40-open", generation: "g-open" };
    const tree = draw(<OpenScreen top={[]} subject={[]} tab="details" value={shown} viewing={{ node, value: shown, stored }} />, prompt);
    act(() => button(tree, CONTINUE).props.onClick());
    expect(compose.mock.calls).toEqual([[`${CONTINUE_COMMAND} > continued`]]);
    act(() => datasetWithdrawals.withdraw(stored));
    expect(buttons(tree, CONTINUE)).toHaveLength(0);
    expect(buttons(tree, REVIEW_CHANGED)).toHaveLength(0);
    expect(tree.root.findAll(item => item.type === "input" && item.props.className === "scan-continuation-input")).toHaveLength(0);
    expect(textOf(tree.root)).not.toContain("issuance ceiling");
    expect(textOf(tree.root)).toContain("Access withdrawn");
    expect(compose).toHaveBeenCalledTimes(1);
  });

  it("prepares the same Continue in the Inspector, reading the value once and never refreshing the original", async () => {
    const { compose, value: prompt } = composer();
    const workspace = run(created, ready);
    const cell = readSession({ workspace, cells: [{ ...newCell(":scan continuation $id12 > bounds"), id: "c", state: "answered", nodes: ["id40"] }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
    const engine = { fetch: vi.fn(async () => shown), liveView: vi.fn(async () => undefined) } as unknown as Engine;
    const props = { engine, workspace, generation: "g", cell, selection: { cell: "c", node: "id40", tab: "inspect" as const }, active: true, onTab: vi.fn(), onClose: vi.fn(), onWindow: vi.fn(), overlay: false };
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<ComposeContext.Provider value={prompt}><Inspector {...props} /></ComposeContext.Provider>); });
    trees.push(tree);
    expect(buttons(tree, CONTINUE)).toHaveLength(1);
    act(() => button(tree, CONTINUE).props.onClick());
    expect(compose.mock.calls).toEqual([[`${CONTINUE_COMMAND} > continued`]]);
    expect((engine as unknown as { fetch: ReturnType<typeof vi.fn> }).fetch.mock.calls).toEqual([["h40"]]);
    act(() => datasetWithdrawals.withdraw({ handle: "h40", generation: "g" }));
    expect(buttons(tree, CONTINUE)).toHaveLength(0);
    expect(buttons(tree, REVIEW_CHANGED)).toHaveLength(0);
    expect(tree.root.findAll(item => item.type === "input" && item.props.className === "scan-continuation-input")).toHaveLength(0);
    expect(textOf(tree.root)).not.toContain("issuance ceiling");
    expect(compose).toHaveBeenCalledTimes(1);
  });
});
