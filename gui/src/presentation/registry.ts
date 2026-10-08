import { budget } from "../limits/policy";
/**
 * The type-name registry: portable presentation entries, validated as data.
 *
 * One entry says how values of one record type (or lists of them) are presented: which kind, which
 * columns, how a field is formatted, what else it offers. Entries are YAML data and never code. The
 * client ships its core entries (`core/*.yaml`); the data home's `presentations/` directory, served
 * read-only by the engine, overrides them by `type`.
 *
 * An entry matches on the type name and on its declared fields with their types:
 * a record called `HttpResponse` whose fields differ takes the structural rule. An invalid file —
 * bad YAML, unknown version, kind, field or formatter — is reported like an unknown kind, and the
 * last valid entry from the same file stays active. Nothing here changes a node's state, error,
 * privacy or authority, and an offer grants nothing.
 */
import { type Document, isAlias, isMap, isScalar, isSeq, parseDocument } from "yaml";
import { describeType, type TypeShape } from "../protocol";
import type { Kind, OfferName } from "./types";
import httpResponse from "./core/http-response.yaml?raw";
import processOutput from "./core/process-output.yaml?raw";

/** The kinds this client can draw from an entry. Anything else is an unknown kind. */
export const ENTRY_KINDS: readonly (Kind | "http")[] = ["table", "fields", "items", "text", "line", "process", "http"];
const OFFERS: readonly OfferName[] = ["http", "source", "copy", "trace"];
const UNITS = ["ns", "us", "ms", "s"] as const;
export type TimeUnit = (typeof UNITS)[number];

/** How one field is formatted. Units come only from here or from the wire type. */
export type Format =
  | { readonly kind: "time"; readonly unit: TimeUnit; readonly epoch: "unix" }
  | { readonly kind: "duration"; readonly unit: TimeUnit }
  | { readonly kind: "size"; readonly unit: "bytes" }
  /** A type expression; drawn whole it opens one field per line. */
  | { readonly kind: "type" };

export interface EntryOffer {
  readonly kind: OfferName;
}

/** A tone a declaration may name; `inherit` only as a field map's fallback. */
export type DeclaredTone = import("../value-meta").Tone;
export type CellStyle = "plain" | "badge";
/** A field's tones by exact value spelling, and what other values take. */
export interface FieldTones { readonly cases: Readonly<Record<string, DeclaredTone>>; readonly otherwise: DeclaredTone | "inherit" }
/** One bounded predicate over a row's own scalar field: equality or membership, nothing else. */
export interface Rule {
  readonly field: string;
  readonly values: readonly string[];
  readonly target: { readonly cell: string } | { readonly row: true };
  readonly tone?: DeclaredTone;
  readonly style?: CellStyle;
}

export interface Entry {
  /** Where it came from: `core`, `home:<file>`. */
  readonly origin: string;
  readonly type: string;
  readonly applies: readonly ("record" | "list")[];
  /** Declared fields, `name → type` as `describeType` writes it. */
  readonly fields: Readonly<Record<string, string>>;
  readonly kind: Kind | "http";
  readonly columns?: readonly string[];
  readonly formats: Readonly<Record<string, Format>>;
  readonly offers: readonly EntryOffer[];
  /** Version 2: tones by field value, cell styles and rules, applied over the type's own tones. */
  readonly tones?: Readonly<Record<string, FieldTones>>;
  readonly styles?: Readonly<Record<string, CellStyle>>;
  readonly rules?: readonly Rule[];
}

export interface EntryProblem {
  readonly origin: string;
  /** The type the file names, when it got far enough to name one. */
  readonly type?: string;
  readonly message: string;
}

/** Largest entry file accepted; an entry is a few lines. */
export function max_entry_bytes():number { return budget("ui.presentation.bytes"); }

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function names(value: unknown, what: string): string[] {
  if (!Array.isArray(value) || !value.every((item) => typeof item === "string" && item !== "")) {
    throw new Error(`${what} must be a list of names`);
  }
  return value as string[];
}

function format(name: string, value: unknown): Format {
  if (value === "size") return { kind: "size", unit: "bytes" };
  if (value === "type") return { kind: "type" };
  if (!isObject(value)) throw new Error(`format for ${name} must be size, type or {kind, unit}`);
  if (value.kind === "type") return { kind: "type" };
  const unit = value.unit;
  if (value.kind === "size") {
    if (unit !== undefined && unit !== "bytes") throw new Error(`size unit for ${name} must be bytes`);
    return { kind: "size", unit: "bytes" };
  }
  if (value.kind === "time" || value.kind === "duration") {
    if (!UNITS.includes(unit as TimeUnit)) throw new Error(`${value.kind} format for ${name} needs unit ns, us, ms or s`);
    if (value.kind === "time") {
      if (value.epoch !== "unix") throw new Error(`time format for ${name} needs epoch: unix`);
      return { kind: "time", unit: unit as TimeUnit, epoch: "unix" };
    }
    return { kind: "duration", unit: unit as TimeUnit };
  }
  throw new Error(`unknown formatter for ${name}: ${String(value.kind)}`);
}

/**
 * One entry from YAML text. Throws with a message a person can act on; the caller keeps the last
 * valid entry and reports the message.
 */
const MAX_RULES = 64, MAX_CASES = 64, MAX_ALIASES = 16;
const STYLES: readonly CellStyle[] = ["plain", "badge"];
const TONE_NAMES: readonly string[] = ["ok", "warn", "bad", "dim", "meta", "ink"];
/** A scalar as the exact spelling values are matched by; large numbers must be quoted. */
function spelling(value: unknown, what: string): string {
  if (typeof value === "string") return value;
  if (typeof value === "boolean") return String(value);
  if (typeof value === "number" && Number.isSafeInteger(value)) return String(value);
  throw new Error(`${what} must be text, a boolean or a safe integer; quote larger or decimal numbers`);
}
/**
 * Case keys and rule values as written. Parsed data has already re-spelled an unquoted number or
 * null (`1.50` → `1.5`, a large integer rounded, `~` → an empty key), so they are checked in the
 * source of the document the data came from. Every node on the way, keys included, is read through
 * its alias: an alias spells what its anchored node spells, and the walk has a fixed depth, so a
 * cyclic alias ends at a container where a scalar is required and is refused.
 */
function writtenSpellings(doc: Document, text: string): void {
  const deref = (node: unknown): unknown => isAlias(node) ? node.resolve(doc) : node;
  const key = (node: unknown): string => {
    const it = deref(node);
    if (!isScalar(it)) throw new Error("presentation keys must be names");
    return String(it.value);
  };
  // The value the data took for `name`: the last pair, as in the converted data.
  const child = (node: unknown, name: string): unknown => {
    const map = deref(node);
    if (!isMap(map)) return undefined;
    let found: unknown;
    for (const pair of map.items) if (key(pair.key) === name) found = pair.value ?? null;
    return deref(found);
  };
  const exact = (raw: unknown, what: string): string => {
    const node = deref(raw);
    if (isScalar(node) && (typeof node.value === "string" || typeof node.value === "boolean")) return String(node.value);
    const written = isScalar(node) && node.range ? text.slice(node.range[0], node.range[1]) : undefined;
    if (isScalar(node) && typeof node.value === "number" && Number.isSafeInteger(node.value) && written === String(node.value)) return written;
    throw new Error(`${what} must be text, a boolean or a safe integer; quote larger or decimal numbers`);
  };
  const present = child(doc.contents, "present");
  const tones = child(present, "tones");
  if (isMap(tones)) for (const field of tones.items) {
    const name = key(field.key), cases = child(field.value, "cases"), seen = new Set<string>();
    if (isMap(cases)) for (const pair of cases.items) {
      const spelled = exact(pair.key, `tone case for ${name}`);
      if (seen.has(spelled)) throw new Error(`tones for ${name} name ${spelled} twice`);
      seen.add(spelled);
    }
  }
  const ruled = child(present, "rules");
  if (isSeq(ruled)) ruled.items.forEach((rule, at) => {
    const when = child(rule, "when");
    const equals = child(when, "equals"), listed = child(when, "in");
    if (equals !== undefined) exact(equals, `rule ${at + 1} value`);
    if (isSeq(listed)) for (const item of listed.items) exact(item, `rule ${at + 1} value`);
  });
}
function tone(value: unknown, what: string, inherit = false): DeclaredTone | "inherit" {
  if (typeof value === "string" && (TONE_NAMES.includes(value) || (inherit && value === "inherit"))) return value as DeclaredTone | "inherit";
  throw new Error(`${what} must be one of ${[...TONE_NAMES, ...(inherit ? ["inherit"] : [])].join(", ")}`);
}
function only(value: Record<string, unknown>, keys: readonly string[], what: string) {
  for (const key of Object.keys(value)) if (!keys.includes(key)) throw new Error(`unknown ${what} key: ${key}`);
}
function fieldTones(value: unknown): Record<string, FieldTones> {
  if (!isObject(value)) throw new Error("tones must map a field name to its cases");
  const out: Record<string, FieldTones> = {};
  for (const [name, spec] of Object.entries(value)) {
    if (!isObject(spec) || !isObject(spec.cases)) throw new Error(`tones for ${name} need cases`);
    only(spec, ["cases", "otherwise"], "tones");
    const cases: Record<string, DeclaredTone> = Object.create(null);
    const entries = Object.entries(spec.cases);
    if (entries.length === 0 || entries.length > MAX_CASES) throw new Error(`tones for ${name} need 1 to ${MAX_CASES} cases`);
    for (const [key, case_] of entries) cases[key] = tone(case_, `tone for ${name} ${key}`) as DeclaredTone;
    out[name] = { cases, otherwise: spec.otherwise === undefined ? "inherit" : tone(spec.otherwise, `otherwise for ${name}`, true) };
  }
  return out;
}
function cellStyles(value: unknown): Record<string, CellStyle> {
  if (!isObject(value)) throw new Error("styles must map a field name to a style");
  const out: Record<string, CellStyle> = {};
  for (const [name, style] of Object.entries(value)) {
    if (!STYLES.includes(style as CellStyle)) throw new Error(`style for ${name} must be plain or badge`);
    out[name] = style as CellStyle;
  }
  return out;
}
function rules(value: unknown): Rule[] {
  if (!Array.isArray(value) || value.length > MAX_RULES) throw new Error(`rules must be a list of at most ${MAX_RULES}`);
  return value.map((raw, at) => {
    const what = `rule ${at + 1}`;
    if (!isObject(raw) || !isObject(raw.when) || !isObject(raw.target)) throw new Error(`${what} needs when and target`);
    only(raw, ["when", "target", "tone", "style"], "rule");
    only(raw.when, ["field", "equals", "in"], "when");
    if (typeof raw.when.field !== "string" || raw.when.field === "") throw new Error(`${what} must name a field`);
    if ((raw.when.equals === undefined) === (raw.when.in === undefined)) throw new Error(`${what} takes exactly one of equals or in`);
    const listed = raw.when.in === undefined ? [raw.when.equals] : raw.when.in;
    if (!Array.isArray(listed) || listed.length === 0 || listed.length > MAX_CASES) throw new Error(`${what}: in must list 1 to ${MAX_CASES} values`);
    const values = listed.map((item) => spelling(item, `${what} value`));
    only(raw.target, ["cell", "row"], "target");
    const row = raw.target.row === true && raw.target.cell === undefined;
    const cell = typeof raw.target.cell === "string" && raw.target.cell !== "" && raw.target.row === undefined;
    if (!row && !cell) throw new Error(`${what} targets either one cell or the row`);
    if (raw.tone === undefined && raw.style === undefined) throw new Error(`${what} must set a tone or a style`);
    if (row && raw.style !== undefined) throw new Error(`${what}: a row rule tints the row and takes no style`);
    if (raw.style !== undefined && !STYLES.includes(raw.style as CellStyle)) throw new Error(`${what}: style must be plain or badge`);
    return { field: raw.when.field, values, target: row ? { row: true as const } : { cell: raw.target.cell as string },
      ...(raw.tone !== undefined ? { tone: tone(raw.tone, `${what} tone`) as DeclaredTone } : {}),
      ...(raw.style !== undefined ? { style: raw.style as CellStyle } : {}) };
  });
}

export function parseEntry(text: string, origin: string): Entry {
  if (new TextEncoder().encode(text).length > max_entry_bytes()) throw new Error("entry exceeds its configured byte budget");
  // `yaml` builds plain data; no tags that construct objects or run code are enabled. The data and
  // the written spellings come from this one document, and converting it enforces the alias budget.
  const doc = parseDocument(text, { schema: "core", customTags: [] });
  if (doc.errors.length > 0) throw doc.errors[0];
  const data: unknown = doc.toJS({ maxAliasCount: MAX_ALIASES });
  if (!isObject(data)) throw new Error("entry must be a mapping");
  if (data.version !== 1 && data.version !== 2) throw new Error(`unsupported version: ${String(data.version)}`);
  if (typeof data.type !== "string" || data.type === "") throw new Error("type must name a record type, or * for a record matched by its fields");
  const applies = data.applies === undefined ? ["record", "list"] : names(data.applies, "applies");
  if (!applies.every((item) => item === "record" || item === "list")) throw new Error("applies takes record and list");
  const fields: Record<string, string> = {};
  if (data.fields !== undefined) {
    if (!isObject(data.fields)) throw new Error("fields must map a field name to its type");
    for (const [name, type] of Object.entries(data.fields)) {
      if (typeof type !== "string" || type === "") throw new Error(`field ${name} needs a type`);
      fields[name] = type;
    }
  }
  if (!isObject(data.present)) throw new Error("present must say how the value is presented");
  const kind = data.present.kind;
  if (typeof kind !== "string" || !ENTRY_KINDS.includes(kind as Kind | "http")) throw new Error(`unknown kind: ${String(kind)}`);
  const columns = data.present.columns === undefined ? undefined : names(data.present.columns, "columns");
  const formats: Record<string, Format> = {};
  if (data.present.formats !== undefined) {
    if (!isObject(data.present.formats)) throw new Error("formats must map a field name to a formatter");
    for (const [name, value] of Object.entries(data.present.formats)) formats[name] = format(name, value);
  }
  const offers: EntryOffer[] = [];
  if (data.present.offers !== undefined) {
    if (!Array.isArray(data.present.offers)) throw new Error("offers must be a list");
    for (const offer of data.present.offers) {
      if (!isObject(offer) || !OFFERS.includes(offer.kind as OfferName)) throw new Error(`unknown offer: ${JSON.stringify(offer)}`);
      offers.push({
        kind: offer.kind as OfferName,
      });
    }
  }
  const declares = (key: string) => (data.present as Record<string, unknown>)[key] !== undefined;
  if (data.version === 1 && (declares("tones") || declares("styles") || declares("rules"))) throw new Error("tones, styles and rules need version: 2");
  if (declares("tones") || declares("rules")) writtenSpellings(doc, text);
  const tones = declares("tones") ? fieldTones(data.present.tones) : undefined;
  const styles = declares("styles") ? cellStyles(data.present.styles) : undefined;
  const ruled = declares("rules") ? rules(data.present.rules) : undefined;
  if ((tones || styles || ruled) && kind !== "table") throw new Error("tones, styles and rules are for kind: table");
  // A structural entry has no name to match on, so its fields are the whole of its match.
  if (data.type === "*" && Object.keys(fields).length === 0) throw new Error("a * entry must declare the fields it matches");
  // A format or column naming a field the entry did not declare cannot be checked against the wire.
  const declared = Object.keys(fields);
  if (declared.length > 0) {
    const toned = [...Object.keys(tones ?? {}), ...Object.keys(styles ?? {}), ...(ruled ?? []).flatMap((rule) => ["cell" in rule.target ? rule.target.cell : rule.field, rule.field])];
    for (const name of [...(columns ?? []), ...Object.keys(formats), ...toned]) {
      if (!declared.includes(name)) throw new Error(`field ${name} is not declared in fields`);
    }
  }
  return { origin, type: data.type, applies: applies as ("record" | "list")[], fields, kind: kind as Kind | "http", ...(columns ? { columns } : {}), formats, offers, ...(tones ? { tones } : {}), ...(styles ? { styles } : {}), ...(ruled ? { rules: ruled } : {}) };
}

/** The type name an unparsable file claims, for its notice. Best effort, never trusted. */
function claimedType(text: string): string | undefined {
  return /^type:\s*["']?([\w.<>-]+)/m.exec(text)?.[1];
}

/** Whether a record type carries every declared field with the declared type. */
function fieldsMatch(entry: Entry, record: Extract<TypeShape, { kind: "record" }>): boolean {
  return Object.entries(entry.fields).every(([name, type]) => {
    const field = record.fields?.find((it) => it.name === name);
    return field !== undefined && describeType(field.type) === type;
  });
}

export interface HomeFile {
  readonly name: string;
  readonly text: string;
}

/**
 * Entries by type, in the order that wins: data home, then core.
 *
 * Immutable: every change makes a new registry, so a presentation computed from an old one never
 * sees half of a new directory.
 */
export class Registry {
  private constructor(
    private readonly core: ReadonlyMap<string, Entry>,
    private readonly home: ReadonlyMap<string, Entry>,
    /** The last valid entry of each data-home file, by file name. */
    private readonly homeFiles: ReadonlyMap<string, Entry>,
    readonly problems: readonly EntryProblem[],
  ) {}

  /** The entries shipped in the bundle. */
  static core(): Registry {
    const core = new Map<string, Entry>();
    for (const [name, text] of [["http-response.yaml", httpResponse], ["process-output.yaml", processOutput]] as const) {
      const entry = parseEntry(text, `core:${name}`);
      core.set(entry.type, entry);
    }
    return new Registry(core, new Map(), new Map(), []);
  }

  /**
   * The data home's directory as it is now. A file that no longer validates keeps its previous
   * valid entry and is reported; a file that is gone takes its entry with it.
   */
  withHome(files: readonly HomeFile[], unreadable: readonly { readonly name: string; readonly message: string }[] = []): Registry {
    const problems: EntryProblem[] = this.problems.filter((problem) => !problem.origin.startsWith("home:"));
    const byFile = new Map<string, Entry>();
    // A file the engine could not read (too large, not UTF-8) keeps its last valid entry.
    for (const file of unreadable) {
      const kept = this.homeFiles.get(file.name);
      problems.push({ origin: `home:${file.name}`, ...(kept ? { type: kept.type } : {}), message: file.message });
      if (kept) byFile.set(file.name, kept);
    }
    for (const file of files) {
      const origin = `home:${file.name}`;
      try {
        byFile.set(file.name, parseEntry(file.text, origin));
      } catch (error) {
        const claimed = claimedType(file.text);
        problems.push({ origin, ...(claimed ? { type: claimed } : {}), message: error instanceof Error ? error.message : String(error) });
        const kept = this.homeFiles.get(file.name);
        if (kept) byFile.set(file.name, kept);
      }
    }
    const home = new Map<string, Entry>();
    for (const entry of byFile.values()) home.set(entry.type, entry);
    return new Registry(this.core, home, byFile, problems);
  }

  /**
   * The entry for this value: one that names its record type and whose declared fields match the
   * wire type, or, for a record the wire does not name, a `*` entry whose declared fields the data
   * carries (a record with no declared type has only its keys to match on).
   */
  match(type: TypeShape | undefined, data?: unknown): { readonly entry: Entry; readonly list: boolean } | undefined {
    const list = type?.kind === "list";
    const record = list ? (type.element.kind === "record" ? type.element : undefined) : type?.kind === "record" ? type : undefined;
    if (record && record.name !== "") {
      for (const source of [this.home, this.core]) {
        const entry = source.get(record.name);
        if (!entry) continue;
        if (!entry.applies.includes(list ? "list" : "record")) return undefined;
        return fieldsMatch(entry, record) ? { entry, list } : undefined;
      }
      return undefined;
    }
    if (list || !isObject(data)) return undefined;
    for (const source of [this.home, this.core]) {
      const entry = source.get("*");
      if (!entry || !entry.applies.includes("record")) continue;
      const keys = Object.keys(entry.fields);
      if (keys.every((name) => name in data) && (record ? fieldsMatch(entry, record) : true)) return { entry, list: false };
    }
    return undefined;
  }

  /** What the tail says about a rejected entry for this type: `presentation x.yaml: unknown kind: y`. */
  noticesFor(type: TypeShape | undefined): readonly string[] {
    const record = type?.kind === "list" ? type.element : type;
    if (record?.kind !== "record") return [];
    return this.problems
      .filter((problem) => problem.type === record.name)
      .map((problem) => `presentation ${problem.origin.replace(/^home:/, "")}: ${problem.message}`);
  }
}
