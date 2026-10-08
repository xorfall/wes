import { renderToStaticMarkup } from "react-dom/server";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { valueViewModules } from "./registry";
import { decodeContract } from "./definition";
import comparison, { facts, prepare } from "../../../views/execution-comparison/View";
import { definition, type Input } from "../../../views/execution-comparison/contract";

/*
 * Independently synthetic comparisons, exactly the InvestigationComparison type and the four cases
 * CompareExecutions produces for a batch target: invented run names, no captured provider data.
 */

type Mode = "preview" | "expanded" | "window";
const render = (input: Input, mode: Mode = "expanded") =>
  renderToStaticMarkup(<comparison.Component input={input} state={undefined as never} revision={0} emit={() => {}} slots={{}} context={{ mode, instance: null }} />);
type Row = Input["comparisons"][number];
const row = (over: Partial<Row>): Row => ({
  subject: "job-red", baseline: "job-green", target: "transform", inputComparable: true, environmentComparable: null, targetExercised: true,
  hypothesis: "different_outcomes", rationale: "The same target and input produced observed outcomes; environment and cause still require investigation",
  regressionRuledOut: false, ...over,
});
const skipped = row({ targetExercised: false, hypothesis: "incomparable", rationale: "Comparable target execution and input evidence are required" });
const unknownEnvironment = row({});
const equalEnvironment = row({ environmentComparable: true, hypothesis: "suspected_intermittence", rationale: "The captured target, input and environment match but outcomes differ; cause unknown" });
const changedInput = row({ inputComparable: false, environmentComparable: true, hypothesis: "incomparable", rationale: "Comparable target execution and input evidence are required" });
const report: Input = { title: "Synthetic batch comparison", comparisons: [skipped, unknownEnvironment, equalEnvironment, changedInput] };

describe("ExecutionComparison", () => {
  it("is a packaged, read-only view matched exactly by the investigation comparison type", () => {
    const module = valueViewModules.named("execution-comparison");
    expect(module?.definition?.name).toBe("ExecutionComparison");
    expect(definition.input).toBe("ExecutionComparisonReport");
    expect(definition.interaction).toBeNull();
    expect(definition.outputs).toEqual({});
    expect(Object.keys((definition.contracts.InvestigationComparison as { fields: object }).fields).sort()).toEqual(
      ["baseline", "environmentComparable", "hypothesis", "inputComparable", "rationale", "regressionRuledOut", "subject", "target", "targetExercised"]);
    expect(() => decodeContract(definition, definition.input, report)).not.toThrow();
    // A wrong type, a broken constraint or a missing required field is refused.
    expect(() => decodeContract(definition, definition.input, { ...report, comparisons: [{ ...skipped, environmentComparable: "unknown" }] })).toThrow();
    expect(() => decodeContract(definition, definition.input, { ...report, comparisons: [{ ...skipped, subject: "" }] })).toThrow();
    expect(() => decodeContract(definition, definition.input, { ...report, comparisons: [(({ regressionRuledOut: _r, ...rest }) => rest)(skipped)] })).toThrow();
  });

  it("projects an undeclared field away, so it grants no claimed cause", () => {
    // Record decoding keeps only declared fields; an extra one is not refused, and nothing reaches the View.
    const decoded = decodeContract(definition, definition.input, { ...report, comparisons: [{ ...skipped, cause: "synthetic flaky cause" }] }) as Input;
    expect(Object.keys(decoded.comparisons[0]!)).not.toContain("cause");
    const html = render(decoded);
    expect(html).not.toContain("synthetic flaky cause");
    expect(html).toContain("Not comparable");
  });

  it("states target execution, input and environment as separate facts, an absent environment as unknown", () => {
    const said = (c: Row) => Object.fromEntries(facts(c).map((f) => [f.label, f.text]));
    expect(said(skipped)).toMatchObject({ target: "not exercised in both · outcomes are not comparable", input: "same captured input", environment: "unknown", regression: "not ruled out" });
    expect(said(unknownEnvironment)).toMatchObject({ target: "ran in both executions", environment: "unknown" });
    expect(said(equalEnvironment).environment).toBe("same captured environment");
    expect(said(changedInput)).toMatchObject({ input: "not shown to be the same", environment: "same captured environment" });
    expect(said(row({ environmentComparable: false })).environment).toBe("differs");
    const model = prepare(report);
    expect(model.rows.map((r) => r.comparable)).toEqual([false, true, true, false]);
    expect([model.incomparable, model.notExercised, model.environmentUnknown]).toEqual([2, 1, 2]);
  });

  it("makes incomparable and not-exercised comparisons obvious and shows the actual rationale", () => {
    const html = render(report);
    expect(html).toContain("4 comparisons");
    expect(html).toContain("2 not comparable");
    expect(html).toContain("1 target not exercised");
    expect(html).toContain("2 environment unknown");
    expect(html.match(/data-comparable="false"/g)).toHaveLength(2);
    expect(html).toContain("Not comparable");
    expect(html).toContain("Suspected intermittence · cause unknown");
    expect(html).toContain("The captured target, input and environment match but outcomes differ; cause unknown");
    expect(html).toContain("subject <code title=\"job-red\">job-red</code>");
    expect(html).toContain("baseline <code title=\"job-green\">job-green</code>");
    expect(html).toContain("A matching input or source tree does not establish the same provider, environment, fix or cause.");
  });

  it("never claims a fix, a cause, a ruled-out regression or equal environments it was not given", () => {
    const same = render({ title: "t", comparisons: [row({ hypothesis: "same_observed_outcome", rationale: "Both executions report the same target outcome; this does not verify a fix" })] });
    expect(same).toContain("Same observed outcome · not a verified fix");
    expect(same).toContain("not ruled out");
    for (const html of [render(report), same]) expect(html).not.toMatch(/\bfixed\b|root cause|caused by|passing fix|regression ruled out/i);
    // An absent environment is never drawn as a match.
    expect(render({ title: "t", comparisons: [unknownEnvironment] })).not.toContain("same captured environment");
    // An unrecognised hypothesis is shown as given, not mapped onto a known one.
    expect(render({ title: "t", comparisons: [row({ hypothesis: "synthetic_new_hypothesis" })] })).toContain("synthetic_new_hypothesis");
  });

  it("keeps a preview to two comparisons with their facts, without rationale", () => {
    const html = render(report, "preview");
    expect(html.match(/class="execution-comparison-item"/g)).toHaveLength(2);
    expect(html).toContain("+2 comparisons");
    expect(html).toContain("not exercised in both");
    expect(html).not.toContain("execution-comparison-rationale");
    expect(render({ title: "empty", comparisons: [] }, "window")).toContain("No comparisons.");
  });

  it("bounds its list inside the frame and wraps long run references instead of widening the page", () => {
    const css = readFileSync(new URL("../../../views/execution-comparison/view.css", import.meta.url), "utf8");
    expect(css).toMatch(/\.execution-comparison-list \{[^}]*min-height:0;[^}]*overflow-y:auto;[^}]*overflow-x:hidden;/);
    expect(css).toMatch(/\.execution-comparison-runs span \{[^}]*text-overflow:ellipsis;/);
    expect(css).not.toMatch(/width:\s*\d+px/);
    // Labels and body text are at least 12px.
    for (const [, size] of css.matchAll(/font-size:([\d.]+)px/g)) expect(Number(size)).toBeGreaterThanOrEqual(12);
    const long = "r".repeat(1024);
    expect(render({ title: "t", comparisons: [row({ subject: long })] })).toContain(`title="${long}"`);
  });
});
