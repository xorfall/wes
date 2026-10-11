import { beforeEach, expect, it, vi } from "vitest";
const work = vi.hoisted(() => ({ units: 0, clusters: 0 }));
vi.mock("unicode-segmenter/grapheme", async importOriginal => {
  const actual = await importOriginal<typeof import("unicode-segmenter/grapheme")>();
  return {
    ...actual,
    graphemeSegments: function*(input: string) {
      work.units += input.length;
      yield* actual.graphemeSegments(input);
    },
    splitGraphemes: function*(input: string) {
      for (const part of actual.splitGraphemes(input)) { work.clusters++; yield part; }
    },
  };
});
import { present, tailOf, runText } from "./present";
import { Registry } from "./registry";
import { TEXT_MEASURE_UNITS } from "./columns";
import type { Context } from "./types";

beforeEach(() => { work.units = 0; work.clusters = 0; });
const show = (data: string, mode: Context["mode"], lines = 3) => present({
  prepared: { type: { kind: "primitive", name: "TEXT" }, data, pending: false, provenance: {} },
  registry: Registry.core(),
  context: { mode, columns: 80, lines, density: "normal", locale: "en-GB", timeZone: "UTC" },
});

it.each(["preview", "expanded", "window"] as const)("bounds %s text measurement independently of payload length", mode => {
  const shown = show("abcd ".repeat(1_000_000), mode);
  expect(shown.lines).toBeLessThanOrEqual(3);
  expect(shown.root.more?.exact).toBe(false);
  expect(work.units).toBeLessThan(TEXT_MEASURE_UNITS * 12);
  expect(work.clusters).toBeLessThan(TEXT_MEASURE_UNITS * 12);
});

it("does not allocate or count every source line just to display three", () => {
  const shown = show("line\n".repeat(1_000_000), "expanded");
  expect(shown.lines).toBe(3);
  expect(shown.root.more).toMatchObject({ exact: false });
  expect(runText(tailOf(shown))).toContain("+≥");
  expect(work.clusters).toBeLessThan(TEXT_MEASURE_UNITS * 3);
});

it("retains exact small counts and keeps partial wrapped-line counts as lower bounds", () => {
  const small = show("a\nb\nc\nd", "expanded");
  expect(small.root.more).toEqual({ lines: 1, exact: true });
  const partial = show("x".repeat(TEXT_MEASURE_UNITS + 1), "expanded", 1000);
  expect(partial.root.more).toEqual({ exact: false });
  expect(partial.root.kind === "text" && runText(partial.root.lines.at(-1)!)).toMatch(/…$/);
});

it("shows a bounded first line even when the next newline is beyond the measured prefix", () => {
  const shown = show("x".repeat(100_000) + "\nlast line", "preview");
  expect(shown.root.kind).toBe("text");
  expect(shown.lines).toBe(1);
  expect(shown.root.kind === "text" && runText(shown.root.lines[0]!)).toBe("x".repeat(79) + "…");
  expect(shown.root.more).toEqual({ exact: false });
});

it("does not invent a column count for an unfinished giant grapheme", () => {
  const shown = show("e" + "\u0301".repeat(100_000), "preview");
  expect(shown.root.more).toEqual({ exact: false });
  expect(work.units).toBeLessThan(TEXT_MEASURE_UNITS * 12);
});
