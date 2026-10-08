/**
 * The calculation language, as the engine defines it.
 *
 * The highlighter and the completion list are generated from the package the engine serves, never
 * written by hand: when the language gains an operation the UI already knows it, and completion can
 * only ever offer what the engine would accept. An engine older than the route leaves the client
 * with the copy bundled at build time; that copy is checked against the engine in the Rust suite,
 * so it is the same grammar, only possibly an older one — which is why its origin is reported and
 * the settings screen says so.
 */
import bundled from "./language-default.json";

export interface OperatorSpec {
  /** The semantics this operator stands for, as the package writes it: "or", "eq", "add". */
  readonly operation: string;
  readonly precedence: number;
}

export interface OperationSpec {
  /** The semantics this operation stands for, as the package writes it: "filter", "reduce". */
  readonly operation: string;
  readonly min: number;
  readonly max: number;
  /** Whether the engine also accepts it after a receiver: `rows.filter(fn)`. Only `true` says so. */
  readonly method: boolean;
}

export interface LanguagePackage {
  readonly language: string;
  readonly version: number;
  /** The four lexical productions: identifier, strings, comments, numbers. */
  readonly lexical: Readonly<Record<string, string>>;
  /** Each statement keyword with the production it stands for: `const` is a `binding`. */
  readonly statements: Readonly<Record<string, string>>;
  readonly operators: Readonly<Record<string, OperatorSpec>>;
  readonly operations: Readonly<Record<string, OperationSpec>>;
  /** The package file itself, which is what a run records to say which package it ran under. */
  readonly source: string;
}

/** Where the package in hand came from. `bundled` means the engine does not serve one yet. */
export type PackageOrigin = "engine" | "bundled";

export interface Language {
  readonly origin: PackageOrigin;
  readonly package: LanguagePackage;
  /** The statement keywords, in the order the package writes them. */
  keywords(): readonly string[];
  /** The operator symbols, longest first, so a lexer matching in order never splits `!=` into `!`. */
  operators(): readonly string[];
  /** The operation names, in the order the package writes them. */
  operations(): readonly string[];
  /** How many arguments an operation takes, or undefined when the package does not name it. */
  arity(name: string): { readonly min: number; readonly max: number } | undefined;
  /** Whether the package declares the operation callable as a method; an undeclared one is not. */
  method(name: string): boolean;
}

export const bundledPackage = bundled as LanguagePackage;

const ROUTE = "/language/calc";

/** Answers about a package, without a question ever reaching the engine twice. */
export function readLanguage(pack: LanguagePackage, origin: PackageOrigin): Language {
  const keywords = Object.keys(pack.statements);
  const operators = Object.keys(pack.operators).sort((a, b) => b.length - a.length || (a < b ? -1 : a > b ? 1 : 0));
  const operations = Object.keys(pack.operations);
  return {
    origin,
    package: pack,
    keywords: () => keywords,
    operators: () => operators,
    operations: () => operations,
    arity(name) {
      const spec = pack.operations[name];
      return spec && { min: spec.min, max: spec.max };
    },
    method: (name) => Object.hasOwn(pack.operations, name) && pack.operations[name]!.method === true,
  };
}

/** A package is a package only if it carries the four parts the client generates its UI from. */
function isPackage(value: unknown): value is LanguagePackage {
  const p = value as LanguagePackage | null;
  return !!p && typeof p === "object"
    && typeof p.version === "number"
    && !!p.statements && typeof p.statements === "object"
    && !!p.operators && typeof p.operators === "object"
    && !!p.operations && typeof p.operations === "object";
}

let pending: Promise<Language> | undefined;

/**
 * Reads the engine's package once and remembers it. Every later caller gets the same answer, because
 * a grammar that changed under a half-typed line would colour the first half by one package and the
 * second by another.
 */
export function language(fetcher: typeof fetch = fetch): Promise<Language> {
  pending ??= load(fetcher);
  return pending;
}

/** Only for tests and for a client that has just reconnected to a different engine. */
export function forgetLanguage(): void {
  pending = undefined;
}

async function load(fetcher: typeof fetch): Promise<Language> {
  try {
    const response = await fetcher(ROUTE, { cache: "no-store" });
    if (!response.ok) return readLanguage(bundledPackage, "bundled");
    const served: unknown = await response.json();
    if (!isPackage(served)) return readLanguage(bundledPackage, "bundled");
    return readLanguage(served, "engine");
  } catch {
    return readLanguage(bundledPackage, "bundled");
  }
}
