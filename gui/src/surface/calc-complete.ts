/**
 * What the editor is allowed to suggest.
 *
 * Only two things: the operations the package names, and the names this workspace holds. Nothing
 * from JavaScript, because offering `Math.round` or `.length` would be offering something the
 * engine will refuse, and a suggestion that does not work is worse than no suggestion.
 *
 * Every candidate carries its arity, counted the way the editor counts it — method style includes
 * the receiver — so `filter(fn)` reads "exactly 2 args" and means it.
 */
import { describeArity } from "./calc-highlight";
import type { Language } from "./language";
import type { Segment } from "./MonoLine";

export interface Candidate {
  readonly text: string;
  /** How it reads in the list: `filter(fn)`, `reduce(fn, init)`, `$orders`. */
  readonly label: string;
  /** `exactly 2 args`, or nothing for a name. */
  readonly detail?: string;
  readonly kind: "operation" | "name";
}

/**
 * What an operation's arguments are usually called.
 *
 * Keyed by the package's semantic id rather than by the written name, so a package that renames
 * `filter` still gets the right hint and a semantics nobody has named here falls back to `…`. The
 * package carries arity and not parameter names; this is the editor's own signature hint and not a
 * second opinion about the language.
 */
const PARAMETERS: Record<string, readonly string[]> = {
  map: ["fn"],
  filter: ["fn"],
  reduce: ["fn", "init"],
  concat: ["other"],
  "sort-by": ["selector"],
  take: ["n"],
  skip: ["n"],
  range: ["from", "to", "step"],
  "unwrap-or": ["fallback"],
  has: ["key"],
  field: ["name"],
  div: ["by"],
  rem: ["by"],
  "round-div": ["by", "places"],
  check: ["type"],
  call: ["provider", "capability", "arguments"],
  "iter-split": ["separator"],
  "iter-regex-split": ["pattern"],
  "iter-matches": ["pattern"],
  "iter-use": ["fn"],
  "iter-checked": ["type"],
  "parse-json": ["type"],
};

/** `filter(fn)` — the name and what it takes after the receiver. */
export function signature(name: string, semantics: string, arity: { readonly min: number; readonly max: number }): string {
  const named = PARAMETERS[semantics];
  // Method style spends the receiver, so a two-argument operation shows one parameter.
  const visible = Math.max(0, arity.max - 1);
  if (visible === 0) return `${name}()`;
  const parts = (named ?? []).slice(0, visible);
  while (parts.length < visible) parts.push("…");
  return `${name}(${parts.join(", ")})`;
}

/**
 * Whether an operation answers to what has been typed.
 *
 * A namespaced operation answers to its last part as well as to the whole: typing `li` after a dot
 * should find `iter.lines`, because that is where the person's hands are.
 */
function matches(name: string, prefix: string): boolean {
  const lower = name.toLowerCase();
  if (lower.startsWith(prefix)) return true;
  const last = lower.slice(lower.lastIndexOf(".") + 1);
  return last.startsWith(prefix);
}

/**
 * The candidates for what has been typed so far.
 *
 * Operations first, then the workspace's names, each group in the package's and the workspace's own
 * order — a list that reorders itself as you type is a list you cannot learn.
 */
export function completions(
  written: string,
  language: Language,
  names: readonly string[] = [],
  limit = 8,
): Candidate[] {
  const prefix = written.replace(/^\$/, "").toLowerCase();
  const wantsName = written.startsWith("$");
  const operations: Candidate[] = wantsName
    ? []
    : language
        .operations()
        .filter((name) => matches(name, prefix))
        .map((name) => {
          const arity = language.arity(name)!;
          const semantics = language.package.operations[name]!.operation;
          return {
            text: name,
            label: signature(name, semantics, arity),
            detail: describeArity(arity),
            kind: "operation" as const,
          };
        });
  const workspace: Candidate[] = names
    .filter((name) => name.toLowerCase().startsWith(prefix))
    .map((name) => ({ text: `$${name}`, label: `$${name}`, kind: "name" as const }));
  return [...operations, ...workspace].slice(0, limit);
}

/** One row of the completion list: the chosen one leads with a caret, the rest with two spaces. */
export function candidateLine(candidate: Candidate, chosen: boolean, width = 20): Segment[] {
  // Always at least one space before the arity, so a long signature does not run into it.
  const label = candidate.label + " ".repeat(Math.max(1, width - candidate.label.length));
  const line: Segment[] = [
    { text: chosen ? "› " : "  ", role: "mono-ref" },
    { text: label, role: chosen ? "mono-provider" : "mono-dim" },
  ];
  if (candidate.detail) {
    line.push({ text: "· ", role: "mono-faint" }, { text: candidate.detail, role: "mono-faint" });
  }
  return line;
}

/** Where the list says its candidates came from, so nobody expects a JavaScript global. */
export function completionSource(language: Language): Segment[] {
  return [
    { text: "from the workspace's ", role: "mono-faint" },
    { text: "calc/default.yaml", role: "mono-literal" },
    { text: language.origin === "bundled" ? " — bundled copy, no JS globals" : " — no JS globals", role: "mono-faint" },
  ];
}
