/** Canonical UTC Instant/Interval wire values; pixels alone use floating point. */
export interface TimeRange { readonly start: string; readonly end: string }
const DAY = 86_400_000_000_000n, SECOND = 1_000_000_000n;
const floor = (a: bigint, b: bigint) => a / b - (a % b < 0n ? 1n : 0n);
const yearStart = (y: bigint) => 365n * y + floor(y + 3n, 4n) - floor(y + 99n, 100n) + floor(y + 399n, 400n);
const EPOCH = yearStart(1970n);
const months = (y: number) => [31, y % 4 === 0 && (y % 100 !== 0 || y % 400 === 0) ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
export function nanos(value: unknown): bigint | undefined {
  if (typeof value !== "string" || value.length > 48) return undefined;
  const match = /^([+-]?\d{4,10})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?Z$/.exec(value);
  if (!match) return undefined;
  const y = Number(match[1]), m = Number(match[2]), d = Number(match[3]);
  const h = Number(match[4]), min = Number(match[5]), sec = Number(match[6]);
  const md = months(y);
  if (Math.abs(y) > 999_999_999 || m < 1 || m > 12 || d < 1 || d > md[m - 1]! || h > 23 || min > 59 || sec > 59) return undefined;
  const days = yearStart(BigInt(y)) - EPOCH + BigInt(md.slice(0, m - 1).reduce((a, b) => a + b, d - 1));
  return days * DAY + BigInt(h * 3600 + min * 60 + sec) * SECOND + BigInt((match[7] ?? "").padEnd(9, "0"));
}
export function instant(value: bigint): string {
  const day = floor(value, DAY), rest = value - day * DAY, absolute = day + EPOCH;
  let lo = -1_000_000_000, hi = 1_000_000_001;
  while (hi - lo > 1) { const mid = Math.floor((hi + lo) / 2); if (yearStart(BigInt(mid)) <= absolute) lo = mid; else hi = mid; }
  if (Math.abs(lo) > 999_999_999) throw new Error("Instant outside supported range");
  let d = Number(absolute - yearStart(BigInt(lo))), month = 0;
  const md = months(lo);
  while (d >= md[month]!) { d -= md[month]!; month++; }
  const pad = (n: number | bigint) => String(n).padStart(2, "0");
  const year = lo < 0 ? `-${String(-lo).padStart(4, "0")}` : lo > 9999 ? `+${lo}` : String(lo).padStart(4, "0");
  const fraction = String(rest % SECOND).padStart(9, "0").replace(/0+$/, "");
  return `${year}-${pad(month + 1)}-${pad(d + 1)}T${pad(rest / (3600n * SECOND))}:${pad(rest / (60n * SECOND) % 60n)}:${pad(rest / SECOND % 60n)}${fraction ? `.${fraction}` : ""}Z`;
}
export function range(value: unknown): TimeRange | undefined {
  if (typeof value !== "string" || value.length > 100) return undefined;
  const [start, end, extra] = value.split("/");
  if (extra !== undefined || start === undefined || end === undefined) return undefined;
  const a = nanos(start), b = nanos(end);
  return a !== undefined && b !== undefined && a <= b ? { start, end } : undefined;
}
export function validRange(value: unknown): value is TimeRange {
  if (typeof value !== "object" || value === null) return false;
  const r = value as TimeRange, a = nanos(r.start), b = nanos(r.end);
  return a !== undefined && b !== undefined && a <= b;
}
export const span = (r: TimeRange) => nanos(r.end)! - nanos(r.start)!;
export const contains = (r: TimeRange, at: string) => nanos(at)! >= nanos(r.start)! && nanos(at)! < nanos(r.end)!;
export const ratio = (at: bigint, r: TimeRange) => {
  const width = span(r); return width === 0n ? 0 : Number(at - nanos(r.start)!) / Number(width);
};
export function atRatio(r: TimeRange, ratio: number): string {
  const part = BigInt(Math.round(Math.max(0, Math.min(1, ratio)) * 1_000_000_000));
  return instant(nanos(r.start)! + span(r) * part / 1_000_000_000n);
}
export const clock = (at: string) => at.slice(at.indexOf("T") + 1, -1);
export function fitRange(request: TimeRange, limit: TimeRange): TimeRange {
  return fitNanos(nanos(request.start)!, nanos(request.end)!, limit);
}
export function fitNanos(start: bigint, end: bigint, limit: TimeRange): TimeRange {
  const a = nanos(limit.start)!, b = nanos(limit.end)!;
  const size = end - start;
  if (size >= b - a) return limit;
  if (start < a) { start = a; end = a + size; }
  if (end > b) { end = b; start = b - size; }
  return { start: instant(start), end: instant(end) };
}
