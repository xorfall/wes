import { describe, expect, it } from "vitest";
import { readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

/**
 * macOS resolves `./Split` and `./split` to the same module, so two modules whose names differ only
 * by case are one module to the bundler and two to the repository. It fails at render time with
 * "element type is invalid", a long way from the cause.
 *
 * Only modules: a stylesheet is always imported with its extension, so `Cell.tsx` beside `cell.css`
 * is two files and stays two.
 */
describe("the surface's file names", () => {
  it("should_DifferBySomethingOtherThanCase_When_TwoFilesAreNamedAlike", () => {
    const roots = ["./", "./forms/", "./screens/"];
    for (const root of roots) {
      const here = fileURLToPath(new URL(root, import.meta.url));
      const bases = readdirSync(here)
        .filter((name) => /\.tsx?$/.test(name))
        .map((name) => name.replace(/\.tsx?$/, ""));
      const seen = new Map<string, string>();
      for (const base of bases) {
        const folded = base.toLowerCase();
        const already = seen.get(folded);
        expect(already, `${root}${base} and ${root}${already} differ only by case`).toBeUndefined();
        seen.set(folded, base);
      }
    }
  });
});
