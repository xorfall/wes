/**
 * Scalar formatting for the presentation layer.
 *
 * Units come from the wire type (`INSTANT`, `DURATION`) or a validated registry format, never from
 * a field name: `timestamp_ns` is a number unless something declares it a
 * time. Formatting is display only; copy yields the raw value at full precision.
 */
import type { Format, TimeUnit } from "./registry";

const PER_MILLISECOND: Record<TimeUnit, number> = { ns: 1_000_000, us: 1_000, ms: 1, s: 0.001 };

/** Identifiers of 32 or more hex characters (Docker ids, digests), optionally `sha256:`-prefixed. */
const IDENTIFIER = /^(sha256:)?[0-9a-f]{32,}$/i;

export function isIdentifier(text: string): boolean {
  return IDENTIFIER.test(text);
}

/** `2c2c5998…a22cb2`: the preview's short form; expanded and window show the whole identifier. */
export function shortIdentifier(text: string): string {
  const prefix = text.toLowerCase().startsWith("sha256:") ? "sha256:" : "";
  const hex = text.slice(prefix.length);
  return `${prefix}${hex.slice(0, 8)}…${hex.slice(-6)}`;
}

function milliseconds(value: number | bigint | string, unit: TimeUnit): number | undefined {
  const number = typeof value === "number" ? value : Number(value);
  return Number.isFinite(number) ? number / PER_MILLISECOND[unit] : undefined;
}

/** `21:24:06.021` in the context's time zone, or the date too when it is not today. */
export function formatTime(ms: number, locale: string, timeZone: string, now = Date.now()): string {
  const date = new Date(ms);
  const time = new Intl.DateTimeFormat(locale, {
    timeZone, hour: "2-digit", minute: "2-digit", second: "2-digit", fractionalSecondDigits: 3, hourCycle: "h23",
  }).format(date);
  const day = (at: number) => new Intl.DateTimeFormat("en-CA", { timeZone, year: "numeric", month: "2-digit", day: "2-digit" }).format(at);
  return day(ms) === day(now) ? time : `${day(ms)} ${time}`;
}

/** `137 ms`, `5.6 s`, `3 min 34 s`. */
export function formatDuration(ms: number): string {
  if (ms < 1) return `${ms.toFixed(1)} ms`;
  if (ms < 10) return `${(Math.round(ms * 10) / 10).toString()} ms`;
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const s = ms / 1000;
  if (s < 60) return `${(Math.round(s * 10) / 10).toString()} s`;
  const rounded = Math.round(s);
  const minutes = Math.floor(rounded / 60);
  const seconds = rounded % 60;
  return seconds === 0 ? `${minutes} min` : `${minutes} min ${seconds} s`;
}

/** Run timestamps have finite resolution; a zero span does not prove zero work. */
export function formatExecutionDuration(ms: number): string {
  return ms >= 0 && ms < 1 ? "<1 ms" : formatDuration(ms);
}

/** `1.95 MB`: decimal units, three significant figures. */
export function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes)) return String(bytes);
  if (Math.abs(bytes) < 1000) return `${bytes} B`;
  const units = ["kB", "MB", "GB", "TB"];
  let value = bytes / 1000;
  let unit = 0;
  while (Math.abs(value) >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value.toPrecision(3)} ${units[unit]}`;
}

/** A number the way the surface writes one: a space every three digits. */
export function grouped(value: number): string {
  return value.toLocaleString("en-US").replace(/,/g, " ");
}

/** A value formatted by a registry format, or undefined when it is not a number the format fits. */
export function formatWith(format: Format, value: unknown, locale: string, timeZone: string): string | undefined {
  if (typeof value !== "number" && typeof value !== "string" && typeof value !== "bigint") return undefined;
  if (format.kind === "type") return undefined;   // one line stays as written; the block form is pretty (prettyType)
  if (format.kind === "size") {
    const bytes = Number(value);
    return Number.isFinite(bytes) ? formatSize(bytes) : undefined;
  }
  const ms = milliseconds(value, format.unit);
  if (ms === undefined) return undefined;
  return format.kind === "time" ? formatTime(ms, locale, timeZone) : formatDuration(ms);
}

/** Canonical ISO times stay exact; numeric display inputs use milliseconds. */
export function formatWire(primitive: string, value: unknown, locale: string, timeZone: string): string | undefined {
  if (["INSTANT", "DURATION", "INTERVAL"].includes(primitive) && typeof value === "string") return value;
  if (primitive === "INSTANT") {
    const ms = typeof value === "number" ? value : NaN;
    return Number.isFinite(ms) ? formatTime(ms, locale, timeZone) : undefined;
  }
  if (primitive === "DURATION" && typeof value === "number") return formatDuration(value);
  return undefined;
}

/**
 * A type expression written whole: `{ a: Text, b: List<{ c: Int }> }` becomes one field per line,
 * records opened by depth, the way `typeStructure` writes a wire type. Applied by the `type` format
 * only when the value is drawn as a block; on one line the expression stays as written.
 */
export function prettyType(text: string): string {
  const out: string[] = [];
  let depth = 0;
  let line = "";
  const flush = () => { if (line.trim() !== "") out.push("  ".repeat(depth) + line.trim()); line = ""; };
  for (const ch of text) {
    if (ch === "{") { line += "{"; flush(); depth += 1; continue; }
    if (ch === "}") { flush(); depth = Math.max(0, depth - 1); line = "}"; continue; }
    if (ch === "," && depth > 0) { flush(); continue; }
    line += ch;
  }
  flush();
  return out.join("\n");
}

/** Display only; never alter raw scale, copying or exact JSON serialization. */
export function compactDecimal(text: string): string {
  return text.replace(/(\.\d*?)0+(?=[eE]|$)/, "$1").replace(/\.(?=[eE]|$)/, "");
}
