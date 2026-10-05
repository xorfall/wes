import { expect, it } from "vitest";
import { read } from "./commands";
import { commandSegments } from "./command-line";
import { failureOf } from "./session-model";
import { failureText, locationLines } from "../failure-text";
import { locatedError, locationSummary } from "./testing/located-error";

it("routes comment-prefixed scripts to the engine, without interpreting comments as UI commands", () => {
  expect(read("// explanation\n:calc { return 1; }")).toEqual({ kind: "engine" });
  expect(read("// /clear")).toEqual({ kind: "engine" });
  expect(read("/clear").kind).toBe("clear");
});
it("dims comments but preserves URLs and the source spelling", () => {
  const source = 'api get url:https://example.invalid value:"//literal" // note';
  const segments = commandSegments(source);
  expect(segments.map(s => s.text).join("")).toBe(source);
  expect(segments.find(s => s.text === "note")?.role).toBe("mono-faint");
  expect(segments.some(s => s.text.includes("https://example.invalid") && s.role !== "mono-faint")).toBe(true);
});
it("uses the same structured location data in cells and complete error text", () => {
  expect(failureOf("summary", locatedError).span).toBe(locationSummary);
  expect(failureText(undefined, locatedError)).toContain("line 3, column 10");
  expect(failureText(undefined, locatedError)).toContain("called from fixture.wes");
  expect(failureOf("source bytes 4..8")).toEqual({ message: "source bytes 4..8" });
});

it("compacts only generated cell identities without losing file names or full diagnostics", () => {
  const id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
  const locations = [
    { ...locatedError.locations![0]!, source: `cell ${id}` },
    { ...locatedError.locations![1]!, source: "library.wes" },
  ];
  expect(locationLines(locations, "other-cell")).toEqual([
    "cell aaaaaaaa · line 3, column 10", "called from library.wes · line 2, column 4",
  ]);
  expect(failureText(undefined, { ...locatedError, locations })).toContain(id);
});
