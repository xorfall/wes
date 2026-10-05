import { expect, it } from "vitest";
import { complete } from "./complete";
import { emptyCatalogue } from "./vocabulary";

it("offers the three output selectors for a reference", () => {
  const line = ":inspect $request::";
  const result = complete(line, line.length, emptyCatalogue, ["request"]);
  expect(result.from).toBe(9);
  expect(result.items.map(item => item.text)).toEqual(["$request::data", "$request::error", "$request::cancel"]);
});

it("preserves the argument key while completing a selector", () => {
  const line = "handler run input:$request::c";
  const result = complete(line, line.length, emptyCatalogue, ["request"]);
  expect(result.items.map(item => item.text)).toEqual(["input:$request::cancel"]);
  expect(line.slice(0, result.from) + result.items[0]?.text).toBe("handler run input:$request::cancel");
});

it("does not suggest commands or references for binding declarations", () => {
  for (const line of ["source read > $", "source read *> $", "source read > request *> error"]) {
    expect(complete(line, line.length, emptyCatalogue, ["request"]).items).toEqual([]);
  }
});
