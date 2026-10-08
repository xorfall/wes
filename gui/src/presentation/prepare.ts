import { budget } from "../limits/policy";
import { isExactNumber } from "../exact-json";
/**
 * Prepare, then present.
 *
 * Decoding is the costly, sometimes asynchronous half of showing a value: base64 Bytes, an HTTP
 * body's charset, gzip or deflate, binary detection and size bounds. It runs here, once per result
 * handle and workspace generation, and `present()` stays a pure function of what this returns. A
 * resize re-runs `present`, never this.
 *
 * Nothing stored is changed: a prepared value is a copy whose Bytes leaves are `DecodedBytes`
 * readings that still carry the stored base64, so copy and the JSON view can always reach the
 * original. A decoding failure keeps the stored Bytes and says why.
 */
import type { StoredValue, TypeShape } from "../protocol";

export interface Prepared {
  readonly viewModules?: readonly import("../value-views/contract").ValueViewModule[];
  readonly type: TypeShape;
  readonly data: unknown;
  readonly provenance: Readonly<Record<string, string>>;
  /** Contract metadata captured with the value; absent means unknown. */
  readonly meta?: import("../value-meta").ValueMeta;
  /** True while an asynchronous decoding has not landed; the tree says `decoding…` for it. */
  readonly pending: boolean;
}

export { DecodedBytes, byteSize, readBytes, decodeBytes } from "./bytes";
import { readBytes, DecodedBytes } from "./bytes";
import { valueViewModules, viewsOfValue } from "../value-views/registry";
function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value) && !isExactNumber(value);
}

function fieldType(type: TypeShape, name: string): TypeShape {
  return type.kind === "record" ? type.fields?.find((field) => field.name === name)?.type ?? { kind: "unknown" } : { kind: "unknown" };
}

function elementOf(type: TypeShape): TypeShape {
  return type.kind === "list" || type.kind === "iter" || type.kind === "option" ? type.element : { kind: "unknown" };
}

/**
 * A type shape as this client can trust it. Values are read off the wire with a cast, so a newer
 * engine or a malformed value can send a record without fields or fields that are not fields; the
 * presentation must still draw something rather than throw inside a render.
 */
export function cleanType(type: unknown, depth = 0): TypeShape {
  if (depth > 64 || typeof type !== "object" || type === null) return { kind: "unknown" };
  const shape = type as Record<string, unknown>;
  switch (shape.kind) {
    case "meta": return typeof shape.name === "string" ? { kind: "meta", name: shape.name } : { kind: "unknown" };
    case "primitive": return typeof shape.name === "string" ? { kind: "primitive", name: shape.name } : { kind: "unknown" };
    case "list": case "option": case "dataset": return { kind: shape.kind, element: cleanType(shape.element, depth + 1) };
    case "iter": {
      const element = cleanType(shape.element, depth + 1), contract = shape.contract;
      return nominal(contract) ? { kind: "iter", element, contract } : { kind: "iter", element };
    }
    case "record": return {
      kind: "record", name: typeof shape.name === "string" ? shape.name : "",
      fields: Array.isArray(shape.fields)
        ? shape.fields.filter((field): field is Record<string, unknown> => typeof field === "object" && field !== null && typeof (field as Record<string, unknown>).name === "string")
          .map((field) => ({ name: field.name as string, type: cleanType(field.type, depth + 1) }))
        : [],
    };
    default: return { kind: "unknown" };
  }
}

/** A nominal name is a non-blank string; anything else is no name at all. */
function nominal(name: unknown): name is string {
  return typeof name === "string" && name.trim() !== "";
}

/**
 * The one type every surface shows for a stored value: the cleaned structural shape, plus an
 * iterator's nominal item contract when its recipe data names one. Reading `itemContract` is a
 * property lookup — the recipe is never run or collected. A missing or malformed annotation
 * keeps whatever valid contract the type itself carried, else the structural element.
 */
export function presentationType(value: Pick<StoredValue, "type" | "data">): TypeShape {
  const structural = cleanType(value.type);
  const annotated = isObject(value.data) ? value.data.itemContract : undefined;
  if (structural.kind !== "iter" || !nominal(annotated)) return structural;
  return { ...structural, contract: annotated };
}

/** Upper bound on leaves visited, so a pathological value cannot stall the client. */
function max_visits():number { return budget("ui.presentation.visits"); }

function walk(type: TypeShape, data: unknown, budget: { visits: number }, depth: number): unknown {
  if (data instanceof DecodedBytes || isExactNumber(data)) return data;
  if (--budget.visits < 0 || depth > 64 || data === null || data === undefined) return data;
  // A dataset descriptor holds no records or Bytes to decode; it is presented exactly as received.
  if (type.kind === "dataset") return data;
  if (type.kind === "primitive" && type.name === "BYTES" && typeof data === "string") return readBytes(data);
  if (type.kind === "option" && isObject(data) && (data.kind === "some" || data.kind === "none")) {
    return data.kind === "some" ? { kind: "some", value: walk(type.element, data.value, budget, depth + 1) } : data;
  }
  if (Array.isArray(data)) return data.map((item) => walk(elementOf(type), item, budget, depth + 1));
  if (isObject(data)) {
    const out: Record<string, unknown> = {};
    for (const [name, item] of Object.entries(data)) out[name] = walk(fieldType(type, name), item, budget, depth + 1);
    return out;
  }
  return data;
}

/** Everything that can be decoded without waiting. Asynchronous bodies are marked pending. */
export function prepareSync(value: Pick<StoredValue, "type" | "data"> & { readonly provenance?: Record<string, string>; readonly meta?: StoredValue["meta"] }): Prepared {
  const type = presentationType(value);
  const viewModules = viewsOfValue(value);
  const module = valueViewModules.find(type, value.data, undefined, viewModules);
  let reading = { data: value.data, pending: false };
  try { reading = module?.prepare?.({ ...value, type }, value.data) ?? reading; } catch { /* generic fallback */ }
  return { ...(viewModules ? {viewModules} : {}), type, data: walk(type, reading.data, { visits: max_visits() }, 0), provenance: value.provenance ?? {}, ...(value.meta ? { meta: value.meta } : {}), pending: reading.pending };
}

function fallback(value: Pick<StoredValue, "type" | "data">, prepared: Prepared): Prepared {
  return { ...prepared, data: walk(prepared.type, value.data, { visits: max_visits() }, 0), pending: false };
}

/** The whole preparation, awaiting compressed HTTP bodies. */
export async function prepare(value: Pick<StoredValue, "type" | "data"> & { readonly provenance?: Record<string, string> }, signal?: AbortSignal): Promise<Prepared> {
  const prepared = prepareSync(value);
  if (!prepared.pending) return prepared;
  const module = valueViewModules.find(prepared.type, value.data, undefined, prepared.viewModules);
  if (!module?.prepareAsync) return fallback(value, prepared);
  try { return { ...prepared, ...await module.prepareAsync({ ...value, type: prepared.type }, prepared.data, signal) }; }
  catch (error) { if (signal?.aborted) throw error; return fallback(value, prepared); }
}

/**
 * One preparation per result handle and workspace generation.
 *
 * `read` answers at once with what can be decoded synchronously and, when something was pending,
 * starts the asynchronous half exactly once and calls `onReady` when it lands. A new generation is a
 * new key, so an old run's decoding never lands on a new run's value.
 */
interface CacheEntry {
  prepared: Prepared;
  modules: ReturnType<typeof valueViewModules.get>;
  listeners: Set<() => void>;
}
export class PreparedCache {
  // Keep bounded identity metadata only. Prepared payloads live exactly as long as their
  // observed StoredValue; the renderer must not retain hundreds of old stream windows.
  private readonly entries = new Set<string>();
  private readonly byValue = new WeakMap<StoredValue, CacheEntry>();
  constructor(private readonly limit = 512) {}

  /** Drops a preparation, so nothing of a withdrawn value stays held for a later draw. */
  forget(key: string, value: StoredValue): void { this.entries.delete(key); this.byValue.delete(value); }
  read(key: string, value: StoredValue, onReady?: () => void): Prepared {
    const modules = valueViewModules.get();
    let entry = this.byValue.get(value);
    if (!entry || entry.modules !== modules) {
      entry = { prepared: prepareSync(value), modules, listeners: new Set() };
      this.byValue.set(value, entry);
      if (entry.prepared.pending) {
        const active = entry;
        const module = valueViewModules.find(active.prepared.type, value.data, undefined, active.prepared.viewModules);
        void Promise.resolve().then(() => module?.prepareAsync?.(value, active.prepared.data) ?? fallback(value, active.prepared)).then(result => {
          active.prepared = { ...active.prepared, ...result };
          active.listeners.forEach(fn => fn()); active.listeners.clear();
        }, () => {
          active.prepared = fallback(value, active.prepared);
          active.listeners.forEach(fn => fn()); active.listeners.clear();
        });
      }
    }
    if (onReady && entry.prepared.pending) entry.listeners.add(onReady);
    this.entries.add(key);
    if (this.entries.size > this.limit) this.entries.delete(this.entries.keys().next().value!);
    return entry.prepared;
  }
  get size(): number { return this.entries.size; }
}
