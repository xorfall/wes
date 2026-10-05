import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { MutableRefObject } from "react";
import { DraftEditor, type DraftControls } from "./DraftEditor";
import { SpecScreen } from "./screens/Spec";
import { sha256Hex, type DraftDiagnostic, type DraftResult, type DraftSummary, type DraftValidation } from "../draft-api";

const hoisted = vi.hoisted(() => ({ reveals: [] as { target: { index?: number; from: number; to: number }; focus: boolean }[] }));
vi.mock("./DraftSourceEditor", async () => {
  const React = await import("react");
  return {
    DraftSourceEditor: (props: { text: string; onChange: (t: string) => void; handle?: React.Ref<unknown> }) => {
      React.useImperativeHandle(props.handle, () => ({ reveal: (target: { index?: number; from: number; to: number }, focus: boolean) => hoisted.reveals.push({ target, focus }), focus() {} }));
      return <textarea aria-label="draft-source" value={props.text} onChange={e => props.onChange(e.target.value)} />;
    },
  };
});
vi.mock("./SpecSourceEditor", () => ({ SpecSourceEditor: ({ source, onChange }: { source: string; onChange: (s: string) => void }) => <textarea aria-label="test-source" value={source} onChange={e => onChange(e.target.value)} /> }));

/* ---------- synthetic `items` API ---------- */

const key = { service: "items", apiVersion: "v1", scope: "all" };
const ITEM = { base: "Record", fields: { id: { type: "Text", optional: false }, name: { type: "Text", optional: false } } };
const draftText = (responses: unknown[]) => JSON.stringify({
  draftVersion: 1, provider: "items", types: { Item: ITEM },
  operations: [{ path: ["listItems"], method: "GET", route: "/items", summary: "List items", auth: [], parameters: [{ name: "limit", wire: "limit", location: "query", type: "Int", required: false, encoding: "scalar" }], responses }],
  problems: [],
}, null, 2);
const MISSING = draftText([]);
const COMPLETE = draftText([{ status: 200, mediaType: "application/json", type: "Item" }]);

const authWarning = (text: string): DraftDiagnostic => {
  const from = text.indexOf('"auth"');
  return { severity: "warning", code: "auth-unknown", target: "#/operations/0/auth", message: "authentication never mentioned", fix: "access unknown · does not block", from, to: from + 6, line: text.slice(0, from).split("\n").length };
};
const missingResponse = (text: string): DraftDiagnostic => {
  const from = text.indexOf('"responses"');
  return { severity: "error", code: "missing-response", target: "#/operations/0/responses", message: "no success status documented", fix: 'add {"status": <status>, "mediaType": <media>, "type": <Type>}', from, to: from + 11, line: text.slice(0, from).split("\n").length };
};
function validation(text: string, diagnostics: DraftDiagnostic[], hashOf = text): DraftValidation {
  let preview: unknown = null;
  try { preview = JSON.parse(text); } catch { /* does not parse */ }
  return { hash: sha256Hex(hashOf), valid: preview !== null && !diagnostics.some(d => d.severity === "error"), diagnostics, preview };
}
function summary(revision: string, over: Partial<DraftSummary> = {}): DraftSummary {
  return { key, revision, accepted: false, origin: "model draft from https://docs.synthetic.invalid/items-guide", sourceDigest: `sha256:${"e".repeat(64)}`, valid: false, descriptorRevision: null, ...over };
}
function result(text: string, revision: string, diagnostics: DraftDiagnostic[], over: Partial<DraftResult> = {}, accepted = false): DraftResult {
  const v = validation(text, diagnostics);
  return {
    draft: summary(revision, { valid: v.valid, accepted, descriptorRevision: v.valid ? "f".repeat(64) : null }), text, validation: v,
    evidence: { source: {}, status: "current", manualTargets: [] },
    ...(v.valid ? { descriptorPath: `/library/items/${revision}.json` } : {}), ...over,
  };
}
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(r => { resolve = r; }); return { promise, resolve }; }

type Handler = (request: Record<string, unknown>) => unknown | Promise<unknown>;
function backend(handlers: Record<string, Handler>) {
  const requests: Record<string, unknown>[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: string, init: { body: string }) => {
    const request = JSON.parse(init.body) as Record<string, unknown>;
    requests.push(request);
    const handler = handlers[request.action as string] ?? (request.action === "draftAccess" ? () => ({ enabled: false }) : undefined);
    if (!handler) return { ok: false, status: 400, text: async () => `unexpected ${String(request.action)}` };
    try { const body = await handler(request); return { ok: true, json: async () => body }; }
    catch (e) { return { ok: false, status: 409, text: async () => (e as Error).message }; }
  }));
  return requests;
}

/* ---------- rendering helpers ---------- */

let tree: ReactTestRenderer | undefined;
beforeEach(() => { hoisted.reveals.length = 0; vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] }); });
afterEach(() => { if (tree) act(() => tree!.unmount()); tree = undefined; vi.unstubAllGlobals(); vi.useRealTimers(); });

const textOf = (node: ReactTestInstance | string): string => typeof node === "string" ? node : node.children.map(textOf).join("");
const all = () => JSON.stringify(tree!.toJSON());
const button = (label: string | RegExp) => {
  const found = tree!.root.findAllByType("button").find(b => typeof label === "string" ? textOf(b).startsWith(label) : label.test(textOf(b)));
  if (!found) throw new Error(`no button ${String(label)}: ${tree!.root.findAllByType("button").map(textOf).join(" | ")}`);
  return found;
};
const click = async (label: string | RegExp) => { await act(async () => { await button(label).props.onClick(); }); };
const type = (text: string) => act(() => tree!.root.findByProps({ "aria-label": "draft-source" }).props.onChange({ target: { value: text } }));
const source = () => tree!.root.findByProps({ "aria-label": "draft-source" }).props.value as string;
const byLabel = (label: string) => {
  const nodes = tree!.root.findAllByProps({ "aria-label": label });
  const visible = nodes.filter(node => { for (let p = node.parent; p; p = p.parent) if (p.props.hidden) return false; return true; });
  if (label === "Evidence") return visible.at(-1)!;
  return visible[0] ?? nodes[0]!;
};

async function open(opened: DraftSummary, revisions: DraftSummary[] = [opened]) {
  const subject = vi.fn();
  const controls: MutableRefObject<DraftControls | undefined> = { current: undefined };
  const submit = vi.fn<() => string | Promise<string>>(() => "cell");
  const close = vi.fn();
  const saved = vi.fn();
  await act(async () => { tree = create(<DraftEditor opened={opened} revisions={revisions} controls={controls} onSubject={subject} onSaved={saved} onSubmit={submit} onClose={close} />); });
  const subjectText = () => (subject.mock.calls.at(-1)?.[0] as { text: string }[]).map(s => s.text).join("");
  return { subject: subjectText, controls, submit, close, saved };
}

/* ---------- acceptance ---------- */

it("shares only saved revisions explicitly and reports a failed revoke without changing source", async () => {
  const r1 = result(MISSING, "rev1", [missingResponse(MISSING)]);
  let granted = false;
  let refuse = false;
  const requests = backend({ inspectDraft: () => r1, draftAccess: request => {
    if (refuse) throw new Error("Library unavailable");
    if (typeof request.enabled === "boolean") granted = request.enabled;
    return { enabled: granted };
  } });
  await open(r1.draft);
  const checkbox = () => tree!.root.findAllByType("input").find(n => n.props.type === "checkbox" && n.parent?.type === "label" && textOf(n.parent).includes("Allow agents"))!;
  expect(checkbox().props.checked).toBe(false);
  expect(requests.some(r => "enabled" in r)).toBe(false);
  type("unsaved source");
  await act(async () => { await checkbox().props.onChange({ target: { checked: true } }); });
  expect(checkbox().props.checked).toBe(true);
  expect(source()).toBe("unsaved source");
  expect(requests.some(r => r.action === "saveDraft")).toBe(false);
  refuse = true;
  await act(async () => { await checkbox().props.onChange({ target: { checked: false } }); });
  expect(checkbox().props.checked).toBe(true);
  expect(all()).toContain("Library unavailable");
  refuse = false;
  await act(async () => { await checkbox().props.onChange({ target: { checked: false } }); });
  expect(checkbox().props.checked).toBe(false);
});

describe("an incomplete draft", () => {
  it("should_OpenWithItsMissingResponseAndHoldImport_When_DescribeLeftTheStatusUnknown", async () => {
    const r1 = result(MISSING, "rev1", [authWarning(MISSING), missingResponse(MISSING)]);
    const requests = backend({ inspectDraft: () => r1 });
    const { subject } = await open(r1.draft);

    expect(requests).toEqual([{ action: "inspectDraft", key, revision: "rev1" }, { action: "draftAccess", key }]);
    expect(source()).toBe(MISSING);
    expect(subject()).toBe("items · r1 · 1 error"); // one state; counts and the source live in the body
    expect(all()).toContain("no success status documented");
    expect(textOf(byLabel("E1 listItems responses no success status documented"))).toContain(`line ${missingResponse(MISSING).line}`);
    expect(textOf(byLabel("Readiness"))).toContain("1 blocking problem");
    expect(button("import…").props.disabled).toBe(true);
    expect(button("mark reviewed").props.disabled).toBe(true);

    await click("operations");
    expect(all()).toContain("no responses");
    expect(all()).not.toMatch(/"200"|>200</); // nothing was filled in for the unknown status
    await click("import");
    expect(tree!.root.findAllByType("button").find(b => textOf(b) === "import")!.props.disabled).toBe(true);
    expect(textOf(tree!.root.findByProps({ className: "spec-command mono-meta" }))).toBe("");
  });

  it("should_SaveTextThatDoesNotParse_When_TheUserSavesMidEdit", async () => {
    const r1 = result(MISSING, "rev1", [missingResponse(MISSING)]);
    const broken = `${MISSING.slice(0, 40)}\n  "responses": [ {"status": 20`;
    const syntax: DraftDiagnostic = { severity: "error", code: "syntax", target: "#", message: "unexpected end of text", fix: "close the open brackets", from: broken.length, to: broken.length, line: broken.split("\n").length };
    const requests = backend({ inspectDraft: () => r1, saveDraft: req => result(req.text as string, "rev2", [syntax]) });
    const { subject, controls, saved } = await open(r1.draft);

    type(broken);
    expect(controls.current!.dirty).toBe(true);
    expect(button("save as r2").props.disabled).toBe(false);
    await click("save");

    expect(requests.at(-1)).toEqual({ action: "saveDraft", key, revision: "rev1", text: broken });
    expect(controls.current!.dirty).toBe(false);
    expect(saved).toHaveBeenCalledWith(expect.objectContaining({ revision: "rev2", valid: false }));
    expect(tree!.root.findAllByProps({ role: "status" }).map(textOf).join(" ")).toContain("Saved with problems");
    expect(textOf(byLabel("Readiness"))).toContain("does not parse");
    expect(source()).toBe(broken);
    // The last text that parsed stays readable, and says it is not this text.
    await click("operations");
    expect(all()).toContain("listItems");
    expect(all()).toContain("last parsed text · stale");
    expect(subject()).toMatch(/^items · \S+ · 1 error$/);
  });

  it("should_KeepTheWorkingText_When_SaveIsRejectedForAConcurrentRevision", async () => {
    const r1 = result(MISSING, "rev1", []);
    backend({ inspectDraft: () => r1, saveDraft: () => { throw new Error("draft revision conflict: rev1 is not the latest revision."); } });
    const { controls } = await open(r1.draft);
    await click("source");
    type(`${MISSING} `);
    await click("save");
    expect(source()).toBe(`${MISSING} `);
    expect(controls.current!.dirty).toBe(true);
    expect(tree!.root.findAllByProps({ role: "status" }).map(textOf).join(" ")).toContain("Your text is kept");
  });
});

describe("navigation", () => {
  it("should_RevealTheBackendRange_When_AProblemOrOutlineEntryIsChosen", async () => {
    const diagnostics = [authWarning(MISSING), missingResponse(MISSING)];
    backend({ inspectDraft: () => result(MISSING, "rev1", diagnostics) });
    await open(summary("rev1"));

    await act(async () => { byLabel("E1 listItems responses no success status documented").props.onClick(); });
    // E1 is the second backend diagnostic: its own index and exact UTF-16 offsets travel to the editor.
    expect(hoisted.reveals.at(-1)).toEqual({ target: { index: 1, from: diagnostics[1]!.from, to: diagnostics[1]!.to }, focus: true });
    expect(textOf(tree!.root.findByProps({ "aria-label": "Selected problem" }))).toContain('add {"status": <status>');

    await act(async () => { tree!.root.findAllByProps({ className: "draft-outline-row " }).find(b => textOf(b).startsWith("listItems"))!.props.onClick(); });
    const at = MISSING.indexOf("{", MISSING.indexOf('"operations"'));
    expect(hoisted.reveals.at(-1)).toEqual({ target: { from: at, to: at }, focus: true });

    // `]` walks errors then warnings without taking the focus from the list.
    const workbench = tree!.root.findByProps({ className: "draft-workbench" });
    act(() => workbench.props.onKeyDown({ key: "]", target: { closest: () => null }, preventDefault() {}, defaultPrevented: false }));
    expect(hoisted.reveals.at(-1)).toEqual({ target: { index: 0, from: diagnostics[0]!.from, to: diagnostics[0]!.to }, focus: false });
    // Keys typed in the editor are the editor's.
    const before = hoisted.reveals.length;
    act(() => workbench.props.onKeyDown({ key: "]", target: { closest: () => ({}) }, preventDefault() {}, defaultPrevented: false }));
    expect(hoisted.reveals).toHaveLength(before);
  });

  it("should_SwitchToTheSourceAndThenReveal_When_AProblemIsChosenFromTheProblemsTab", async () => {
    const diagnostics = [missingResponse(MISSING)];
    backend({ inspectDraft: () => result(MISSING, "rev1", diagnostics) });
    await open(summary("rev1"));
    await click("problems");
    expect(tree!.root.findAllByProps({ "aria-label": "draft-source" })).toHaveLength(0);
    await act(async () => { byLabel("E1 listItems responses no success status documented").props.onClick(); });
    expect(tree!.root.findAllByProps({ "aria-label": "draft-source" })).toHaveLength(1);
    expect(hoisted.reveals.at(-1)).toEqual({ target: { index: 0, from: diagnostics[0]!.from, to: diagnostics[0]!.to }, focus: true });
  });
});

describe("the valid gate", () => {
  it("should_HoldImportWhileDirtyOrChecking_When_ASavedValidDraftIsEdited", async () => {
    const r1 = result(COMPLETE, "rev1", [authWarning(COMPLETE)]);
    const check = deferred<DraftValidation>();
    backend({ inspectDraft: () => r1, validateDraft: () => check.promise });
    const { subject } = await open(r1.draft);
    expect(subject()).toBe("items · r1 · ready to import"); // an advisory warning does not make a valid draft look unfinished
    expect(button("import…").props.disabled).toBe(false);

    await click("source");
    type(`${COMPLETE}\n`);
    expect(subject()).toContain("unsaved");
    expect(button("import…").props.disabled).toBe(true);
    expect(textOf(byLabel("Readiness"))).toContain("unsaved changes");
    expect(button("mark reviewed").props.disabled).toBe(true);

    // Back to the saved bytes: no longer dirty, but the pending check still holds the gate.
    type(COMPLETE);
    expect(subject()).toBe("items · r1 · checking…");
    expect(button("import…").props.disabled).toBe(true);
    await act(async () => { vi.advanceTimersByTime(600); });
    expect(textOf(byLabel("Readiness"))).toContain("Checking…");
    await act(async () => { check.resolve(validation(COMPLETE, [authWarning(COMPLETE)])); });
    expect(button("import…").props.disabled).toBe(false);
    expect(textOf(byLabel("Readiness"))).toContain("Ready to import");
  });

  it("should_IgnoreAnswersForOlderTextOrAnotherHash_When_ChecksRace", async () => {
    const r1 = result(MISSING, "rev1", [missingResponse(MISSING)]);
    const answers = [deferred<DraftValidation>(), deferred<DraftValidation>(), deferred<DraftValidation>()];
    let asked = 0;
    const requests = backend({ inspectDraft: () => r1, validateDraft: () => answers[asked++]!.promise });
    const { subject } = await open(r1.draft);
    const edited = COMPLETE;

    await click("check");
    type(edited); // the first check now speaks for older text
    await act(async () => { answers[0]!.resolve(validation(MISSING, [])); });
    expect(subject()).not.toMatch(/· valid$/);
    expect(textOf(byLabel("Problems"))).toContain("stale");
    expect(all()).toContain("○ E1");

    await click("check");
    expect(requests.at(-1)).toEqual({ action: "validateDraft", text: edited });
    await act(async () => { answers[1]!.resolve(validation(edited, [], MISSING)); }); // hash of other bytes
    expect(subject()).not.toMatch(/· valid$/);
    expect(tree!.root.findAllByProps({ role: "status" }).map(textOf).join(" ")).toContain("answered for other text");

    await click("check");
    await act(async () => { answers[2]!.resolve(validation(edited, [])); });
    expect(subject()).toBe("items · r1 · unsaved · valid");
    // Checked is not saved: import still waits for the save.
    expect(button("import…").props.disabled).toBe(true);
  });

  it("should_KeepNewerEdits_When_ASaveAnswersAfterMoreTyping", async () => {
    const r1 = result(MISSING, "rev1", [missingResponse(MISSING)]);
    const answer = deferred<DraftResult>();
    const requests = backend({ inspectDraft: () => r1, saveDraft: () => answer.promise });
    const { controls } = await open(r1.draft);
    type(COMPLETE);
    await act(async () => { button("save").props.onClick(); });
    type(`${COMPLETE}\n\n`);
    await act(async () => { answer.resolve(result(COMPLETE, "rev2", [])); });

    expect(requests.at(-1)).toMatchObject({ action: "saveDraft", text: COMPLETE });
    expect(source()).toBe(`${COMPLETE}\n\n`);
    expect(controls.current!.dirty).toBe(true);
    expect(tree!.root.findAllByProps({ role: "status" }).map(textOf).join(" ")).toContain("newer edits unsaved");
    expect(button("import…").props.disabled).toBe(true);
  });
});

describe("the problems tab", () => {
  it("should_SummarizeReadinessOnceAndGroupAdvisories_When_ADraftIsReadyWithRepeatedWarnings", async () => {
    const first = authWarning(COMPLETE);
    const second = { ...first, target: "#/operations/0/auth/0" };
    const r1 = result(COMPLETE, "rev1", [first, second]);
    backend({ inspectDraft: () => r1 });
    await open(r1.draft);
    await click("problems");

    expect(all()).not.toContain("next problem");
    expect(all()).not.toContain("well-formed JSON");
    const readiness = textOf(byLabel("Readiness"));
    expect(readiness).toContain("✓ Ready to import");
    expect(readiness).toContain("2 advisories are notes you can resolve later.");
    expect(readiness).toContain("Review is optional.");

    const group = byLabel("auth-unknown 2");
    // The shared message and fix are said once; each entry keeps its own place and stays navigable.
    expect(textOf(group).split("authentication never mentioned")).toHaveLength(2);
    expect(textOf(group).split("access unknown · does not block")).toHaveLength(2);
    expect(group.findAllByType("button")).toHaveLength(2);
    expect(byLabel("W2 listItems auth.0 authentication never mentioned")).toBeDefined();
  });

  it("should_NameTheBlockingCount_When_AnErrorHoldsImport", async () => {
    const r1 = result(MISSING, "rev1", [missingResponse(MISSING)]);
    backend({ inspectDraft: () => r1 });
    await open(r1.draft);
    await click("problems");
    expect(textOf(byLabel("Readiness"))).toContain("● Import held  1 blocking problem to fix.");
  });
});

describe("review and import", () => {
  it("should_ImportTheSavedDescriptorWithAnExplicitEndpoint_When_TheRevisionIsValidButNotReviewed", async () => {
    const r1 = result(COMPLETE, "rev1", []);
    const requests = backend({ inspectDraft: () => r1 });
    const { submit, close } = await open(r1.draft);
    await click("import…");
    const form = tree!.root.findByProps({ className: "spec-import-form" });
    await act(async () => form.props.onSubmit({ preventDefault() {} }));
    expect(submit).not.toHaveBeenCalled(); // no endpoint yet
    act(() => byLabel("Import endpoint").props.onChange({ target: { value: "https://synthetic.invalid/v1" } }));
    await act(async () => form.props.onSubmit({ preventDefault() {} }));
    expect(submit).toHaveBeenCalledExactlyOnceWith(':import spec file:"/library/items/rev1.json" as:items endpoint:"https://synthetic.invalid/v1" replace:false');
    expect(close).toHaveBeenCalledOnce();
    expect(requests.map(r => r.action)).toEqual(["inspectDraft", "draftAccess"]); // no review, no API call
  });

  it("should_KeepTypedImportInputWhileHeld_And_ImportTheNewlySavedRevision", async () => {
    const r1 = result(COMPLETE, "rev1", []);
    const edited = COMPLETE.replace("List items", "List all items");
    backend({ inspectDraft: () => r1, saveDraft: req => result(req.text as string, "rev2", []) });
    const { submit, close } = await open(r1.draft);
    await click("import…");
    act(() => byLabel("Import endpoint").props.onChange({ target: { value: "https://synthetic.invalid/v1" } }));
    await click("source");
    type(edited);
    await click("import");
    // Held by the unsaved edit: the typed values stay visible, read-only, and nothing can be sent.
    expect(tree!.root.findAllByProps({ "aria-label": "Import endpoint" })).toHaveLength(0);
    const form = () => tree!.root.findByProps({ className: "spec-import-form" });
    expect(textOf(form())).toContain("Import uses saved text only");
    expect(textOf(form())).toContain("https://synthetic.invalid/v1");
    expect(textOf(form().findByProps({ className: "spec-command mono-meta" }))).toBe("");
    await act(async () => { form().findAllByType("button").find(b => textOf(b) === "save")!.props.onClick(); });
    expect(byLabel("Import endpoint").props.value).toBe("https://synthetic.invalid/v1");
    expect(byLabel("Import alias").props.value).toBe("items");
    await act(async () => form().props.onSubmit({ preventDefault() {} }));
    expect(submit).toHaveBeenCalledExactlyOnceWith(':import spec file:"/library/items/rev2.json" as:items endpoint:"https://synthetic.invalid/v1" replace:false');
    expect(close).toHaveBeenCalledOnce();
  });

  it("should_RecordReviewForThatRevisionOnly_When_MarkedAndThenEdited", async () => {
    const r1 = result(COMPLETE, "rev1", []);
    const edited = COMPLETE.replace("List items", "List all items");
    const requests = backend({
      inspectDraft: () => r1,
      reviewDraft: req => result(COMPLETE, req.revision as string, [], {}, true),
      saveDraft: req => result(req.text as string, "rev2", []),
    });
    const { subject } = await open(r1.draft);
    const review = () => textOf(byLabel("Review"));
    await click("problems");
    expect(review()).toContain("not reviewed");
    await click("mark reviewed");
    expect(requests.at(-1)).toEqual({ action: "reviewDraft", key, revision: "rev1" });
    expect(review()).toContain("reviewed r1");
    expect(subject()).not.toContain("review"); // review is optional, so the status line never names it
    expect(button("reviewed r1 ✓").props.disabled).toBe(true);

    await click("source");
    type(edited);
    expect(button("reviewed saved r1").props.disabled).toBe(true);
    await click("save");
    await click("problems");
    expect(review()).toContain("not reviewed");
    expect(requests.filter(r => r.action === "reviewDraft")).toHaveLength(1);
  });
});

describe("evidence", () => {
  it("should_ShowManualSeparatelyAndOriginalRecordsAsStaleHistory_When_TheBackendSaysSo", async () => {
    const digest = `sha256:${"e".repeat(64)}`;
    const r2 = result(COMPLETE, "rev2", [], {
      evidence: {
        status: "stale", manualTargets: ["#/operations/0/responses/0"],
        source: { location: "https://docs.synthetic.invalid/items-guide", provenance: { version: 1, status: "current", entries: [
          { target: "#/operations/0/responses", source: digest, pointer: "#/paths/~1items/get", lines: [{ start: 44, end: 52 }], basis: "unknown", reason: "the guide prints an example body but never names a status" },
          { target: "#/types/Item/fields/id/type", source: digest, pointer: "#/components/schemas/Item/properties/id", lines: [{ start: 7, end: 7 }], basis: "documented", reason: "id: string" },
        ] } },
      },
    });
    backend({ inspectDraft: () => r2 });
    await open(r2.draft, [summary("rev1"), r2.draft]);
    await click("operations");
    await act(async () => { byLabel("Expand operation listItems").props.onClick(); });
    await act(async () => { byLabel("Provenance of response 200 of listItems").props.onClick(); });

    const evidence = textOf(byLabel("Evidence"));
    expect(evidence).toContain("#/operations/0/responses/0");
    expect(evidence).toContain("unknown · stale");
    expect(evidence).toContain("the guide prints an example body but never names a status");
    expect(evidence).toContain("lines 44–52");
    expect(evidence).toContain("manual · you");
    expect(evidence).toContain("never documented");
    expect(evidence).not.toMatch(/documented( · stale)? · source/); // the user's fact never reads as documented

    await click("schema");
    if (tree!.root.findAllByProps({ "aria-label": "Expand type Item" }).length) await act(async () => byLabel("Expand type Item").props.onClick());
    await act(async () => { button("id").props.onClick(); });
    const field = textOf(byLabel("Evidence"));
    expect(field).toContain("documented · stale");
    expect(field).not.toContain("manual · you");
  });
});

describe("review fixes", () => {
  const documentedId = (status: "current" | "stale") => ({
    status, manualTargets: [],
    source: { provenance: { version: 1, status: "current", entries: [
      { target: "#/types/Item/fields/id/type", source: `sha256:${"e".repeat(64)}`, pointer: "#/components/schemas/Item/properties/id", lines: [{ start: 7, end: 7 }], basis: "documented", reason: "id: string" },
    ] } },
  });

  it("should_TreatOriginalEvidenceAsHistory_When_TheTextHasUnsavedEdits", async () => {
    const r1 = result(COMPLETE, "rev1", [], { evidence: documentedId("current") });
    backend({ inspectDraft: () => r1 });
    await open(r1.draft);
    await click("schema");
    if (tree!.root.findAllByProps({ "aria-label": "Expand type Item" }).length) await act(async () => byLabel("Expand type Item").props.onClick());
    await act(async () => { button("id").props.onClick(); });
    expect(textOf(byLabel("Evidence"))).not.toContain("stale");
    expect(textOf(byLabel("Schema fields"))).not.toContain("stale");

    await click("source");
    type(COMPLETE.replace('"Text"', '"Int"'));
    await click("schema");
    const evidence = textOf(byLabel("Evidence"));
    expect(evidence).toContain("documented · stale");
    expect(evidence).toContain("unsaved edits · recorded when saved");
    expect(textOf(byLabel("Schema fields"))).toContain("documented · stale");
  });

  it("should_NotCallAnyOperationReady_When_AWholeDraftProblemBlocks", async () => {
    const global: DraftDiagnostic = { severity: "error", code: "unresolved-problem", target: "#/problems/0", message: "unresolved requirement", fix: "resolve it and remove the entry", from: 1, to: 2, line: 2 };
    backend({ inspectDraft: () => result(COMPLETE, "rev1", [global]) });
    await open(summary("rev1"));
    await click("operations");
    const table = textOf(byLabel("Draft operations"));
    expect(table).not.toContain("ready");
    expect(textOf(byLabel("Readiness"))).toContain("Import held");
  });
});

it("moves a selected extraction problem to an advisory without saving or relaxing structural errors", async () => {
  const doc = JSON.parse(MISSING);
  const message = "Writes are simulated; changes are not persisted.";
  doc.problems = [{ target: "#/operations/0", message }];
  const text = JSON.stringify(doc, null, 2);
  const diagnostic: DraftDiagnostic = { code: "DRAFT_UNRESOLVED", target: "#/problems/0", message, severity: "error", fix: "Resolve", from: 0, to: 1, line: 1 };
  const r1 = result(text, "rev1", [diagnostic, missingResponse(text)]);
  const requests = backend({ inspectDraft: () => r1 });
  const { submit, saved } = await open(r1.draft);
  await click("● E1");
  expect(button("Keep as advisory").props.disabled).toBe(false);
  type(text + " ");
  expect(button("Keep as advisory").props.disabled).toBe(true);
  type(text);
  // The check still owns exactly this text, but the queued check must settle first.
  await act(async () => { vi.clearAllTimers(); });
  // Restore by reopening, then perform the explicit edit from a current check.
  act(() => tree!.unmount()); tree = undefined;
  await open(r1.draft);
  await click("● E1");
  await click("Keep as advisory");
  expect(JSON.parse(source()).problems).toEqual([]);
  expect(JSON.parse(source()).diagnostics).toEqual([`#/operations/0: ${message}`]);
  expect(JSON.parse(source()).operations).toEqual(doc.operations);
  expect(button("import…").props.disabled).toBe(true);
  expect(button("save").props.disabled).toBe(false);
  await click(/[●○] E2/);
  expect(tree!.root.findAllByType("button").some(b => textOf(b) === "Keep as advisory")).toBe(false);
  expect(requests.some(r => r.action === "saveDraft")).toBe(false);
  expect(submit).not.toHaveBeenCalled(); expect(saved).not.toHaveBeenCalled();
});

it("lets a newly valid retained draft be explicitly saved before import", async () => {
  const r1 = result(COMPLETE, "rev1", []);
  r1.draft.valid = false; r1.draft.descriptorRevision = null; delete r1.descriptorPath;
  const r2 = result(COMPLETE, "rev2", []);
  const requests = backend({ inspectDraft: () => r1, saveDraft: () => r2 });
  await open(r1.draft);
  expect(button("import…").props.disabled).toBe(true);
  expect(all()).toContain("save a revision to prepare this newly valid draft");
  expect(button("save").props.disabled).toBe(false);
  await click("save");
  expect(button("import…").props.disabled).toBe(false);
  expect(requests.find(r => r.action === "saveDraft")?.text).toBe(COMPLETE);
});

describe("/spec library", () => {
  it("should_ListDraftsHideTheirMaterializedDuplicateAndGuardDirtyLeaving_When_OpeningADraft", async () => {
    const duplicate = { key, revision: "f".repeat(64), accepted: false, origin: "describe", sourceDigest: null };
    const legacy = { key: { service: "inventory", apiVersion: "v1", scope: "all" }, revision: "a".repeat(64), accepted: false, origin: "fixture", sourceDigest: null };
    const r1 = result(COMPLETE, "rev1", []);
    backend({ list: () => ({ packages: [legacy, duplicate], drafts: [r1.draft] }), inspectDraft: () => r1 });
    const close = vi.fn();
    await act(async () => { tree = create(<SpecScreen top={[]} onClose={close} onSubmit={vi.fn(() => "cell")} />); });

    expect(byLabel("Open items draft r1")).toBeDefined();
    expect(tree!.root.findAllByProps({ "aria-label": "Open items API r1" })).toHaveLength(0);
    expect(byLabel("Open inventory API r1")).toBeDefined();

    await act(async () => byLabel("Open items draft r1").props.onClick());
    expect(tree!.root.findAllByProps({ "aria-label": "API library" })).toHaveLength(0);
    expect(tree!.root.findAllByProps({ "aria-label": "Describe documentation" })).toHaveLength(0);
    await click("source");
    type(`${COMPLETE} `);
    act(() => byLabel("/spec").props.onKeyDown({ key: "Escape", preventDefault() {}, stopPropagation() {} }));
    expect(close).not.toHaveBeenCalled();
    expect(textOf(tree!.root.findByProps({ role: "alertdialog" }))).toContain("Unsaved draft edits");
    await click("keep editing");
    expect(source()).toBe(`${COMPLETE} `);

    // Returning to the overview uses the same guard as closing the screen.
    await click("← Back to library");
    expect(tree!.root.findAllByProps({ role: "alertdialog" })).toHaveLength(1);
    await click("keep editing");
    expect(source()).toBe(`${COMPLETE} `);
    await click("← Back to library");
    await click("discard");
    expect(byLabel("API library")).toBeDefined();
    expect(byLabel("Open inventory API r1")).toBeDefined();
    expect(tree!.root.findAllByProps({ "aria-label": "draft-source" })).toHaveLength(0);
    expect(close).not.toHaveBeenCalled();
  });

  it("should_FocusTheEditedRevisionAndFollowSavesWithoutReopening_When_ADraftIsSaved", async () => {
    const duplicate = { key, revision: "f".repeat(64), accepted: false, origin: "describe", sourceDigest: null };
    const r1 = result(MISSING, "rev1", [missingResponse(MISSING)]);
    const r0 = summary("rev0", { valid: true, descriptorRevision: duplicate.revision }); // its descriptor is the hidden duplicate
    let drafts = [r0, r1.draft];
    const edited = `${MISSING} `;
    const newer = `${MISSING}  `;
    const answer = deferred<DraftResult>();
    const requests = backend({
      list: () => ({ packages: [duplicate], drafts }), inspectDraft: () => r1,
      saveDraft: () => answer.promise,
    });
    await act(async () => { tree = create(<SpecScreen top={[]} onClose={vi.fn()} onSubmit={vi.fn(() => "cell")} />); });
    // One group: the draft's materialized descriptor is not another library row.
    expect(byLabel("Library APIs").findAll(n => n.props.role === "rowgroup")).toHaveLength(1);
    expect(tree!.root.findAllByProps({ "aria-label": "Open items API r1" })).toHaveLength(0);
    await act(async () => byLabel("Expand items version v1").props.onClick());
    const earlier = () => tree!.root.findByProps({ className: "spec-home-link" });
    expect(textOf(earlier())).toBe("▸ 1 earlier revision");
    expect(all()).toContain("Creates an editable draft in the library. Nothing is imported and the API isn’t called.");

    const heading = () => textOf(tree!.root.findByProps({ className: "spec-editor-heading" }));
    await act(async () => byLabel("Open items draft r2").props.onClick());
    expect(heading()).toContain("Draft r2");

    type(edited);
    await act(async () => { button("save").props.onClick(); });
    type(newer);
    const r3 = result(edited, "rev3", [missingResponse(edited)]);
    drafts = [r0, r1.draft, r3.draft];
    await act(async () => { answer.resolve(r3); });

    expect(heading()).toContain("Draft r3");
    expect(source()).toBe(newer); // the editor stayed mounted with the newer text
    expect(requests.filter(r => r.action === "inspectDraft")).toHaveLength(1);
    await click("← Back to library");
    await click("discard");
    expect(byLabel("Open items draft r3")).toBeDefined();
    // The group stays expanded across opening a draft; its earlier revisions now include the edited one.
    expect(byLabel("Collapse items version v1").props["aria-expanded"]).toBe(true);
    expect(textOf(earlier())).toBe("▸ 2 earlier revisions");
    await act(async () => earlier().props.onClick());
    expect(byLabel("Open items draft r2")).toBeDefined();
    expect(byLabel("Open items draft r1")).toBeDefined();
  });

  it("saves incomplete draft edits before returning to the library", async () => {
    const r1 = result(MISSING, "rev1", [missingResponse(MISSING)]);
    const edited = `${MISSING} `;
    const r2 = result(edited, "rev2", [missingResponse(edited)]);
    let drafts = [r1.draft];
    const requests = backend({
      list: () => ({ packages: [], drafts }), inspectDraft: () => r1,
      saveDraft: () => { drafts = [...drafts, r2.draft]; return r2; },
    });
    const close = vi.fn(), submit = vi.fn();
    await act(async () => { tree = create(<SpecScreen top={[]} onClose={close} onSubmit={submit}/>); });
    await act(async () => byLabel("Open items draft r1").props.onClick());
    type(edited);
    await click("← Back to library");
    await click("save draft");
    expect(byLabel("API library")).toBeDefined();
    expect(byLabel("Open items draft r2")).toBeDefined();
    expect(requests.filter(r => r.action === "saveDraft")).toHaveLength(1);
    expect(requests.find(r => r.action === "saveDraft")?.text).toBe(edited);
    expect(submit).not.toHaveBeenCalled();
    expect(close).not.toHaveBeenCalled();
  });
});

it("waits for draft import acknowledgement and leaves a failed send open", async () => {
  const r1 = result(COMPLETE, "rev1", []);
  backend({ inspectDraft: () => r1 });
  const { submit, close } = await open(r1.draft);
  await click("import…");
  act(() => byLabel("Import endpoint").props.onChange({ target: { value: "https://synthetic.invalid/v1" } }));
  let reject!: (error: Error) => void;
  submit.mockImplementationOnce(() => new Promise<string>((_, no) => { reject = no; }));
  act(() => tree!.root.findByProps({ className: "spec-import-form" }).props.onSubmit({ preventDefault() {} }));
  expect(close).not.toHaveBeenCalled();
  await act(async () => reject(new Error("Synthetic send failed")));
  expect(all()).toContain("Synthetic send failed");
  expect(close).not.toHaveBeenCalled();
  await act(async () => tree!.root.findByProps({ className: "spec-import-form" }).props.onSubmit({ preventDefault() {} }));
  expect(close).toHaveBeenCalledOnce();
});
