import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "../engine";
import type { DatasetRead } from "../dataset-read";
import { decodeDatasetReference } from "../presentation/dataset";
import type { StoredValue, TypeShape } from "../protocol";
import type { WorkspaceNode } from "../workspace";
import { ComposeContext, deletePlanOf, freshName, retentionCommand, selectionExpression, snapshotCommand, type Composer } from "./dataset-management";
import { DeletePlanReview } from "./DeletePlanReview";
import { ReadStatus } from "./ReadStatus";
import { ObservationStatus } from "./ObservationStatus";
import { resumeRefusal, ScanReceiptDetails } from "./ScanReceiptDetails";
import { scanReceiptOf } from "./scan-receipt";
import { datasetWithdrawals, WITHDRAWN_TITLE } from "./render/dataset-source";
import { ValueBlock } from "./render/ValueBlock";

/* Synthetic results, plans and receipts only: invented names, identities, counts and digests. */

vi.mock("./Cell", async (original) => ({ ...(await original<typeof import("./Cell")>()), useBlockReport: () => () => undefined }));
afterEach(() => { vi.restoreAllMocks(); });

const textOf = (item: ReactTestInstance): string => item.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
const button = (tree: ReactTestRenderer, label: string) => tree.root.findAll(item => item.type === "button" && textOf(item) === label)[0]!;
const composer = (taken: readonly string[] = []) => { const compose = vi.fn(); return { compose, value: { compose, taken: new Set(taken) } satisfies Composer }; };

const plan = (over: Record<string, unknown> = {}): StoredValue => ({
  type: { kind: "meta", name: "DatasetDeletePlan" }, provenance: {},
  data: { dataset: "00000000-0000-4000-8000-0000000000b2", generation: "3", protectedBytes: "9007199254740993", activeReaders: "0", activeWriter: false,
    references: [{ identity: "synthetic-root-1", kind: "value", retention: "protected" }, { identity: "synthetic-root-2", kind: "checkpoint", retention: "automatic" }],
    notice: "One live use in this workspace; expires in five minutes. Changed roots require a new plan. Active readers and writers must be stopped explicitly; restored plans have no authority.", ...over },
});

describe("command spelling", () => {
  it("names a Dataset only through record fields of a named result", () => {
    expect(selectionExpression("analysis", "/outputs")).toBe("$analysis.outputs");
    expect(selectionExpression("analysis", "")).toBe("$analysis");
    expect(selectionExpression("analysis", "/runs/0/events")).toBeUndefined();
    expect(selectionExpression(undefined, "/outputs")).toBeUndefined();
    expect(selectionExpression("analysis", "/a b")).toBeUndefined();
    expect(selectionExpression("bad name", "/outputs")).toBeUndefined();
  });
  it("spells a prefix snapshot exactly and refuses non-canonical identities", () => {
    const basis = `sha256:${"a".repeat(64)}`, shown = `sha256:${"b".repeat(64)}`;
    expect(snapshotCommand("$holding.dataset", basis, "4", shown, "shownPrefix"))
      .toBe(`:dataset snapshot $holding.dataset basis:"${basis}" generation:"4" digest:"${shown}" > shownPrefix`);
    for (const [left, generation, right] of [[`sha256:${"A".repeat(64)}`, "4", shown], [basis, "04", shown], [basis, "4", `${shown}"`], [basis, "4 ", shown], [basis, "4", "sha256:b"]])
      expect(snapshotCommand("$holding.dataset", left!, generation!, right!, "shownPrefix")).toBeUndefined();
  });
  it("names only a strictly positive u64 generation, as the engine accepts", () => {
    const basis = `sha256:${"a".repeat(64)}`, shown = `sha256:${"b".repeat(64)}`;
    expect(snapshotCommand("$holding.dataset", basis, "1", shown, "shownPrefix")).toContain(`generation:"1"`);
    expect(snapshotCommand("$holding.dataset", basis, "18446744073709551615", shown, "shownPrefix")).toContain(`generation:"18446744073709551615"`);
    for (const generation of ["0", "00", "-1", "18446744073709551616"])
      expect(snapshotCommand("$holding.dataset", basis, generation, shown, "shownPrefix")).toBeUndefined();
  });
  it("spells a retention preview guarded by the exact basis and refuses a non-canonical one", () => {
    const basis = `sha256:${"a".repeat(64)}`;
    expect(retentionCommand("$holding.dataset", basis, "retention")).toBe(`:dataset retention $holding.dataset basis:"${basis}" > retention`);
    for (const bad of [`sha256:${"A".repeat(64)}`, `sha256:${"a".repeat(63)}`, `${basis}"`, ` ${basis}`, "sha256:a", ""])
      expect(retentionCommand("$holding.dataset", bad, "retention")).toBeUndefined();
  });
  it("binds a fresh result name", () => {
    expect(freshName("deletion", new Set())).toBe("deletion");
    expect(freshName("deletion", new Set(["deletion", "deletion2"]))).toBe("deletion3");
  });
});

describe("deletion plan projection", () => {
  it("reads exactly the engine's projection and never an authority", () => {
    expect(deletePlanOf(plan())?.references).toHaveLength(2);
    for (const bad of [plan({ token: "secret" }), plan({ protectedBytes: 12 }), plan({ activeWriter: "no" }),
      plan({ references: [{ identity: "x", kind: "value" }] }), { ...plan(), type: { kind: "record", name: "DatasetDeletePlan", fields: [] } as TypeShape }])
      expect(deletePlanOf(bad as StoredValue)).toBeUndefined();
  });

  it("prepares a delete command only after both approvals are answered explicitly, and never runs it", () => {
    const { compose, value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><DeletePlanReview value={plan()} name="deletion" /></ComposeContext.Provider>); });
    const prepare = () => button(tree, "prepare delete command");
    expect(textOf(tree.root)).toContain("synthetic-root-2");
    expect(textOf(tree.root)).toContain("9 007 199 254 740 993 protected bytes");
    expect(prepare().props.disabled).toBe(true);
    const radios = tree.root.findAll(item => item.type === "input" && item.props.type === "radio");
    expect(radios.every(radio => radio.props.checked === false)).toBe(true);
    act(() => radios[0]!.props.onChange());
    expect(prepare().props.disabled).toBe(true);
    act(() => radios[3]!.props.onChange());
    expect(prepare().props.disabled).toBe(false);
    expect(compose).not.toHaveBeenCalled();
    act(() => prepare().props.onClick());
    expect(compose).toHaveBeenCalledWith(":dataset delete $deletion references:true protected:false");
    act(() => tree.unmount());
  });

  it("warns about active readers and a writer and refuses without a plan name", () => {
    const { value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><DeletePlanReview value={plan({ activeReaders: "2", activeWriter: true })} /></ComposeContext.Provider>); });
    expect(textOf(tree.root)).toContain("Stop the active readers and writer explicitly");
    expect(textOf(tree.root)).toContain("this plan has no result name");
    act(() => tree.unmount());
  });

  it("drops the whole plan on access withdrawal at every tier, collapsed included", () => {
    const stored = { handle: "stored-plan", generation: "g-plan" };
    let tree!: ReactTestRenderer;
    for (const collapsed of [false, true]) {
      act(() => { tree = create(<ValueBlock value={plan()} stored={{ ...stored, handle: `${stored.handle}-${collapsed}` }} name="deletion" cacheKey={`plan-${collapsed}`} mode="preview" collapsed={collapsed} inCell={false} engine={{} as Engine} />); });
      expect(textOf(tree.root)).toContain("Deletion plan");
      act(() => datasetWithdrawals.withdraw({ ...stored, handle: `${stored.handle}-${collapsed}` }));
      const shown = textOf(tree.root);
      expect(shown).toContain("Access withdrawn");
      for (const gone of ["Deletion plan", "synthetic-root", "protected bytes", "prepare delete"]) expect(shown).not.toContain(gone);
      act(() => tree.unmount());
    }
  });
});

describe("Dataset actions beneath the descriptor", () => {
  const reference = decodeDatasetReference({
    store: "00000000-0000-4000-8000-0000000000a1", dataset: "00000000-0000-4000-8000-0000000000b2", generation: "7",
    manifest: "00000000-0000-4000-8000-0000000000c3", manifestDigest: `sha256:${"0123456789abcdef".repeat(4)}`, manifestBytes: "512",
    schemaDigest: `sha256:${"fedcba9876543210".repeat(4)}`, records: "12", authorizationGeneration: "1",
  })!;
  const dataset: StoredValue = { type: { kind: "dataset", element: { kind: "primitive", name: "TEXT" } }, provenance: {}, data: { kind: "dataset", reference: { ...reference } } };
  const engine = { readDataset: vi.fn(() => new Promise<DatasetRead>(() => undefined)) } as unknown as Engine;

  it("states the fixed Keep/Pin prefix and prepares inspect and plan commands by name", () => {
    const { compose, value } = composer(["deletion"]);
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><ValueBlock engine={engine} value={dataset} name="events" stored={{ handle: "h-ds", generation: "g-ds" }} cacheKey="g-ds:h-ds" mode="window" inCell={false} /></ComposeContext.Provider>); });
    expect(textOf(tree.root)).toContain("Keep or Pin retains exactly records 0–11 of generation 7; records committed later are not included");
    act(() => button(tree, "inspect…").props.onClick());
    act(() => button(tree, "plan deletion…").props.onClick());
    expect(compose.mock.calls).toEqual([[":dataset inspect $events"], [":dataset plan-delete $events > deletion2"]]);
    act(() => tree.unmount());
  });

  it("offers no command for an unnamed result and says why", () => {
    const { compose, value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><ValueBlock engine={engine} value={dataset} stored={{ handle: "h-un", generation: "g-un" }} cacheKey="g-un:h-un" mode="window" inCell={false} /></ComposeContext.Provider>); });
    expect(tree.root.findAll(item => item.type === "button" && textOf(item) === "plan deletion…")).toHaveLength(0);
    expect(textOf(tree.root)).toContain("this result has none");
    expect(compose).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  describe("retention preview", () => {
    const PREVIEW = "preview retention…";
    const DIGEST = reference.manifestDigest;
    const previews = (tree: ReactTestRenderer) => tree.root.findAll(item => item.type === "button" && textOf(item) === PREVIEW);
    /** A fresh reader per mount whose page never arrives: the band does not wait for records. */
    const reader = () => ({ readDataset: vi.fn(() => new Promise<DatasetRead>(() => undefined)) });
    let mounts = 0;
    function mount(stored: StoredValue, options: { readonly name?: string; readonly prompt?: Composer; readonly mode?: "preview" | "expanded" | "window"; readonly collapsed?: boolean } = {}) {
      const engine = reader();
      const at = { handle: `h-retention-${++mounts}`, generation: "g-retention" };
      const inner = <ValueBlock engine={engine as unknown as Engine} value={stored} stored={at} cacheKey={`${at.generation}:${at.handle}`} mode={options.mode ?? "window"} inCell={false}
        {...(options.name ? { name: options.name } : {})} {...(options.collapsed ? { collapsed: true } : {})} />;
      let tree!: ReactTestRenderer;
      act(() => { tree = create(options.prompt ? <ComposeContext.Provider value={options.prompt}>{inner}</ComposeContext.Provider> : inner); });
      return { tree, engine, at };
    }

    it("prepares the saved snapshot's guarded preview under a fresh name in the management band and submits nothing", () => {
      const { compose, value } = composer(["retention"]);
      const { tree, engine } = mount(dataset, { name: "events", prompt: value });
      const reads = engine.readDataset.mock.calls.length;
      const [action] = previews(tree);
      expect(action!.parent!.props["aria-label"]).toBe("Dataset management");
      expect(tree.root.findAll(item => item.props.className === "dataset-recovery").flatMap(slot => slot.findAll(item => item.type === "button"))).toHaveLength(0);
      expect(compose).not.toHaveBeenCalled();
      act(() => action!.props.onClick());
      expect(compose.mock.calls).toEqual([[`:dataset retention $events basis:"${DIGEST}" > retention2`]]);
      // Preparing reads nothing more and leaves the Keep/Pin explanation as it was.
      expect(engine.readDataset.mock.calls).toHaveLength(reads);
      const shown = textOf(tree.root);
      expect(shown).toContain("Keep or Pin retains exactly records 0–11 of generation 7; records committed later are not included");
      expect(shown).not.toContain("other fields");
      expect(shown).not.toMatch(/\bcost\b|reclaim|reserv|quota/i);
      act(() => tree.unmount());
    });

    it("names a field of a larger result and says the preview leaves out its other fields", () => {
      const holder: StoredValue = { type: { kind: "record", name: "Holding", fields: [{ name: "dataset", type: dataset.type }, { name: "label", type: { kind: "primitive", name: "TEXT" } }] },
        provenance: {}, data: { dataset: { kind: "dataset", reference: { ...reference } }, label: "synthetic" } };
      const { compose, value } = composer();
      const { tree } = mount(holder, { name: "holding", prompt: value });
      act(() => tree.root.findByProps({ "aria-label": "Read records at /dataset" }).props.onClick());
      act(() => previews(tree)[0]!.props.onClick());
      expect(compose.mock.calls).toEqual([[`:dataset retention $holding.dataset basis:"${DIGEST}" > retention`]]);
      expect(textOf(tree.root)).toContain("retention covers this snapshot, not the result's other fields");
      act(() => tree.unmount());
    });

    it.each([
      ["no session prompt", { name: "events" }, "management commands are prepared in the session"],
      ["an unnamed result", { prompt: composer().value }, "management commands refer to results by name; this result has none"],
    ] as const)("offers none with %s and says why", (_case, options, explanation) => {
      const { tree } = mount(dataset, options);
      expect(previews(tree)).toHaveLength(0);
      expect(textOf(tree.root)).toContain(explanation);
      act(() => tree.unmount());
    });

    it("offers none for a Dataset whose path a command cannot spell", () => {
      const { compose, value } = composer();
      const { tree } = mount({ type: { kind: "list", element: dataset.type }, provenance: {}, data: [{ kind: "dataset", reference: { ...reference } }] }, { name: "events", prompt: value });
      act(() => tree.root.findAll(item => item.type === "button" && item.props["aria-expanded"] === false && String(item.props.className).includes("value-toggle"))[0]!.props.onClick());
      expect(previews(tree)).toHaveLength(0);
      expect(textOf(tree.root)).toContain("management commands cannot name this Dataset's path");
      expect(compose).not.toHaveBeenCalled();
      act(() => tree.unmount());
    });

    it("composes nothing when every candidate result name is taken", () => {
      const { compose, value } = composer(["retention", ...Array.from({ length: 998 }, (_, at) => `retention${at + 2}`)]);
      const { tree } = mount(dataset, { name: "events", prompt: value });
      const [action] = previews(tree);
      expect(action!.props.disabled).toBe(true);
      act(() => action!.props.onClick());
      expect(compose).not.toHaveBeenCalled();
      act(() => tree.unmount());
    });

    it("offers none while collapsed", () => {
      const { compose, value } = composer();
      const { tree } = mount(dataset, { name: "events", prompt: value, mode: "preview", collapsed: true });
      expect(previews(tree)).toHaveLength(0);
      expect(compose).not.toHaveBeenCalled();
      act(() => tree.unmount());
    });

    it("drops the action on withdrawal and composes nothing over the prepared command", () => {
      const { compose, value } = composer();
      const { tree, at } = mount(dataset, { name: "events", prompt: value });
      act(() => previews(tree)[0]!.props.onClick());
      act(() => datasetWithdrawals.withdraw(at));
      expect(textOf(tree.root)).toContain("Access withdrawn");
      expect(previews(tree)).toHaveLength(0);
      expect(compose).toHaveBeenCalledTimes(1);
      act(() => tree.unmount());
    });

    it.each([["preview"], ["expanded"], ["window"]] as const)("prepares the same command in %s", (mode) => {
      const { compose, value } = composer();
      const { tree } = mount(dataset, { name: "events", prompt: value, mode });
      act(() => previews(tree)[0]!.props.onClick());
      expect(compose.mock.calls).toEqual([[`:dataset retention $events basis:"${DIGEST}" > retention`]]);
      act(() => tree.unmount());
    });
  });
});

describe("analysis continuation", () => {
  const none = { kind: "none" };
  const some = (value: unknown) => ({ kind: "some", value });
  const receipt = (over: Record<string, unknown> = {}) => ({
    status: "cancelled", position: 1, readPosition: 2, extent: 3, positionUnit: "records", inputChargeUnit: "logical_charge",
    inputCharge: 128, inputRecords: 1, malformed: "strict", rejectedRecords: 0, rejectedInputBytes: 0,
    outputCharge: 128, outputRecords: 1, work: 17100, measuredWork: 16000, workAllowance: 16524288,
    outstandingWork: some(1048576), durationChargedMs: 1200, durationOutstandingMs: some(500),
    heldCharge: 2000, highWaterCharge: 5000, finishApplied: false, sourceComplete: none,
    failureCode: none, failureMessage: none, exhausted: none, rejectedStart: none, rejectedEnd: none,
    analysisId: "analysis-synthetic", attempt: some("synthetic-attempt-1"), previousAttempt: none,
    budgetDigest: some(`sha256:${"1".repeat(64)}`), budgetIssuedAttempt: some("synthetic-attempt-1"), budgetPrevious: none, authorizedWork: 0, workGrant: 0, durationOverrunMs: some(0),
    transitionRevision: "sha256:transition-synthetic", finishRevision: none,
    profile: "TypedRecords", profileRevision: "sha256:profile-synthetic",
    sourceNode: some("node-synthetic"), sourceRun: some("run-synthetic"), sourceRevision: some(1), sourcePort: some("data"), sourcePath: [],
    durableResume: true,
    limits: { work: 64000000, inputCharge: 16777216, inputRecords: 250000, heldCharge: 134217728, outputCharge: 16777216,
      outputRecords: 100000, recordWork: 1000000, recordCharge: 8388608, pageBytes: 65536, stateCharge: 1048576, contextCharge: 1048576, durationMs: 60000 },
    ...over,
  });
  const result = (over: Record<string, unknown> = {}): StoredValue => ({
    type: { kind: "record", name: "ScanResult", fields: [{ name: "state", type: { kind: "primitive", name: "INT" } }, { name: "outputs", type: { kind: "list", element: { kind: "primitive", name: "INT" } } },
      { name: "receipt", type: { kind: "record", name: "ScanReceipt", fields: [] } }] },
    data: { state: 1, outputs: [1], receipt: receipt(over) }, provenance: {},
  });
  const node = (over: Partial<WorkspaceNode> = {}): WorkspaceNode => ({ id: "n1", name: "analysis", command: ":scan synthetic", dependsOn: [], state: "cancelled", provenance: {}, cautions: [], kept: false, ...over });

  it("is permitted only by the receipt's own durableResume and a stopped, named analysis", () => {
    const allowed = scanReceiptOf(result())!;
    expect(resumeRefusal(allowed, node())).toBeUndefined();
    expect(resumeRefusal(scanReceiptOf(result({ durableResume: false }))!, node())).toBe("this receipt does not permit resuming");
    expect(resumeRefusal(scanReceiptOf(result({ durableResume: false, status: "complete" }))!, node())).toContain("complete");
    expect(resumeRefusal(allowed, node({ state: "running" }))).toBe("the analysis is still running");
    expect(resumeRefusal(allowed, node({ name: undefined }))).toContain("this result has none");
  });

  it.each([
    ["an exhausted duration", { status: "stopped", exhausted: some("duration") }],
    ["an exhausted earned work allowance", { status: "stopped", exhausted: some("work_allowance") }],
    ["a refused record", { status: "stopped", failureCode: some("SYN001"), failureMessage: some("synthetic record refusal"), rejectedStart: some(10), rejectedEnd: some(20) }],
  ])("follows the engine's refusal after %s and never grants from other fields", (_, over) => {
    expect(resumeRefusal(scanReceiptOf(result({ ...over, durableResume: false }))!, node()))
      .toBe("stopped · checkpoint kept, but only a cancelled analysis is offered resuming");
    // The GUI does not second-guess a permitting receipt either; the engine re-checks when the command runs.
    expect(resumeRefusal(scanReceiptOf(result({ ...over, durableResume: true }))!, node())).toBeUndefined();
  });

  it("names a missing durable checkpoint only as the reason for an engine refusal", () => {
    const memory = { attempt: none, previousAttempt: none, outstandingWork: none, durationOutstandingMs: none, budgetDigest: none, budgetIssuedAttempt: none };
    expect(resumeRefusal(scanReceiptOf(result({ ...memory, durableResume: false }))!, node())).toBe("no durable checkpoint · nothing to resume from");
  });

  it("shows a stopped analysis's kept checkpoint without turning it into resume authority", () => {
    const { compose, value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><ScanReceiptDetails value={result({ status: "stopped", durableResume: false })} node={node({ state: "ready" })} open /></ComposeContext.Provider>); });
    expect(textOf(tree.root)).toContain("durable checkpoint yes");
    expect(button(tree, "resume analysis…").props.disabled).toBe(true);
    expect(textOf(tree.root)).toContain("only a cancelled analysis is offered resuming");
    act(() => button(tree, "resume analysis…").props.onClick());
    expect(compose).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("prepares a new resume cell and never refreshes the original", () => {
    const { compose, value } = composer(["continuation"]);
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><ScanReceiptDetails value={result()} node={node()} open /></ComposeContext.Provider>); });
    expect(tree.root.findAll(item => item.type === "button" && textOf(item) === "continue analysis…")).toHaveLength(0);
    act(() => button(tree, "resume analysis…").props.onClick());
    expect(compose.mock.calls).toEqual([[":scan resume $analysis > continuation2"]]);
    act(() => tree.update(<ComposeContext.Provider value={value}><ScanReceiptDetails value={result({ durableResume: false })} node={node()} open /></ComposeContext.Provider>));
    expect(button(tree, "resume analysis…").props.disabled).toBe(true);
    act(() => button(tree, "resume analysis…").props.onClick());
    expect(compose).toHaveBeenCalledTimes(1);
    act(() => tree.unmount());
  });

  it("keeps resume disabled for a refusing receipt whether the disclosure is closed (Inspector) or open (Open details)", () => {
    const { compose, value } = composer();
    for (const open of [false, true]) {
      let tree!: ReactTestRenderer;
      act(() => { tree = create(<ComposeContext.Provider value={value}><ScanReceiptDetails value={result({ durableResume: false })} node={node()} open={open} /></ComposeContext.Provider>); });
      expect(button(tree, "resume analysis…").props.disabled).toBe(true);
      expect(textOf(tree.root)).toContain("this receipt does not permit resuming");
      act(() => tree.unmount());
    }
    // Without a session composer nothing can be prepared, even when the receipt permits it.
    let bare!: ReactTestRenderer;
    act(() => { bare = create(<ScanReceiptDetails value={result()} node={node()} />); });
    expect(button(bare, "resume analysis…").props.disabled).toBe(true);
    act(() => bare.unmount());
    expect(compose).not.toHaveBeenCalled();
  });

  it("prepares a read-only continuation review by node id beside resume, even when resume is refused", () => {
    const { compose, value } = composer(["bounds"]);
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><ScanReceiptDetails value={result({ status: "stopped", durableResume: false, exhausted: some("work") })} node={node({ state: "failed" })} open /></ComposeContext.Provider>); });
    expect(button(tree, "resume analysis…").props.disabled).toBe(true);
    expect(compose).not.toHaveBeenCalled();
    act(() => button(tree, "Review continuation…").props.onClick());
    expect(compose.mock.calls).toEqual([[":scan continuation $n1 > bounds2"]]);
    act(() => tree.unmount());
  });

  it.each([
    ["a memory analysis", { attempt: none, previousAttempt: none, outstandingWork: none, durationOutstandingMs: none, budgetDigest: none, budgetIssuedAttempt: none }, {}, "no durable checkpoint · nothing to continue from"],
    ["a complete analysis", { status: "complete", durableResume: false }, {}, "complete · nothing to continue"],
    ["a running analysis", {}, { state: "running" as const }, "the analysis is still running"],
    ["an id no command can name", {}, { id: "node-1" }, "the analysis has no node id a command can name"],
  ])("refuses a review for %s and says why", (_, over, at, why) => {
    const { compose, value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><ScanReceiptDetails value={result(over)} node={node(at)} open /></ComposeContext.Provider>); });
    expect(button(tree, "Review continuation…").props.disabled).toBe(true);
    expect(textOf(tree.root)).toContain(why);
    act(() => button(tree, "Review continuation…").props.onClick());
    expect(compose).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("offers the same review in the Inspector's closed disclosure and Open's open one, and none without a session", () => {
    for (const open of [false, true]) {
      const { compose, value } = composer();
      let tree!: ReactTestRenderer;
      act(() => { tree = create(<ComposeContext.Provider value={value}><ScanReceiptDetails value={result()} node={node()} open={open} /></ComposeContext.Provider>); });
      act(() => button(tree, "Review continuation…").props.onClick());
      expect(compose.mock.calls).toEqual([[":scan continuation $n1 > bounds"]]);
      act(() => tree.unmount());
    }
    let bare!: ReactTestRenderer;
    act(() => { bare = create(<ScanReceiptDetails value={result()} node={node()} open />); });
    expect(button(bare, "Review continuation…").props.disabled).toBe(true);
    expect(textOf(bare.root)).toContain("reviewing continuation is prepared in the session");
    act(() => bare.unmount());
  });

  it("draws nothing for a node whose access was withdrawn, even if a value is still passed", () => {
    const { value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><ScanReceiptDetails value={result()} node={node({ accessWithdrawn: true })} open /></ComposeContext.Provider>); });
    expect(tree.toJSON()).toBeNull();
    act(() => tree.unmount());
  });

  it("draws no receipt, count or action once the value is withdrawn", () => {
    const { value } = composer();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ComposeContext.Provider value={value}><ScanReceiptDetails value={result()} node={node()} open /></ComposeContext.Provider>); });
    expect(textOf(tree.root)).toContain("16,524,288");
    // Inspector passes no value for a withdrawn result; Open draws the withdrawal header instead.
    act(() => tree.update(<ComposeContext.Provider value={value}><ScanReceiptDetails value={undefined} node={node({ accessWithdrawn: true })} open /></ComposeContext.Provider>));
    expect(tree.toJSON()).toBeNull();
    act(() => tree.unmount());
  });
});

describe("reading again", () => {
  it("is offered for ordinary failures and missing results, never for a withdrawal", () => {
    const retry = vi.fn();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ReadStatus problem="Result read failed (HTTP 404)" onRetry={retry} />); });
    expect(textOf(tree.root)).toContain("retry reading result");
    act(() => tree.update(<ReadStatus problem="Result · Access withdrawn" withdrawn onRetry={retry} />));
    expect(textOf(tree.root)).not.toContain("retry");
    expect(textOf(tree.root)).toContain(WITHDRAWN_TITLE);
    act(() => tree.update(<ObservationStatus observation={{ value: plan(), handle: "h", state: "stale", problem: "Result · Access withdrawn", withdrawn: true }} onRetry={retry} />));
    expect(tree.root.findAll(item => item.type === "button")).toHaveLength(0);
    act(() => tree.update(<ObservationStatus observation={{ value: plan(), handle: "h", state: "stale", problem: "busy" }} onRetry={retry} />));
    expect(tree.root.findAll(item => item.type === "button")).toHaveLength(1);
    act(() => tree.unmount());
  });
});
