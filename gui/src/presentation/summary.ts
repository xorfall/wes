/**
 * What the verdict may say about a value, without preparing or presenting it.
 *
 * The verdict's facts slot carries what the result actually was — `exit 1`, `404`, `213` — and the
 * verdict is computed for every cell on every render, so this reads only the few fields it needs and
 * never decodes Bytes or walks rows. It agrees with `present()`'s summary by construction: the same
 * registry decides whether a record is a process or an HTTP response.
 */
import { matchesHttp } from "../views/http-response";
import type { TypeShape } from "../protocol";
import { grouped } from "./format";
import { cleanType } from "./prepare";
import type { Registry } from "./registry";
import { registryStore } from "./registry-store";
import type { Run } from "./types";

function isObject(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function summarize(type: TypeShape | undefined, data: unknown, registry: Registry = registryStore.get()): readonly Run[] {
  if (type === undefined || data === undefined) return [];
  let shape: TypeShape = cleanType(type);
  let value: unknown = data;
  while (shape.kind === "option" && isObject(value) && value.kind === "some") {
    value = value.value;
    shape = shape.element;
  }
  if (matchesHttp(shape, value)) {
    const status = (value as { status: number }).status;
    return [{ text: String(status), tone: status < 400 ? "ok" : "warn" }];
  }
  const matched = registry.match(shape);
  if (matched && !matched.list && isObject(value)) {
    if (matched.entry.kind === "process" && typeof value.exitCode === "number") {
      return [{ text: `exit ${value.exitCode}`, tone: value.exitCode === 0 ? "ok" : "warn" }];
    }
    if (matched.entry.kind === "http" && typeof value.status === "number") {
      return [{ text: String(value.status), tone: value.status < 400 ? "ok" : "warn" }];
    }
  }
  if (Array.isArray(value)) return [{ text: grouped(value.length), tone: "dim" }];
  return [];
}
