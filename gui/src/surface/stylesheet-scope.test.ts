import { readFileSync, readdirSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, it } from "vitest";

const ROOT = fileURLToPath(new URL(".", import.meta.url));
const SCOPE = ".wes-terminal";

function stylesheets(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    return entry.isDirectory() ? stylesheets(path) : entry.name.endsWith(".css") ? [path] : [];
  });
}

/** Top-level comma parts of a selector list; commas inside `:is(…)` or `[…]` stay with their part. */
function parts(selector: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let current = "";
  for (const char of selector) {
    if (char === "(" || char === "[") depth += 1;
    if (char === ")" || char === "]") depth -= 1;
    if (char === "," && depth === 0) { out.push(current); current = ""; } else current += char;
  }
  return [...out, current].map((part) => part.trim()).filter(Boolean);
}

/** Every style rule's selector parts, at any nesting depth; at-rule preludes are not selectors. */
function selectors(css: string): string[] {
  const body = css.replace(/\/\*[\s\S]*?\*\//g, "");
  return [...body.matchAll(/([^{};]+)\{/g)]
    .map((match) => match[1]!.trim())
    .filter((prelude) => !prelude.startsWith("@"))
    .flatMap(parts)
    .filter((part) => !/^(from|to|\d+(\.\d+)?%)$/.test(part));
}

it("should_ScopeEverySurfaceRuleUnderTheTerminal_When_TheResetWouldOtherwiseWinTheCascade", () => {
  // Arrange: `.wes-terminal [class]` (0,2,0) clears borders, backgrounds, padding and font, so an
  // unscoped single-class rule (0,1,0) silently loses every one of those declarations.
  const unscoped = stylesheets(ROOT).flatMap((file) =>
    // Act
    selectors(readFileSync(file, "utf8"))
      .filter((selector) => !selector.startsWith(SCOPE))
      .map((selector) => `${relative(ROOT, file)}: ${selector}`));
  // Assert
  expect(unscoped).toEqual([]);
});
