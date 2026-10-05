import { readFileSync } from "node:fs";
import { expect, it } from "vitest";

it("should_WrapEveryNoticeLineWhole_When_AMessageIsWiderThanTheCell", () => {
  // Arrange
  const css = readFileSync(new URL("./presentation.css", import.meta.url), "utf8");
  // Act
  const rules = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const rule = [...rules.matchAll(/([^{}]+)\{([^}]*)\}/g)]
    .find(match => match[1]!.split(",").some(selector => selector.trim() === ".wes-terminal .value-notice .value-line"))?.[2] ?? "";
  // Assert: more specific than `.wes-terminal .mono-line`, so its ellipsis never applies here.
  expect(rule).toMatch(/white-space: pre-wrap;/);
  expect(rule).toMatch(/overflow: visible;/);
  expect(rule).toMatch(/text-overflow: clip;/);
});
