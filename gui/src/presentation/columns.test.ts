import { afterEach, expect, it, vi } from "vitest";
import { ellipsizeEnd, ellipsizeMiddle, width } from "./columns";

afterEach(() => vi.restoreAllMocks());

it("bounds preview segmentation by visible width instead of payload length", () => {
  const original = Intl.Segmenter.prototype.segment;
  let visited = 0;
  vi.spyOn(Intl.Segmenter.prototype, "segment").mockImplementation(function(this: Intl.Segmenter, input) {
    const segments = original.call(this, input);
    return { [Symbol.iterator]: function* () {
      for (const part of segments) { visited++; yield part; }
    } } as Intl.Segments;
  });
  const source = "a".repeat(1_000_000);
  expect(ellipsizeEnd(source, 80)).toBe("a".repeat(79) + "…");
  expect(visited).toBeLessThanOrEqual(162);
  visited = 0;
  expect(width(source, 80)).toBeGreaterThan(80);
  expect(visited).toBe(81);
});

it("retains combining sequences, wide glyphs and joined emoji at truncation boundaries", () => {
  expect(ellipsizeEnd("e\u0301東京abc", 5)).toBe("e\u0301東…");
  expect(ellipsizeEnd("👨‍👩‍👧‍👦abc", 4)).toBe("👨‍👩‍👧‍👦a…");
  expect(ellipsizeEnd("e\u0301東", 3)).toBe("e\u0301東");
  expect(width("e\u0301東👨‍👩‍👧‍👦")).toBe(5);
  expect(ellipsizeMiddle("abcdefghij", 5)).toBe("ab…ij");
});
