/**
 * Editable API drafts: the private, possibly incomplete text `:describe` retains when it cannot yet
 * publish an executable descriptor. The backend owns every judgement — syntax, semantics, the
 * readiness gate and the evidence status. This module only types the contract, proves that a
 * validation answer belongs to exactly the text it was asked about, and reads the text's shape
 * tolerantly enough to navigate and complete a document that may not parse.
 */
import { libraryRequest, readSchemaProvenance, type Descriptor, type PackageKey, type SchemaProvenance } from "./api-library";

export interface DraftSummary { key: PackageKey; revision: string; accepted: boolean; origin: string; sourceDigest: string | null; valid: boolean; descriptorRevision: string | null }
export type DraftSeverity = "error" | "warning";
export interface DraftDiagnostic { severity: DraftSeverity; code: string; target: string; message: string; fix: string; from: number; to: number; line: number }
export interface DraftValidation { hash: string; valid: boolean; diagnostics: DraftDiagnostic[]; preview: unknown | null }
export type EvidenceStatus = "current" | "stale";
export interface DraftEvidence { source: unknown; status: EvidenceStatus; manualTargets: string[] }
export interface DraftResult { draft: DraftSummary; text: string; validation: DraftValidation; evidence: DraftEvidence; descriptorPath?: string; descriptor?: Descriptor }

export const draftApi = {
  list: (signal?: AbortSignal) => libraryRequest<{ drafts: DraftSummary[] }>({ action: "listDrafts" }, signal),
  inspect: (key: PackageKey, revision: string) => libraryRequest<DraftResult>({ action: "inspectDraft", key, revision }),
  validate: (text: string) => libraryRequest<DraftValidation>({ action: "validateDraft", text }),
  save: (key: PackageKey, revision: string, text: string) => libraryRequest<DraftResult>({ action: "saveDraft", key, revision, text }),
  review: (key: PackageKey, revision: string) => libraryRequest<DraftResult>({ action: "reviewDraft", key, revision }),
  export: (key: PackageKey, revision: string, file: string) => libraryRequest<{ exportedPath: string }>({ action: "exportDraft", key, revision, file }),
};

export function sameKey(a: PackageKey, b: PackageKey): boolean {
  return a.service === b.service && a.apiVersion === b.apiVersion && a.scope === b.scope;
}

/* ---------- exact-text identity ---------- */

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
  0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
  0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
  0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

/**
 * SHA-256 of the UTF-8 bytes of `text`, lowercase hex. Synchronous and dependency-free on purpose:
 * `crypto.subtle` is missing outside secure contexts, and the gate must not depend on where the
 * page happens to be served from.
 */
export function sha256Hex(text: string): string {
  const bytes = new TextEncoder().encode(text);
  const length = ((bytes.length + 9 + 63) >> 6) << 6;
  const padded = new Uint8Array(length);
  padded.set(bytes);
  padded[bytes.length] = 0x80;
  const view = new DataView(padded.buffer);
  const bits = bytes.length * 8;
  view.setUint32(length - 8, Math.floor(bits / 0x100000000));
  view.setUint32(length - 4, bits >>> 0);
  const h = new Uint32Array([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]);
  const w = new Uint32Array(64);
  const rotr = (x: number, n: number) => (x >>> n) | (x << (32 - n));
  for (let block = 0; block < length; block += 64) {
    for (let i = 0; i < 16; i++) w[i] = view.getUint32(block + i * 4);
    for (let i = 16; i < 64; i++) {
      const a = w[i - 15]!, b = w[i - 2]!;
      w[i] = (w[i - 16]! + (rotr(a, 7) ^ rotr(a, 18) ^ (a >>> 3)) + w[i - 7]! + (rotr(b, 17) ^ rotr(b, 19) ^ (b >>> 10))) >>> 0;
    }
    let [a, b, c, d, e, f, g, hh] = [h[0]!, h[1]!, h[2]!, h[3]!, h[4]!, h[5]!, h[6]!, h[7]!];
    for (let i = 0; i < 64; i++) {
      const t1 = (hh + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + K[i]! + w[i]!) >>> 0;
      const t2 = ((rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c))) >>> 0;
      hh = g; g = f; f = e; e = (d + t1) >>> 0; d = c; c = b; b = a; a = (t1 + t2) >>> 0;
    }
    h[0] = (h[0]! + a) >>> 0; h[1] = (h[1]! + b) >>> 0; h[2] = (h[2]! + c) >>> 0; h[3] = (h[3]! + d) >>> 0;
    h[4] = (h[4]! + e) >>> 0; h[5] = (h[5]! + f) >>> 0; h[6] = (h[6]! + g) >>> 0; h[7] = (h[7]! + hh) >>> 0;
  }
  return Array.from(h, word => word.toString(16).padStart(8, "0")).join("");
}

const HASH_PREFIX = "sha256:";
/** A validation answer speaks for `text` only when its hash is the hash of exactly those bytes. */
export function validationFor(text: string, validation: DraftValidation | undefined): boolean {
  if (!validation || typeof validation.hash !== "string") return false;
  const hash = validation.hash.toLowerCase();
  return (hash.startsWith(HASH_PREFIX) ? hash.slice(HASH_PREFIX.length) : hash) === sha256Hex(text);
}

/* ---------- tolerant JSON shape ---------- */

export type JsonPath = (string | number)[];
type Frame = { kind: "object"; key: string | null; colon: boolean; valueStart: number } | { kind: "array"; index: number; valueStart: number };
export interface JsonCursor { path: JsonPath; position: "key" | "value" | "none"; inString: boolean }

/**
 * Walks `text` up to `end` the way a JSON reader would, but never fails: a malformed document
 * simply yields the containers still open at `end`. `onValue` hears where every value begins.
 */
function walk(text: string, end: number, onValue?: (path: JsonPath, at: number) => boolean): { stack: Frame[]; inString: boolean } {
  const stack: Frame[] = [];
  const pathOf = (): JsonPath => stack.flatMap((frame): (string | number)[] => frame.kind === "array" ? [frame.index] : frame.key === null ? [] : [frame.key]);
  const valueBegins = (at: number): boolean => {
    const top = stack[stack.length - 1];
    if (top?.kind === "object" && !top.colon) return false;
    return onValue ? onValue(pathOf(), at) : false;
  };
  if (onValue && onValue([], firstNonSpace(text))) return { stack, inString: false };
  let i = 0;
  while (i < end) {
    const ch = text[i]!;
    if (ch === '"') {
      const start = i;
      i++;
      let value = "";
      while (i < text.length && text[i] !== '"' && text[i] !== "\n") {
        if (text[i] === "\\" && i + 1 < text.length) { value += text[i + 1]; i += 2; } else { value += text[i]; i++; }
      }
      // The caret sits inside this string (up to and including its closing quote).
      if (end <= i) return { stack, inString: true };
      if (text[i] === '"') i++;
      const top = stack[stack.length - 1];
      if (top?.kind === "object" && !top.colon) top.key = value;
      else if (valueBegins(start)) return { stack, inString: false };
      continue;
    }
    if (ch === "{" || ch === "[") {
      if (stack.length && valueBegins(i)) return { stack, inString: false };
      stack.push(ch === "{" ? { kind: "object", key: null, colon: false, valueStart: i } : { kind: "array", index: 0, valueStart: i });
    } else if (ch === "}" || ch === "]") {
      stack.pop();
    } else if (ch === ":") {
      const top = stack[stack.length - 1];
      if (top?.kind === "object") top.colon = true;
    } else if (ch === ",") {
      const top = stack[stack.length - 1];
      if (top?.kind === "object") { top.key = null; top.colon = false; } else if (top) top.index++;
    } else if (!/\s/.test(ch)) {
      // a bare literal: true, false, null or a number
      const top = stack[stack.length - 1];
      const literalStart = i;
      while (i < text.length && !/[\s,:{}[\]"]/.test(text[i]!)) i++;
      if (top && valueBegins(literalStart)) return { stack, inString: false };
      if (i >= end) break;
      continue;
    }
    i++;
  }
  return { stack, inString: false };
}
function firstNonSpace(text: string): number { const m = /\S/.exec(text); return m ? m.index : 0; }

/** Where the caret stands: which container, and whether a key or a value goes here. */
export function cursorAt(text: string, offset: number): JsonCursor {
  const { stack, inString } = walk(text, offset);
  const top = stack[stack.length - 1];
  const path = stack.flatMap((frame): (string | number)[] => frame.kind === "array" ? [frame.index] : frame.colon && frame.key !== null ? [frame.key] : []);
  if (!top) return { path, position: "none", inString };
  if (top.kind === "object") return { path, position: top.colon ? "value" : "key", inString };
  return { path, position: "value", inString };
}

export function pointerOf(path: JsonPath): string {
  return path.length === 0 ? "#" : `#/${path.map(p => String(p).replace(/~/g, "~0").replace(/\//g, "~1")).join("/")}`;
}
function tokensOf(pointer: string): string[] {
  if (pointer === "#" || pointer === "#/") return [];
  return pointer.replace(/^#\//, "").split("/").map(t => t.replace(/~1/g, "/").replace(/~0/g, "~"));
}

/**
 * The offset where the value at `pointer` begins, or where its nearest existing ancestor begins
 * when the pointer names something the text does not have yet.
 */
export function offsetOfPointer(text: string, pointer: string): number | undefined {
  const want = tokensOf(pointer);
  let best: { depth: number; at: number } | undefined;
  walk(text, text.length, (path, at) => {
    if (path.length > want.length || !path.every((p, i) => String(p) === want[i])) return false;
    if (!best || path.length > best.depth) best = { depth: path.length, at };
    return path.length === want.length;
  });
  return best?.at;
}

/** The caret's pointer, trimmed to the depth where evidence is recorded: a field, a parameter, a response. */
export function evidencePointerAt(text: string, offset: number): string | undefined {
  const { path } = cursorAt(text, offset);
  if (path[0] === "operations" && typeof path[1] === "number") return pointerOf(path.slice(0, typeof path[3] === "number" ? 4 : 3));
  if (path[0] === "types" && typeof path[1] === "string") return pointerOf(path.slice(0, path[2] === "fields" && typeof path[3] === "string" ? 4 : 2));
  if (path[0] === "problems" && typeof path[1] === "number") return pointerOf(path.slice(0, 2));
  return undefined;
}

/* ---------- the parsed preview, read defensively ---------- */

export interface PreviewParameter { name: string; location: string; type: string; required?: boolean; description?: string }
export interface PreviewResponse { status: number | null; mediaType: string | null; type: string | null | undefined }
export interface PreviewOperation { index: number; name: string; summary?: string; description?: string; responseDescriptions?: Record<string, string>; method: string; route: string; auth: { scheme: string; secret?: string }[]; parameters: PreviewParameter[]; responses: PreviewResponse[] }
export interface DraftPreview { provider: string; operations: PreviewOperation[]; types: Record<string, unknown>; problems: { target: string; message: string }[] }

function isRecord(value: unknown): value is Record<string, unknown> { return !!value && typeof value === "object" && !Array.isArray(value); }
const str = (value: unknown, fallback = ""): string => typeof value === "string" ? value : fallback;
const list = (value: unknown): unknown[] => Array.isArray(value) ? value : [];

/**
 * The preview as screens show it. Any part with the wrong shape reads as absent rather than
 * throwing, so a preview of a half-typed draft can never break the screen.
 */
export function readPreview(preview: unknown): DraftPreview | undefined {
  if (!isRecord(preview)) return undefined;
  const operations = list(preview.operations).map((raw, index): PreviewOperation => {
    const op = isRecord(raw) ? raw : {};
    return {
      index,
      name: list(op.path).filter((p): p is string => typeof p === "string").join(" ") || `operation ${index}`,
      summary: str(op.summary), description: str(op.description),
      responseDescriptions: isRecord(op.responseDescriptions) ? Object.fromEntries(Object.entries(op.responseDescriptions).filter((entry): entry is [string, string] => typeof entry[1] === "string")) : {},
      method: str(op.method, "?"), route: str(op.route, "?"),
      auth: list(op.auth).filter(isRecord).map(a => ({ scheme: str(a.scheme), ...(typeof a.secret === "string" ? { secret: a.secret } : {}) })),
      parameters: list(op.parameters).filter(isRecord).map(p => ({ name: str(p.name, "?"), location: str(p.location, "?"), type: str(p.type, "?"), required: typeof p.required === "boolean" ? p.required : undefined, description: str(p.description) })),
      responses: list(op.responses).filter(isRecord).map(r => ({
        status: typeof r.status === "number" ? r.status : null,
        mediaType: typeof r.mediaType === "string" ? r.mediaType : null,
        type: r.type === null ? null : typeof r.type === "string" ? r.type : undefined,
      })),
    };
  });
  const problems = list(preview.problems).filter(isRecord).map(p => ({ target: str(p.target), message: str(p.message) }));
  return { provider: str(preview.provider), operations, types: isRecord(preview.types) ? preview.types : {}, problems };
}

export function responseLabel(response: PreviewResponse): string {
  const type = response.type === undefined ? "type unknown" : response.type === null ? "empty" : response.type;
  return [response.status ?? "status unknown", type, response.mediaType ?? (response.type === null ? "" : "media unknown")].filter(Boolean).join(" ");
}

/* ---------- evidence ---------- */

/**
 * The describe step's original records, with the status the backend says they have now. The
 * backend's `status` wins over anything the source block asserts about itself: once the saved text
 * departs from what was described, those records are history — and so are they while unsaved edits
 * exist, since nothing was recorded about text that has not been saved.
 */
export function draftProvenance(evidence: DraftEvidence | undefined, unsavedEdits = false): SchemaProvenance | undefined {
  if (!evidence) return undefined;
  const read = readSchemaProvenance(evidence.source);
  if (!read) return undefined;
  return { ...read, status: evidence.status === "current" && !unsavedEdits ? "current" : "stale" };
}
export function manualTargetsOf(evidence: DraftEvidence | undefined): string[] {
  return list(evidence?.manualTargets).filter((t): t is string => typeof t === "string" && t.startsWith("#"));
}
/** True when the user supplied `target` or something beneath or above it. */
export function isManual(manualTargets: readonly string[], target: string): boolean {
  return manualTargets.some(m => m === target || m.startsWith(`${target}/`) || target.startsWith(`${m}/`));
}

/* ---------- problems ---------- */

export interface DraftProblem extends DraftDiagnostic { index: number; label: string; operation: string; field: string }
/** Errors first then warnings, numbered E1… W1…, each named by the operation or type it concerns. */
export function orderProblems(diagnostics: readonly DraftDiagnostic[], preview: DraftPreview | undefined): DraftProblem[] {
  const indexed = diagnostics.map((d, index) => ({ d, index }));
  const errors = indexed.filter(x => x.d.severity === "error");
  const warnings = indexed.filter(x => x.d.severity !== "error");
  return [...errors.map((x, i) => ({ ...x, label: `E${i + 1}` })), ...warnings.map((x, i) => ({ ...x, label: `W${i + 1}` }))].map(({ d, index, label }) => {
    const tokens = tokensOf(d.target);
    let operation = "draft";
    let rest = tokens;
    if (tokens[0] === "operations" && tokens[1] !== undefined) {
      operation = preview?.operations[Number(tokens[1])]?.name ?? `operation ${tokens[1]}`;
      rest = tokens.slice(2);
    } else if (tokens[0] === "types" && tokens[1] !== undefined) {
      operation = tokens[1];
      rest = tokens.slice(2).filter(t => t !== "fields");
    } else if (tokens.length) {
      operation = tokens[0]!;
      rest = tokens.slice(1);
    }
    return { ...d, index, label, operation, field: rest.join(".") };
  });
}

/** Saved descriptors and drafts share the reader; execution still uses the original bytes. */
export function descriptorPreview(descriptor: { provider: string; types: Record<string, unknown>; operations: unknown[] }): DraftPreview {
  return readPreview({ ...descriptor, operations: descriptor.operations.map(raw => {
    const op = isRecord(raw) ? raw : {};
    return { ...op, responses: isRecord(op.responses) ? Object.entries(op.responses).map(([status, type]) => ({ status: Number(status), type, mediaType: null })) : [] };
  }) })!;
}
