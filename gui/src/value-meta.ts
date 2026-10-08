import { compareNumeric, isNumeric, numericText, type NumericValue } from "./exact-json";

/**
 * Contract metadata that travels beside a value: the aliases and declared enum members of the
 * contract that validated it, keyed by declaration path. It never changes the value or its shape.
 * Missing metadata means unknown, never unconstrained.
 */
export type Tone = "ok" | "warn" | "bad" | "dim" | "meta" | "ink";
export const TONES: readonly Tone[] = ["ok", "warn", "bad", "dim", "meta", "ink"];
export interface ContractIdentity { readonly name: string; readonly digest: string }
export interface FieldMeta {
  readonly contract: ContractIdentity;
  readonly kind: "text" | "int" | "decimal" | "bool";
  readonly source?: "declared" | "validated";
  readonly members?: readonly string[];
  readonly total?: number;
  readonly complete?: boolean;
  readonly tones?: Readonly<Record<string, Tone>>;
}
export interface ValueMeta {
  readonly version: 1;
  readonly contract: ContractIdentity;
  readonly truncated: boolean;
  readonly fields: Readonly<Record<string, FieldMeta>>;
}

/** Bounds mirror the engine's: a larger or deeper description is refused as a whole. */
const MAX_DESCRIPTORS = 128, MAX_MEMBERS = 64, MAX_DEPTH = 64, MAX_BYTES = 64 * 1024;
const KINDS = new Set(["text", "int", "decimal", "bool"]);
const SEGMENT = /^(?:\/e|\/o|\/f:(?:[^~/]|~[01])*)$/;

/** Declaration path segments: a record field, a list element (any index), an option's content. */
export const field = (name: string) => `/f:${name.replace(/~/g, "~0").replace(/\//g, "~1")}`;
export const ELEMENT = "/e", OPTION = "/o";

function segments(path: string): string[] | undefined {
  if (path === "") return [];
  const parts = path.match(/\/(?:e|o|f:(?:[^~/]|~[01])*)/g);
  return parts && parts.join("") === path && parts.every(p => SEGMENT.test(p)) ? parts : undefined;
}
const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
function identity(v: unknown): ContractIdentity | undefined {
  return isObject(v) && typeof v.name === "string" && v.name.length > 0 && v.name.length <= 1024 && typeof v.digest === "string" && /^sha256:[0-9a-f]{64}$/.test(v.digest)
    ? { name: v.name, digest: v.digest } : undefined;
}
function descriptor(v: unknown): FieldMeta | undefined {
  if (!isObject(v)) return undefined;
  const contract = identity(v.contract);
  if (!contract || typeof v.kind !== "string" || !KINDS.has(v.kind)) return undefined;
  if (v.source !== undefined && v.source !== "declared" && v.source !== "validated") return undefined;
  const result: { -readonly [K in keyof FieldMeta]: FieldMeta[K] } = { contract, kind: v.kind as FieldMeta["kind"], ...(v.source ? { source: v.source as "declared" | "validated" } : {}) };
  if (v.members === undefined) return v.total === undefined && v.complete === undefined && v.tones === undefined ? result : undefined;
  if (!Array.isArray(v.members) || v.members.length > MAX_MEMBERS || !v.members.every(m => typeof m === "string")) return undefined;
  if (new Set(v.members).size !== v.members.length) return undefined;
  if (typeof v.total !== "number" || !Number.isSafeInteger(v.total) || v.total < v.members.length || typeof v.complete !== "boolean") return undefined;
  if (v.complete !== (v.total === v.members.length)) return undefined;
  result.members = v.members as string[]; result.total = v.total; result.complete = v.complete;
  if (v.tones !== undefined) {
    if (!isObject(v.tones) || Object.keys(v.tones).length > MAX_MEMBERS) return undefined;
    for (const tone of Object.values(v.tones)) if (!TONES.includes(tone as Tone)) return undefined;
    result.tones = v.tones as Record<string, Tone>;
  }
  return result;
}

/** Decode metadata from the wire, or undefined when it is absent or does not validate. */
export function decodeMeta(raw: unknown): ValueMeta | undefined {
  if (!isObject(raw) || raw.version !== 1 || typeof raw.truncated !== "boolean" || !isObject(raw.fields)) return undefined;
  const contract = identity(raw.contract);
  if (!contract) return undefined;
  try { if (new TextEncoder().encode(JSON.stringify(raw)).length > MAX_BYTES) return undefined; } catch { return undefined; }
  const entries = Object.entries(raw.fields);
  if (entries.length > MAX_DESCRIPTORS) return undefined;
  const fields: Record<string, FieldMeta> = {};
  for (const [path, value] of entries) {
    const parts = segments(path), d = descriptor(value);
    if (!parts || parts.length > MAX_DEPTH || !d) return undefined;
    fields[path] = d;
  }
  return { version: 1, contract, truncated: raw.truncated, fields };
}

/** A scalar's exact spelling, as declarations match it; undefined for anything else. */
export function scalarSpelling(value: unknown): string | undefined {
  return typeof value === "string" ? value : typeof value === "boolean" ? String(value) : isNumeric(value) ? numericText(value) : undefined;
}

/**
 * The declared tone of a scalar, matched by its exact spelling; undefined when none is declared.
 * A Decimal member matches as the contract validates it: by exact numeric value, so `1.50` takes
 * the tone of `1.5`. Nothing passes through a float. The reader may already have reduced `1.50`
 * to `1.5`, so a spelling cannot tell equal members apart: when equal members declare different
 * tones, or one cannot be compared, the value is drawn neutral rather than by a guessed member.
 */
export function declaredTone(meta: FieldMeta | undefined, value: unknown): Tone | undefined {
  if (!meta?.tones) return undefined;
  if (meta.kind === "decimal") return isNumeric(value) ? numericTone(meta.tones, value) : undefined;
  const spelling = scalarSpelling(value);
  return spelling !== undefined && Object.hasOwn(meta.tones, spelling) ? meta.tones[spelling] : undefined;
}

/** The one tone every numerically equal key declares; undefined when none, several, or unknown. */
function numericTone(tones: Readonly<Record<string, Tone>>, value: NumericValue): Tone | undefined {
  let found: Tone | undefined;
  for (const [key, tone] of Object.entries(tones)) {
    let equal: boolean;
    try { equal = compareNumeric(value, key) === 0; } catch { return undefined; }
    if (!equal) continue;
    if (found !== undefined && found !== tone) return undefined;
    found = tone;
  }
  return found;
}

/** The descriptor declared at a path, read only from the metadata's own keys. */
export function fieldMeta(meta: ValueMeta | undefined, path: string): FieldMeta | undefined {
  return meta && Object.hasOwn(meta.fields, path) ? meta.fields[path] : undefined;
}

/** Characters one member takes in a label before it is cut. */
const MEMBER_ROOM = 32;
/** A member on one line: quoted when it has spaces, separators or control characters, cut when long. */
function memberText(member: string): string {
  const written = /^[^\s|"\\…\p{C}]+$/u.test(member) ? member : JSON.stringify(member);
  const chars = [...written];
  return chars.length > MEMBER_ROOM ? `${chars.slice(0, MEMBER_ROOM - 1).join("")}…` : written;
}

/** "a | b | c", or the first members and how many were declared in all. */
export function membersLabel(meta: FieldMeta, room = 6): string | undefined {
  if (!meta.members) return undefined;
  const shown = meta.members.slice(0, room).map(memberText), more = meta.total! - shown.length;
  return more > 0 ? `${shown.join(" | ")} | … (${shown.length} of ${meta.total} declared)` : shown.join(" | ");
}

/** A value read from the engine with its metadata decoded, or dropped when it does not validate. */
export function withValidMeta<T extends { readonly meta?: unknown }>(value: T): T {
  if (value === null || typeof value !== "object" || !("meta" in value)) return value;
  const { meta, ...rest } = value as T & { meta?: unknown };
  const decoded = decodeMeta(meta);
  return (decoded ? { ...rest, meta: decoded } : rest) as T;
}

/**
 * The declaration path of a value inside a typed root, from its data JSON Pointer: list indices
 * become `/e`, record keys `/f:<key>`, and every Option layer `/o`. Undefined when the pointer
 * leaves the declared structure.
 */
export function declarationPath(root: import("./protocol").TypeShape, pointer: string): string | undefined {
  if (pointer !== "" && !pointer.startsWith("/")) return undefined;
  const keys = pointer === "" ? [] : pointer.slice(1).split("/").map(key => key.replace(/~1/g, "/").replace(/~0/g, "~"));
  let type = root, out = "";
  const options = () => { while (type.kind === "option") { out += OPTION; type = type.element; } };
  for (const key of keys) {
    options();
    if (type.kind === "list") { out += ELEMENT; type = type.element; }
    else if (type.kind === "record") {
      const declared = type.fields.find(it => it.name === key);
      if (!declared) return undefined;
      out += field(key); type = declared.type;
    } else return undefined;
  }
  options();
  return out;
}

/** A declaration path continued into a value's Option layers, where its scalar is described. */
export function throughOptions(path: string, type: import("./protocol").TypeShape): string {
  let out = path;
  for (let at = type, depth = 0; at.kind === "option" && depth < 64; at = at.element, depth++) out += OPTION;
  return out;
}
