/** JSON numbers stay numeric on the wire. Preserve lexemes that JS cannot round-trip. */
import { parse, isSafeNumber, isNumber } from "lossless-json";

export class ExactNumber {
  readonly text: string;

  constructor(text: string) {
    if (text.length > 8 * 1024 * 1024 || !isNumber(text)) {
      throw new Error("Invalid exact numeric lexeme");
    }
    this.text = text;
    Object.freeze(this);
  }

  toString(): string { return this.text; }
}

export type NumericValue = number | ExactNumber;
export const isExactNumber = (value: unknown): value is ExactNumber => value instanceof ExactNumber;
export const isNumeric = (value: unknown): value is NumericValue =>
  isExactNumber(value) || typeof value === "number" && Number.isFinite(value);

export function numericText(value: NumericValue): string {
  return isExactNumber(value) ? value.text : String(value);
}

/** Compare compact decimal values without expanding exponent-sized runs of zeroes. */
export function compareNumeric(value: NumericValue, other: string): number {
  const normalized = (text: string) => {
    const parts = /^(-?)(\d+)(?:\.(\d+))?(?:[eE]([+-]?\d+))?$/.exec(text);
    if (!parts || text.length > 8 * 1024 * 1024 || (parts[4]?.length ?? 0) > 4096) {
      throw new Error("Numeric comparison exceeds its boundary budget");
    }
    const coefficient = parts[2]! + (parts[3] ?? "");
    const leading = coefficient.match(/^0*/)?.[0].length ?? 0;
    const digits = coefficient.slice(leading).replace(/0*$/, "");
    const sign = digits.length === 0 ? 0 : parts[1] === "-" ? -1 : 1;
    const exponent = BigInt(parts[4] ?? 0) + BigInt(parts[2]!.length - leading - 1);
    return { sign, digits, exponent };
  };
  const left = normalized(numericText(value)), right = normalized(other);
  if (left.sign !== right.sign) return left.sign < right.sign ? -1 : 1;
  if (!left.sign) return 0;
  const magnitude = left.exponent < right.exponent ? -1 : left.exponent > right.exponent ? 1
    : left.digits < right.digits ? -1 : left.digits > right.digits ? 1 : 0;
  return left.sign * magnitude;
}

/** Drawing/mapping boundary refuses information loss instead of rounding stored data. */
export function drawingNumber(value: unknown): number | undefined {
  if (typeof value === "number") return Number.isFinite(value) ? value : undefined;
  if (isExactNumber(value) && isSafeNumber(value.text)) return Number(value.text);
  return undefined;
}

export function drawingTextNumber(value: string): number | undefined {
  const text = value.trim();
  return isNumber(text) && isSafeNumber(text) ? Number(text) : undefined;
}

export function parseExactJson(text: string): unknown {
  return parse(text, undefined, {
    parseNumber: lexeme => isSafeNumber(lexeme) ? Number(lexeme) : new ExactNumber(lexeme),
  });
}

export async function readExactJson<T = unknown>(response: Response): Promise<T> {
  return parseExactJson(await response.text()) as T;
}

/** Exact scalars serialize without tags, quoted numbers, or magic record keys. */
export function stringifyExactJson(value: unknown, space = 0): string {
  let visits = 500_000, bytes = 32 * 1024 * 1024;
  const ancestors = new Set<object>();
  const unit = " ".repeat(Math.min(10, Math.max(0, space)));
  const emit = (text: string): string => {
    bytes -= text.length * 2;
    if (bytes < 0) throw new Error("JSON display exceeds its byte budget");
    return text;
  };
  const encode = (value: unknown, depth: number): string | undefined => {
    if (--visits < 0 || depth > 128) throw new Error("JSON display exceeds its nesting/item budget");
    if (isExactNumber(value)) return emit(value.text);
    if (value === null || typeof value !== "object") {
      const text = JSON.stringify(value);
      return text === undefined ? undefined : emit(text);
    }
    if (ancestors.has(value)) throw new Error("Circular JSON value");
    ancestors.add(value);
    const entries: string[] = [], array = Array.isArray(value);
    if (array) {
      for (const item of value) entries.push(encode(item, depth + 1) ?? emit("null"));
    } else {
      for (const [key, item] of Object.entries(value)) {
        const text = encode(item, depth + 1);
        if (text !== undefined) entries.push(emit(JSON.stringify(key) + (unit ? ": " : ":")) + text);
      }
    }
    ancestors.delete(value);
    const open = array ? "[" : "{", close = array ? "]" : "}";
    if (!entries.length) return emit(open + close);
    const indent = unit.repeat(depth + 1), separator = unit ? ",\n" + indent : ",";
    bytes -= separator.length * 2 * Math.max(0, entries.length - 1);
    if (bytes < 0) throw new Error("JSON display exceeds its byte budget");
    return emit(open + (unit ? "\n" + indent : "")) + entries.join(separator)
      + emit((unit ? "\n" + unit.repeat(depth) : "") + close);
  };
  return encode(value, 0) ?? "null";
}
