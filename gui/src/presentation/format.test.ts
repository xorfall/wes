import { expect, it } from "vitest";
import { compactDecimal, formatWire, formatDuration, formatExecutionDuration } from "./format";

it("shows sub-millisecond run spans as a resolution bound without changing Duration values", () => {
  expect(formatExecutionDuration(0)).toBe("<1 ms");
  expect(formatExecutionDuration(0.75)).toBe("<1 ms");
  expect(formatExecutionDuration(1)).toBe("1 ms");
  expect(formatExecutionDuration(12)).toBe("12 ms");
  expect(formatDuration(0)).toBe("0.0 ms");
  expect(formatDuration(0.75)).toBe("0.8 ms");
  expect(formatDuration(-1)).toBe("-1.0 ms");
});

it("carries rounded seconds into the next minute", () => {
  expect(formatExecutionDuration(119_400)).toBe("1 min 59 s");
  expect(formatExecutionDuration(119_600)).toBe("2 min");
  expect(formatExecutionDuration(179_500)).toBe("3 min");
});

it("preserves exact temporal scalars without a lossy browser Date conversion", () => {
  for (const [kind, value] of [
    ["INSTANT", "2025-01-01T12:00:00.000000001Z"],
    ["INSTANT", "+1000000000-12-31T23:59:59.999999999Z"],
    ["DURATION", "PT-0.000000001S"],
    ["INTERVAL", "2025-01-01T12:00:00Z/2025-01-01T12:00:00Z"],
  ]) expect(formatWire(kind!, value, "en-US", "UTC")).toBe(value);
});

it("trims only redundant fractional display zeros without expanding exponents", () => {
  for (const [input, output] of [["1790000000.000000000", "1790000000"], ["-0.500000", "-0.5"], ["1.000e+100000", "1e+100000"], ["1.234000e-20", "1.234e-20"], ["1000", "1000"]]) expect(compactDecimal(input!)).toBe(output);
});
