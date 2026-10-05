import { describe, expect, it } from "vitest";
import { bytes, report } from "./debug";

const place = (over: Partial<Record<string, unknown>> = {}) => ({
  name: "archive",
  where: "/tmp/values",
  holds: "kept results",
  durable: true,
  files: 56,
  bytes: 192037,
  ...over,
});

const storage = (places: unknown[]) =>
  ({ event: "storage", workspace: "default", places }) as never;

describe("saying what is kept", () => {
  /** "56 × 187.5 KB" reads as fifty-six files of that size, which is wrong by a factor of fifty-six. */
  it("should_GiveTheCountAndTheTotal_When_APlaceHoldsMoreThanOneFile", () => {
    expect(report(storage([place()]), [])).toContain("56 files, 187.5 KB");
  });

  it("should_GiveOnlyTheSize_When_APlaceHoldsOneFile", () => {
    const said = report(storage([place({ files: 1, bytes: 2148 })]), []);

    expect(said).toContain("2.1 KB");
    expect(said).not.toContain("1 files");
  });

  it("should_SayEmpty_When_NothingHasBeenWrittenThere", () => {
    expect(report(storage([place({ files: 0, bytes: 0 })]), [])).toContain("empty");
  });

  it("should_SayWhetherItSurvives_When_ListingAPlace", () => {
    expect(report(storage([place({ durable: true })]), [])).toContain("kept");
    expect(report(storage([place({ durable: false })]), [])).toContain("temporary");
  });

  it("should_SayWhereAndWhatItHolds_When_ListingAPlace", () => {
    const said = report(storage([place()]), []);

    expect(said).toContain("/tmp/values");
    expect(said).toContain("kept results");
  });

  /** An older engine, or none: saying nothing at all would look like "the engine keeps nothing". */
  it("should_SayItDidNotAnswer_When_TheEngineSaidNothingAboutStorage", () => {
    expect(report(undefined, [])).toContain("did not answer");
  });

  it("should_StillReportTheClient_When_TheEngineSaidNothing", () => {
    expect(report(undefined, [])).toContain("localStorage wes.settings");
  });

  it("should_ShowWhatTheClientWasTold_When_GivenFacts", () => {
    expect(report(undefined, [["engine", "http://127.0.0.1:8099"]])).toContain("http://127.0.0.1:8099");
  });

  it("should_StepUpTheUnit_When_TheCountIsLarge", () => {
    expect(bytes(512)).toBe("512 B");
    expect(bytes(2048)).toBe("2.0 KB");
    expect(bytes(5 * 1024 * 1024)).toBe("5.0 MB");
  });
});
