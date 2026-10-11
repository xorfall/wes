import { afterEach, expect, it, vi } from "vitest";
import { clusters, ellipsizeEnd, ellipsizeMiddle, textPrefix, TEXT_MEASURE_UNITS, width } from "./columns";

afterEach(() => vi.restoreAllMocks());

it("bounds preview segmentation by visible width instead of payload length", () => {
  const native = vi.spyOn(Intl.Segmenter.prototype, "segment").mockImplementation(() => { throw new Error("Native segmentation is unavailable"); });
  const source = "a".repeat(1_000_000);
  expect(ellipsizeEnd(source, 80)).toBe("a".repeat(79) + "…");
  expect(width(source, 80)).toBeGreaterThan(80);
  expect(native).not.toHaveBeenCalled();
});

it("preserves extended grapheme boundaries for Hangul, Indic conjuncts and regional indicators", () => {
  const sequences = ["한", "क्ष", "🇹🇷", "👩🏽‍💻", "e\u0301"];
  for (const sequence of sequences) expect([...clusters(sequence + "x")]).toEqual([sequence, "x"]);
});

it("drops an unfinished cluster at the measurement boundary", () => {
  const source = "x".repeat(TEXT_MEASURE_UNITS - 2) + "e" + "\u0301".repeat(100_000);
  expect(textPrefix(source)).toEqual({ text: "x".repeat(TEXT_MEASURE_UNITS - 2), whole: false });
  expect(ellipsizeEnd("e" + "\u0301".repeat(100_000), 80)).toBe("…");
});

it("keeps both ends of a huge name without materializing all its clusters", () => {
  expect(ellipsizeMiddle("start" + "x".repeat(1_000_000) + "東京", 10)).toBe("start…東京");
});

it("retains combining sequences, wide glyphs and joined emoji at truncation boundaries", () => {
  expect(ellipsizeEnd("e\u0301東京abc", 5)).toBe("e\u0301東…");
  expect(ellipsizeEnd("👨‍👩‍👧‍👦abc", 4)).toBe("👨‍👩‍👧‍👦a…");
  expect(ellipsizeEnd("e\u0301東", 3)).toBe("e\u0301東");
  expect(width("e\u0301東👨‍👩‍👧‍👦")).toBe(5);
  expect(ellipsizeMiddle("abcdefghij", 5)).toBe("ab…ij");
});
