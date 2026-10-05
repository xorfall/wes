import { isExactNumber } from "../exact-json";
/**
 * A display-only JSON projection of a stored value: Bytes decoded where they are text, iterators
 * said as recipes. The stored value is never changed and nothing is guessed from a string's shape —
 * only the wire type decides that Bytes are Bytes. The window's JSON tab and copy use this.
 */
import { describeType, type TypeShape } from "../protocol";
import { byteSize, decodeBytes } from "./prepare";

function isObject(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value) && !isExactNumber(value);
}

function element(type: TypeShape): TypeShape {
  return type.kind === "list" ? type.element : { kind: "unknown" };
}

function typeOfField(type: TypeShape, name: string): TypeShape {
  return type.kind === "record" ? type.fields?.find((field) => field.name === name)?.type ?? { kind: "unknown" } : { kind: "unknown" };
}

export function readableData(type: TypeShape, data: unknown, depth = 0): unknown {
  if (isExactNumber(data)) return data;
  if (depth > 64 || data === null || data === undefined) return data;
  if (type.kind === "iter") return { type: describeType(type), mode: isObject(data) ? data.mode : undefined, note: "Explicit collection required" };
  if (type.kind === "primitive" && type.name === "BYTES" && typeof data === "string") {
    return decodeBytes(data) ?? { bytes: byteSize(data), note: "Binary data; download original data to preserve the bytes" };
  }
  if (type.kind === "option" && isObject(data) && data.kind === "some") return { ...data, value: readableData(type.element, data.value, depth + 1) };
  if (Array.isArray(data)) return data.map((item) => readableData(element(type), item, depth + 1));
  if (isObject(data)) return Object.fromEntries(Object.entries(data).map(([name, value]) => [name, readableData(typeOfField(type, name), value, depth + 1)]));
  return data;
}
