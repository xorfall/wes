import { describe, expect, it } from "vitest";
import { openRoute, peekRoute, readOpenRoute, readScreenRoute, screenRoute, sourceRoute } from "./open-route";

describe("routes of a window of its own", () => {
  it("should_NameAScreen_When_TheHashSaysGraphStaleOrSettings", () => {
    // Arrange / Act / Assert
    expect(readScreenRoute(screenRoute("graph", "w1"))).toEqual({ screen: "graph", workspace: "w1" });
    expect(readScreenRoute(screenRoute("spec", "api-import-lab"))).toEqual({ screen: "spec", workspace: "api-import-lab" });
    expect(readScreenRoute(screenRoute("stale"))).toEqual({ screen: "stale" });
    expect(readScreenRoute(screenRoute("settings", "w1", "appearance"))).toEqual({ screen: "settings", section: "appearance", workspace: "w1" });
    expect(screenRoute("settings", undefined, "results")).toBe("#settings/results");
  });

  it("should_NameNoScreen_When_TheHashIsAResultOrSomethingElse", () => {
    // Arrange / Act / Assert
    expect(readScreenRoute(openRoute("n1", "result", "w1"))).toBeUndefined();
    expect(readScreenRoute(peekRoute("n1", "error"))).toBeUndefined();
    expect(readScreenRoute("#gallery/cells")).toBeUndefined();
    expect(readScreenRoute("")).toBeUndefined();
    expect(readOpenRoute(screenRoute("graph"))).toBeUndefined();
  });

  it("should_RefuseAWorkspaceThatIsNotAName_When_TheQueryCarriesOne", () => {
    // Arrange / Act / Assert
    expect(readScreenRoute("#graph?workspace=%2F%2Fnot%20a%20name")).toBeUndefined();
  });
});


it("addresses a whole command cell without a result or source text in the URL", () => {
  const route = sourceRoute("cell/with space", "source-lab");
  expect(readOpenRoute(route)).toEqual({ cell: "cell/with space", tab: "result", peek: "source", workspace: "source-lab" });
  expect(readScreenRoute(route)).toBeUndefined();
  expect(readOpenRoute("#source/")).toBeUndefined();
  expect(readOpenRoute("#source/cell?workspace=%2Fbad")).toBeUndefined();
});
