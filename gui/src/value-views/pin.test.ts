import { describe, expect, it } from "vitest";
import type { ViewInstance } from "./instances";
import { freshPinName, pinCommand, pinRefusal, referenceLabel } from "./pin";

const input = { type: { kind: "primitive" as const, name: "TEXT" }, data: "synthetic", provenance: {} };
function entry(over: Partial<ViewInstance> = {}): ViewInstance {
  return { id: "chart", instance: "chart-identity", revision: "3", inputRevision: "7", definition: "metric", digest: "d",
    inputReference: { kind: "current", node: "orders", port: "data", fields: ["body"], shownRun: "r1" }, inputDelivery: "finite",
    observing: true, inputProblem: null, inputCautions: [], query: null, linkedInputs: [], input, members: {}, ...over };
}

describe("freshPinName", () => {
  it("should_ChooseTheFirstUnusedName_When_EarlierCandidatesAreTaken", () => {
    // Arrange
    const taken = new Set(["chart_pin", "chart_pin2"]);
    // Act
    const name = freshPinName("chart", taken);
    // Assert
    expect(name).toBe("chart_pin3");
  });

  it("should_FallBackToAValidBase_When_TheViewIdIsOutsideTheBindingGrammar", () => {
    // Arrange
    const view = "chart²";
    // Act
    const name = freshPinName(view, new Set());
    // Assert
    expect(name).toBe("pinned_pin");
  });
});

describe("pinRefusal", () => {
  it.each([
    ["linked input fields", { linkedInputs: ["selection"] }, /linked input fields/],
    ["an already pinned input", { inputReference: { kind: "retained" as const, node: "p", run: "r", handle: "h", origin: null } }, /already shows a pinned input/],
    ["no displayed input", { input: null }, /no displayed input/],
    ["an unapplied query", { input: null, query: { environment: null, template: "T", mode: "finite" as const, adapter: null, source: "s", output: "o", trigger: "manual" as const, running: false } }, /Apply the query first/],
    ["an input problem", { inputProblem: "Source unavailable" }, /Source unavailable/],
  ])("should_ExplainWhyPinIsUnavailable_When_TheViewHas %s", (_case, over, reason) => {
    // Arrange
    const view = entry(over);
    // Act
    const refusal = pinRefusal(view);
    // Assert
    expect(refusal).toMatch(reason);
  });

  it("should_AllowPin_When_ACurrentOrQueryMaterializedInputIsDisplayed", () => {
    // Arrange
    const query = entry({ inputReference: { kind: "unlinked" }, query: { environment: null, template: "T", mode: "live", adapter: "A", source: "s", output: "o", trigger: "manual", running: true } });
    // Act
    const refusals = [pinRefusal(entry()), pinRefusal(query)];
    // Assert
    expect(refusals).toEqual([undefined, undefined]);
  });
});

describe("pinCommand", () => {
  it("should_GuardTheExactVisibleFrame_When_BuildingThePinCommand", () => {
    // Arrange
    const view = entry();
    // Act
    const command = pinCommand(view, "chart_pin", JSON.stringify);
    // Assert
    expect(command).toBe(':view pin $chart instance:"chart-identity" revision:3 inputRevision:7 > chart_pin');
  });

  it("should_RefuseToBuildACommand_When_ARevisionIsNotAnInteger", () => {
    // Arrange
    const view = entry({ revision: "3 > other" });
    // Act
    const build = () => pinCommand(view, "chart_pin", JSON.stringify);
    // Assert
    expect(build).toThrow(/reopen it/);
  });
});

describe("referenceLabel", () => {
  it("should_NameTheFollowedOutputAndStreamBudget_When_TheReferenceIsCurrent", () => {
    // Arrange
    const view = entry({ inputDelivery: "window" });
    // Act
    const label = referenceLabel(view);
    // Assert
    expect(label).toEqual({ kind: "Current", detail: "$orders.body · stream window", inputRun: "r1" });
  });

  it("keeps finite run identity visible and does not imply a sample before one arrives", () => {
    expect(referenceLabel(entry())).toEqual({ kind: "Current", detail: "$orders.body · shown run r1" });
    expect(referenceLabel(entry({ inputDelivery: "window", inputReference: { kind: "current", node: "orders", port: "data", fields: ["body"], shownRun: null } })))
      .toEqual({ kind: "Current", detail: "$orders.body · stream window · no input run yet" });
  });

  it("should_NameTheRetainedRunAndItsAuditedOrigin_When_TheReferenceIsPinned", () => {
    // Arrange
    const reference = { kind: "retained" as const, node: "chart_pin", run: "r9", handle: "h", origin: { node: "orders", port: "data" as const, fields: ["body"], run: "r1" } };
    // Act
    const label = referenceLabel(entry({ inputReference: reference }));
    // Assert
    expect(label).toEqual({ kind: "Pinned", detail: "$chart_pin run r9 · from $orders.body run r1" });
  });
});

describe("referenceLabel for unlinked inputs", () => {
  it.each([
    ["a literal", {}, { kind: "Literal input", detail: "not linked to a result" }],
    ["a removed source", { input: null, inputProblem: "Input work was removed; bind another result" }, { kind: "No input", detail: "not linked to a result" }],
    ["an applied query", { query: { environment: null, template: "Watch", mode: "finite" as const, adapter: null, source: "s", output: "o", trigger: "manual" as const, running: false } }, { kind: "Query input", detail: "from template Watch" }],
  ])("should_NotCallEveryUnlinkedInputALiteral_When_TheViewHas %s", (_case, over, expected) => {
    // Arrange
    const view = entry({ inputReference: { kind: "unlinked" }, ...over });
    // Act
    const label = referenceLabel(view);
    // Assert
    expect(label).toEqual(expected);
  });
});

describe("referenceLabel output ports", () => {
  it.each([
    ["data", "$orders.body"],
    ["error", "$orders.error.body"],
    ["cancel", "$orders.cancel.body"],
  ] as const)("should_NameThe%sPortExactly_When_ACurrentReferenceFollowsIt", (port, expected) => {
    // Arrange
    const view = entry({ inputReference: { kind: "current", node: "orders", port, fields: ["body"], shownRun: null } });
    // Act
    const label = referenceLabel(view);
    // Assert
    expect(label).toEqual({ kind: "Current", detail: `${expected} · no run shown yet` });
  });

  it("should_KeepACancellationOriginDistinctFromError_When_ThePinnedInputCameFromACancelOutput", () => {
    // Arrange
    const reference = { kind: "retained" as const, node: "chart_pin", run: "r9", handle: "h", origin: { node: "orders", port: "cancel" as const, fields: [], run: "r4" } };
    // Act
    const label = referenceLabel(entry({ inputReference: reference }));
    // Assert
    expect(label).toEqual({ kind: "Pinned", detail: "$chart_pin run r9 · from $orders.cancel run r4" });
  });
});
