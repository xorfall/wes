import { useState } from "react";
import { act, create } from "react-test-renderer";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { ExactNumber } from "@wes/view-sdk";
import { valueViewModules } from "./registry";
import { decodeContract } from "./definition";
import summary, { prepare } from "../../../views/failure-summary/View";
import { definition, type Input, type State } from "../../../views/failure-summary/contract";

type Mode = "preview" | "expanded" | "window";
const context = (mode: Mode) => ({ mode, instance: null });
const empty: State = { failure: null, evidence: null };
const render = (input: Input, mode: Mode = "expanded", state: State = empty) =>
  renderToStaticMarkup(<summary.Component input={input} state={state} revision={0} emit={() => {}} slots={{}} context={context(mode)} />);

/*
 * Independently synthetic scenario: an invented pipeline whose packer tool is invoked without a
 * required option, wrapped by the launcher that ran it and by the stage's exit, and reported by an
 * aggregate gate. Run names, jobs, paths and messages are made up for this test.
 */
const artifact = { run: "synthetic-run-a", attempt: 1, job: "packaging", origin: "stage-log", digest: "c".repeat(64) };
const at = (from: number, to: number) => ({ artifact, from, to });
const failure = (id: string, kind: Input["failures"][number]["kind"], title: string, evidence: ReturnType<typeof at> | null, extra: Partial<Input["failures"][number]> = {}): Input["failures"][number] =>
  ({ id, kind, title, detail: "", location: null, job: "packaging", step: "Assemble bundle", evidence, excerpt: [], confidence: "observed", ...extra });

const run: Input = {
  view: "failure-summary",
  title: "synthetic-run-a · attempt 1",
  failures: [
    failure("inv", "invocation", "packer: error: option --layout is required", at(12, 13), { location: "tools/packer.toml", detail: "packer usage: packer --layout FILE [--quiet]",
      excerpt: [[11, "launching packer for bundle alpha", null], [12, "packer usage: packer --layout FILE [--quiet]", null], [13, "packer: error: option --layout is required", "error"], [14, "launcher: stopping after the packer step", null]]
        .map(([ordinal, text, level]) => ({ ordinal: ordinal as number, text: text as string, level: level as string | null })) }),
    failure("launch", "build", "launcher: child process returned 2", at(14, 19), { confidence: "inferred" }),
    failure("exit", "build", "stage Assemble bundle ended with status 2", at(20, 20)),
    failure("gate", "unknown", "release-gate: required stage did not succeed", null, { job: "release gate", step: null }),
    failure("stray", "timeout", "a failure no edge reaches", null),
  ],
  edges: [
    { from: "launch", to: "inv", kind: "wraps", confidence: "inferred" },
    { from: "exit", to: "launch", kind: "wraps", confidence: "inferred" },
    { from: "gate", to: "exit", kind: "reports", confidence: "observed" },
  ],
  originating: ["inv"],
  coverage: { read: [{ artifact, from: 1, to: 30, scope: "failed stages only", complete: false }], notRead: [{ job: "docs preview", origin: "stage log", reason: "stage succeeded" }] },
};

describe("FailureSummary", () => {
  it("is a packaged, interactive view matched by its declared input", () => {
    const module = valueViewModules.named("failure-summary");
    expect(module?.definition?.name).toBe("FailureSummary");
    expect(module?.definition?.interaction?.sharedFields).toEqual(["failure", "evidence"]);
    expect(() => decodeContract(definition, definition.input, run)).not.toThrow();
    for (const bad of [
      { ...run, view: "span-timeline" },
      { ...run, failures: [{ ...run.failures[0]!, kind: "flaky" }] },
      { ...run, edges: [{ ...run.edges[0]!, kind: "follows" }] },
    ]) expect(() => decodeContract(definition, definition.input, bad)).toThrow();
  });

  it("walks each originating failure outward and keeps unreached failures apart", () => {
    const model = prepare(run);
    expect(model.origins.map((o) => o.failure.id)).toEqual(["inv"]);
    expect(model.origins[0]!.chain.map((l) => [l.failure.id, l.edge.kind, l.depth])).toEqual([["launch", "wraps", 0], ["exit", "wraps", 1], ["gate", "reports", 2]]);
    expect(model.unlinked.map((f) => f.id)).toEqual(["stray"]);
  });

  it("refuses unknown or repeated ids and survives a cycle", () => {
    expect(() => prepare({ ...run, originating: ["nope"] })).toThrow("originating failure nope is unknown");
    expect(() => prepare({ ...run, edges: [{ from: "x", to: "inv", kind: "wraps", confidence: "observed" }] })).toThrow("names an unknown failure");
    expect(() => prepare({ ...run, failures: [...run.failures, run.failures[0]!] })).toThrow("listed twice");
    const cycle = prepare({ ...run, edges: [...run.edges, { from: "inv", to: "gate", kind: "caused_by", confidence: "uncertain" }] });
    expect(cycle.origins[0]!.chain.map((l) => l.failure.id)).toEqual(["launch", "exit", "gate"]);
  });

  it("names evidence lines, confidence, location and what was not read", () => {
    const html = render(run);
    expect(html).toContain("1 originating");
    expect(html).toContain("lines 12–13");
    expect(html).toContain("line 20");
    expect(html).toContain("← wrapped by (inferred):");
    expect(html).toContain("← reported by:");
    expect(html).toContain("<code>tools/packer.toml</code>");
    expect(html).toContain("no evidence lines");
    expect(html).toContain("1 not linked to an originating failure");
    expect(html).toContain("Read 1 log range, some partial · not read: docs preview (stage succeeded)");
    const many = { job: "a", origin: "stage log", reason: "complete stage logs unavailable" };
    expect(render({ ...run, coverage: { ...run.coverage, notRead: [many, { ...many, job: "b" }, run.coverage.notRead[0]!] } }))
      .toContain("not read: 2 jobs (complete stage logs unavailable); docs preview (stage succeeded)");
    expect(render(run, "preview")).toContain("line 20");
  });

  it("says nothing was recognized in what was read, never that nothing failed, with the scope in every tier", () => {
    const quiet: Input = { ...run, failures: [], edges: [], originating: [],
      coverage: { read: [{ artifact, from: 1, to: 30, scope: "Unrecognized formats are not a no-failure verdict.", complete: true }], notRead: [] } };
    for (const mode of ["preview", "expanded", "window"] as const) {
      const html = render(quiet, mode);
      expect(html).toContain("No failure observation was recognized in what was read. This is not a verdict that nothing failed.");
      expect(html).toContain("Unrecognized formats are not a no-failure verdict.");
      expect(html).not.toMatch(/No originating failure was found|no failures? (?:was|were) found|\bpassed\b|succeeded/i);
    }
  });

  it("selects evidence as state and a picked event; choosing it again does nothing; close and Escape clear it", () => {
    const keys = new Map<string, (event: { key: string; preventDefault: () => void }) => void>();
    vi.stubGlobal("window", { addEventListener: (name: string, fn: never) => keys.set(name, fn), removeEventListener: (name: string) => keys.delete(name) });
    const events: State[] = [];
    function Harness() {
      const [state, setState] = useState(empty);
      return <summary.Component input={run} state={state} revision={0} slots={{}} context={context("expanded")}
        emit={(event) => { events.push(event); setState((previous) => summary.reduce!(previous, event)); }} />;
    }
    let tree!: ReturnType<typeof create>;
    act(() => { tree = create(<Harness />); });
    const button = () => tree.root.findAll((node) => node.type === "button" && node.children.join("") === "lines 12–13")[0]!;
    act(() => button().props.onClick());
    const picked = summary.reduce!(empty, events[0]!);
    expect(button().props["aria-pressed"]).toBe(true);
    expect(summary.outputs!(picked)).toEqual({ evidence: at(12, 13) });
    expect(summary.eventOutputs!(empty, picked, events[0]!)).toEqual([{ port: "picked", value: at(12, 13) }]);
    act(() => button().props.onClick());
    expect(events).toHaveLength(1);
    act(() => tree.root.findByProps({ "aria-label": "Close lines 12–13" }).props.onClick());
    expect(events[1]).toEqual(empty);
    expect(summary.eventOutputs!(picked, empty, events[1]!)).toEqual([]);
    act(() => button().props.onClick());
    act(() => keys.get("keydown")!({ key: "Escape", preventDefault: () => {} }));
    expect(events.at(-1)).toEqual(empty);
    expect(keys.has("keydown")).toBe(false);
    vi.unstubAllGlobals();
    act(() => tree.unmount());
  });

  it("shows exact line numbers as their lexemes", () => {
    const exact = { artifact: { ...artifact, attempt: new ExactNumber("1") }, from: new ExactNumber("9007199254740993"), to: new ExactNumber("9007199254740993") };
    const html = render({ ...run, failures: [failure("inv", "invocation", "t", exact as unknown as ReturnType<typeof at>)], edges: [] });
    expect(html).toContain(">line 9007199254740993<");
    expect(html).toContain("attempt 1");
  });

  it("keeps a preview to three origins without details", () => {
    const many = { ...run, originating: ["inv", "launch", "exit", "gate"], edges: [] };
    const html = render(many, "preview");
    expect(html).toContain("+1 originating");
    expect(html).not.toContain("packer usage:");
  });

  it("opens the selected failure's lines in place, marks the evidence, and says what it does not carry", () => {
    const selected = (id: string, evidence: State["evidence"]): State => ({ failure: id, evidence });
    const open = render(run, "expanded", selected("inv", at(12, 13)));
    expect(open).toContain("failure-summary-excerpt");
    expect(open.match(/aria-current="true"/g)).toHaveLength(2);
    expect(open).toContain("launcher: stopping after the packer step");
    expect(open).not.toContain('class="failure-summary-detail"');
    expect(render(run)).not.toContain("failure-summary-excerpt");
    expect(render(run, "expanded", selected("launch", at(14, 19)))).toContain("These lines were not retained with the summary.");
    const cut = { ...run, failures: [{ ...run.failures[0]!, evidence: at(12, 18) }, ...run.failures.slice(1)] };
    expect(render(cut, "expanded", selected("inv", at(12, 18)))).toContain("Lines 15–18 are past the excerpt limit.");
  });

  /** Shaped like the native investigation recipe's summary: compact observations, inferred wraps from
   *  the scoped log sequence, no originating failure named, and a scope caveat on the one read range. */
  const native: Input = {
    view: "failure-summary",
    title: "Execution evidence · synthetic-run",
    failures: [
      failure("panic", "test", "paced_reader", at(127, 127), { detail: "rust-panic; original bytes 3810..3871" }),
      failure("assert", "test", "not ok 1 - responsive input", at(129, 129), { detail: "node-assertion; original bytes 3900..3990" }),
      failure("trace", "unknown", "subprocess.CalledProcessError: nested test exited with status 101", at(131, 133)),
    ],
    edges: [{ from: "panic", to: "assert", kind: "wraps", confidence: "inferred" }],
    originating: [],
    coverage: { read: [{ artifact, from: 1, to: 646, scope: "All framed records examined; only recognized observations retained. Unclassified text is not a no-failure verdict.", complete: true }], notRead: [] },
  };

  it("shows observations with no originating failure as unranked findings, never as roots or as nothing found", () => {
    for (const mode of ["preview", "expanded", "window"] as const) {
      const html = render(native, mode);
      expect(html).toContain("3 observations · none is marked as originating");
      expect(html).not.toContain("No originating failure was found");
      expect(html).not.toMatch(/\d+ originating/);
      expect(html).toContain("paced_reader");
      expect(html).toContain("← wrapped by (inferred): paced_reader");
    }
    expect(render(native)).not.toContain("not linked to an originating failure");
    expect(render(native)).toContain("Unclassified text is not a no-failure verdict.");
    expect(render(native, "preview")).not.toContain("Unclassified text");
    expect(prepare(native).origins).toEqual([]);
  });

  it("says an unavailable log was not read instead of reporting no failure", () => {
    const unavailable: Input = { ...native, failures: [], edges: [], coverage: { read: [], notRead: [{ job: "worker", origin: "synthetic:run", reason: "Captured log availability: expired; no log was scanned and no failure cause is inferred" }] } };
    const html = render(unavailable);
    expect(html).toContain("No log was read, so no failure is identified or ruled out.");
    expect(html).not.toContain("No originating failure was found");
    expect(html).toContain("not read: worker (Captured log availability: expired");
  });

  it("keeps a selected range while focus is elsewhere and only reads it as inactive", () => {
    const chosen: State = { failure: "inv", evidence: at(12, 13) };
    const html = renderToStaticMarkup(<summary.Component input={run} state={chosen} revision={1} slots={{}} context={{ ...context("expanded"), active: false }} emit={() => {}} />);
    expect(html).toContain('data-active="false"');
    expect(html).toContain('aria-pressed="true"');
    expect(html).toContain("failure-summary-excerpt");
  });
});
