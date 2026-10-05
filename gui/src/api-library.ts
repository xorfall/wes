/** Backend-owned API artifacts and deterministic source conversion. */
export interface PackageKey { service: string; apiVersion: string; scope: string }
export interface SpecPackage { key: PackageKey; revision: string; accepted: boolean; origin: string; sourceDigest?: string | null }
export interface SpecOperation { path: string[]; method: string; route: string; summary?: string; description?: string; responseDescriptions?: Record<string,string>; parameters: {name:string; location:string; type:string; required:boolean; description?:string}[]; responses: Record<string,string|null>; auth: {scheme:string;secret?:string}[]; evidence?: string }
export interface Descriptor { version: number; provider: string; operations: SpecOperation[]; types: Record<string,unknown>; diagnostics?: string[]; servers?: {url:string}[]; source?: unknown }
export interface SpecResult { descriptorPath: string; package: SpecPackage; descriptor: Descriptor; source: string }
export async function libraryRequest<T>(request: object, signal?: AbortSignal): Promise<T> {
  const response = await fetch("/api-library", { method:"POST", headers:{"Content-Type":"application/json"}, body:JSON.stringify(request), ...(signal ? {signal} : {}) });
  if (!response.ok) throw new Error((await response.text()).slice(0,4096) || `API library request failed (${response.status})`);
  return response.json() as Promise<T>;
}
export function quoted(value: string): string { return JSON.stringify(value); }
/**
 * `:describe` keys are storage identity, not API facts: the backend writes this synthetic version and
 * `scope = sha256(location)`, so the location itself is what identifies such a key to a person.
 */
export const DESCRIBED_API_VERSION = "describe";
const DESCRIBED_ORIGIN = /^described:(.*); source:(.*)$/s;
/** The version and scope worth reading, or none for a key the describe step synthesized. */
export function readableIdentity(key: PackageKey): { version: string; scope: string } | undefined {
  return key.apiVersion === DESCRIBED_API_VERSION ? undefined : { version: key.apiVersion, scope: key.scope };
}
/** Where a revision came from, once: a described location and, only when it differs, the page it was discovered at. */
export function readableOrigin(origin: string): { source: string; via?: string } | undefined {
  if (!origin.trim()) return undefined;
  const described = DESCRIBED_ORIGIN.exec(origin);
  if (!described) return { source: origin };
  const [, source, discovered] = described as unknown as [string, string, string];
  return discovered && discovered !== source ? { source, via: discovered } : { source };
}
/** Whether a describe location names a URL (any `scheme://`); anything else is a local file. */
export function describeSourceKind(location: string): "url" | "file" {
  return /^[A-Za-z][A-Za-z0-9+.-]*:\/\//.test(location.trim()) ? "url" : "file";
}
/** Why the typed alias or endpoint cannot form an import command, per field; empty when both can. */
export interface ImportFieldProblems { alias?: string; endpoint?: string }
const IMPORT_ALIAS = /^[A-Za-z_][A-Za-z0-9_]*$/;
const SCHEME_HOST_PATH = "an endpoint is scheme, host and path only.";
function endpointProblem(endpoint: string): string | undefined {
  if (!endpoint) return "Type the endpoint the snapshot will call.";
  let url: URL;
  try { url = new URL(endpoint); } catch { return "Type a full address, such as https://api.example.com/v1."; }
  if (!["http:", "https:"].includes(url.protocol)) return "Use an http:// or https:// address.";
  if (url.username || url.password) return "Remove the user name and password; credentials are set up in /env.";
  if (url.search) return `Remove the query part; ${SCHEME_HOST_PATH}`;
  if (url.hash) return `Remove the #fragment; ${SCHEME_HOST_PATH}`;
  return undefined;
}
/** The one validation authority for import input: the import forms show these, and the command refuses them. */
export function importSpecProblems(alias: string, endpoint: string): ImportFieldProblems {
  const aliasProblem = !alias ? "Type an alias." : IMPORT_ALIAS.test(alias) ? undefined : "Use letters, digits and _; start with a letter or _.";
  const endpointIssue = endpointProblem(endpoint);
  return { ...(aliasProblem ? { alias: aliasProblem } : {}), ...(endpointIssue ? { endpoint: endpointIssue } : {}) };
}
export function importSpecCommand(path: string, alias: string, endpoint: string, replace: boolean): string {
  const problems = importSpecProblems(alias, endpoint);
  const first = problems.alias ?? problems.endpoint;
  if (first) throw new Error(first);
  return `:import spec file:${quoted(path)} as:${alias} endpoint:${quoted(endpoint)} replace:${replace}`;
}

/*
 * Schema provenance: inert metadata the describe step leaves beside a saved descriptor, saying where
 * each final schema fact came from. `source.provenance` is versioned; only version 1 with a known status
 * is read, and anything else — unsupported `source.review`, a missing block, a malformed entry — reads as
 * "no provenance recorded", never as a claim. A target is a JSON pointer into the final descriptor
 * (`#/types/Item/fields/id/type`, `#/operations/2/auth`); a pointer addresses the original source.
 */
export type ProvenanceBasis = "documented" | "example" | "inferred" | "unknown";
export type ProvenanceStatus = "current" | "stale";
export interface ProvenanceLines { start: number; end: number }
export interface ProvenanceEntry { target: string; source: string; pointer: string; lines: ProvenanceLines[]; basis: ProvenanceBasis; reason: string }
export interface SchemaProvenance { status: ProvenanceStatus; entries: readonly ProvenanceEntry[]; location?: string }

/** Weakest first: a target with two claims is only as certain as its least certain claim. */
const BASIS_ORDER: readonly ProvenanceBasis[] = ["unknown", "inferred", "example", "documented"];
const PROVENANCE_VERSION = 1;
export const NO_CREDENTIALS = "no credentials attached";
export const NO_PROVENANCE = "no provenance";

function isRecord(value: unknown): value is Record<string, unknown> { return !!value && typeof value === "object" && !Array.isArray(value); }
function isBasis(value: unknown): value is ProvenanceBasis { return typeof value === "string" && (BASIS_ORDER as readonly string[]).includes(value); }
function isStatus(value: unknown): value is ProvenanceStatus { return value === "current" || value === "stale"; }
/** A canonical JSON pointer in fragment form: `#` then one or more `/token`s, where `~` only appears as `~0` or `~1`. */
const CANONICAL_POINTER = /^#(\/([^~/]|~[01])*)+$/;
export function isCanonicalPointer(value: unknown): value is string { return typeof value === "string" && CANONICAL_POINTER.test(value); }
function isLine(value: unknown): value is ProvenanceLines {
  return isRecord(value) && Number.isInteger(value.start) && Number.isInteger(value.end) && (value.start as number) >= 1 && (value.end as number) >= (value.start as number);
}
function nonempty(value: unknown): value is string { return typeof value === "string" && value.trim() !== ""; }
/**
 * One entry, or nothing. Every field has to be what the contract says — canonical target and pointer,
 * a nonempty source identifier and reason, a valid basis, and a list whose every range is a real
 * 1-based inclusive range (empty when deterministic extraction cites nothing). A record that fails
 * any of this is dropped whole, so a malformed record can never surface as a documented claim.
 */
function readEntry(value: unknown): ProvenanceEntry[] {
  if (!isRecord(value) || !isCanonicalPointer(value.target) || !isCanonicalPointer(value.pointer) || !isBasis(value.basis)) return [];
  if (!nonempty(value.source) || !nonempty(value.reason) || !Array.isArray(value.lines) || !value.lines.every(isLine)) return [];
  return [{ target: value.target, pointer: value.pointer, basis: value.basis, source: value.source, reason: value.reason, lines: value.lines.map(line => ({ start: line.start, end: line.end })) }];
}
/** Reads `source.provenance` if it is the supported version; anything else is simply not provenance. */
export function readSchemaProvenance(source: unknown): SchemaProvenance | undefined {
  if (!isRecord(source) || !isRecord(source.provenance)) return undefined;
  const raw = source.provenance;
  if (raw.version !== PROVENANCE_VERSION || !isStatus(raw.status) || !Array.isArray(raw.entries)) return undefined;
  const entries = raw.entries.flatMap(readEntry);
  return { status: raw.status, entries, ...(typeof source.location === "string" && source.location ? { location: source.location } : {}) };
}
/** One JSON-pointer token, escaped the way RFC 6901 asks (`~` then `/`). */
export function pointerToken(name: string): string { return name.replace(/~/g, "~0").replace(/\//g, "~1"); }
/** The entries about `target`: the exact target and the facts nested under it, excluding another type's fields. */
export function provenanceEntriesFor(provenance: SchemaProvenance | undefined, target: string): ProvenanceEntry[] {
  if (!provenance) return [];
  return provenance.entries.filter(entry => entry.target === target || (entry.target.startsWith(`${target}/`) && !entry.target.slice(target.length + 1).startsWith("fields/")));
}
export function weakestBasis(entries: readonly ProvenanceEntry[]): ProvenanceBasis | undefined {
  return BASIS_ORDER.find(basis => entries.some(entry => entry.basis === basis));
}
/** A basis as the screen says it: historical once the block is stale, because it was recorded before the saved text was edited. */
export function basisLabel(basis: ProvenanceBasis, provenance: SchemaProvenance | undefined): string {
  return provenance?.status === "stale" ? `${basis} · stale` : basis;
}
export const operationTarget = {
  auth: (operation: number) => `#/operations/${operation}/auth`,
  parameter: (operation: number, index: number) => `#/operations/${operation}/parameters/${index}`,
  response: (operation: number, status: string) => `#/operations/${operation}/responses/${pointerToken(status)}`,
};
/**
 * The auth column of one operation: what is attached, and on what basis the descriptor says so.
 * An empty `auth` is never "documented" on its own — only a documented entry for the operation's
 * `auth` target makes it "documented · no auth"; an unknown one keeps it unknown, and no entry at all
 * says so. Configured schemes always stay visible. A stale record says so too.
 */
export function authProvenanceLabel(auth: readonly { scheme: string; secret?: string }[], provenance: SchemaProvenance | undefined, operationIndex: number): string {
  const schemes = auth.map(a => a.secret ?? a.scheme).join(" · ");
  const basis = weakestBasis(provenance?.entries.filter(entry => entry.target === operationTarget.auth(operationIndex)) ?? []);
  const stale = provenance?.status === "stale" ? " · stale" : "";
  if (!basis) return `${auth.length ? schemes : NO_CREDENTIALS} · ${NO_PROVENANCE}${stale}`;
  if (auth.length) return `${schemes} · ${basisLabel(basis, provenance)}`;
  return basis === "documented" ? `documented · no auth${stale}` : `${basisLabel(basis, provenance)} · ${NO_CREDENTIALS}`;
}

export interface ApiPackage { key: PackageKey; revision: string; accepted: boolean; origin: string; sourceDigest: string | null }
export type Repository = { kind: "local"; directory: string } |
  { kind: "github"; owner: string; repository: string; commit: string; prefix: string };
export interface LibrarySettings { localDirectory: string; repository: Repository | null; extractor: string | null }
export interface LibraryStatus { managed?: boolean; settings: LibrarySettings | null; revision: string | null }
export interface PackageResult { found: boolean; from?: string; package?: ApiPackage; descriptor?: unknown; descriptorPath?: string; message?: string }

export const libraryAction = libraryRequest;
export function packageIdentity(key: PackageKey): string { return `${key.service} / ${key.apiVersion} / ${key.scope}`; }
export function revisionKey(p: ApiPackage): string { return `${packageIdentity(p.key)} / ${p.revision}`; }
export function parseCredentialReferences(value: string): Record<string,string> {
  if (!value.trim()) return {};
  const parsed: unknown = JSON.parse(value);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed) || Object.values(parsed).some(v => typeof v !== "string")) {
    throw new Error('Credential references must be an object such as {"token":"team/dev/api"}.');
  }
  return parsed as Record<string,string>;
}
