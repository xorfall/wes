/**
 * Dataset and analysis management through the ordinary session commands.
 *
 * Nothing here executes anything. An action writes the exact command into the prompt, where the
 * person reads it and submits it through the same admission as any typed command; the engine
 * checks authority, the plan's liveness and every root again when it runs. No identifier is
 * managed directly: commands name results by their workspace names only.
 *
 * Commands follow the engine's grammar exactly:
 *   :dataset inspect $analysis.outputs
 *   :dataset plan-delete $analysis.outputs > deletion
 *   :dataset delete $deletion references:true protected:false
 *   :dataset snapshot $holding.dataset basis:"sha256:…" generation:"4" digest:"sha256:…" > shownPrefix
 *   :dataset retention $holding.dataset basis:"sha256:…" > retention
 *   :scan resume $analysis > continuation
 */
import { createContext } from "react";
import { canonicalOrdinal } from "../dataset-read";
import type { StoredValue } from "../protocol";

/** The engine's binding grammar; a name outside it cannot be referenced or bound. */
const BINDING = /^[\p{L}\p{Nd}_]+$/u;
/**
 * Node ids a command can name as `$id`: a strict subset of the language's reference characters
 * (letters, decimal digits and `_`). Engine ids are `id<number>`; anything else offers no command.
 */
export const REFERABLE_NODE = /^[A-Za-z0-9_]+$/;
/** The engine's canonical run identity: a lowercase hyphenated UUID, the only form `run:` accepts. */
export const CANONICAL_RUN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const MAX_CANDIDATES = 999;

/** Where a reviewed command goes: the session prompt, plus the names already bound there. */
export interface Composer {
  readonly compose: (command: string) => void;
  readonly taken: ReadonlySet<string>;
}
export const ComposeContext = createContext<Composer | undefined>(undefined);

/** `base`, else `base2`, `base3`…: the first name nobody in this workspace uses. */
export function freshName(base: string, taken: ReadonlySet<string>): string | undefined {
  if (!BINDING.test(base)) return undefined;
  for (let at = 1; at <= MAX_CANDIDATES; at++) {
    const name = at === 1 ? base : `${base}${at}`;
    if (!taken.has(name)) return name;
  }
  return undefined;
}

/**
 * The expression naming the value at `pointer` inside the result bound to `name`: `$name` and its
 * record fields, `$analysis.outputs`. Undefined when the result is unnamed or the path steps
 * through something a name cannot spell (a list index, a field outside the binding grammar).
 */
export function selectionExpression(name: string | undefined, pointer: string): string | undefined {
  if (!name || !BINDING.test(name)) return undefined;
  if (pointer === "") return `$${name}`;
  if (!pointer.startsWith("/")) return undefined;
  const fields = pointer.slice(1).split("/").map(part => part.replace(/~1/g, "/").replace(/~0/g, "~"));
  if (fields.some(field => !BINDING.test(field) || /^\p{Nd}+$/u.test(field))) return undefined;
  return `$${[name, ...fields].join(".")}`;
}

export const inspectCommand = (dataset: string) => `:dataset inspect ${dataset}`;
export const planDeleteCommand = (dataset: string, plan: string) => `:dataset plan-delete ${dataset} > ${plan}`;
export const resumeCommand = (analysis: string, continuation: string) => `:scan resume ${analysis} > ${continuation}`;
/**
 * Captures one exact committed prefix of `dataset` as a new result. `basis` is the result's own
 * snapshot digest, `generation` and `digest` the prefix a reader showed; they only constrain what the
 * engine checks again when the command runs and grant nothing. Undefined unless both digests are
 * canonical and the generation is a canonical u64 decimal of at least 1, as the engine requires.
 */
export function snapshotCommand(dataset: string, basis: string, generation: string, digest: string, result: string): string | undefined {
  if (!SHA256.test(basis) || !SHA256.test(digest) || !canonicalOrdinal(generation) || generation === "0") return undefined;
  return `:dataset snapshot ${dataset} basis:"${basis}" generation:"${generation}" digest:"${digest}" > ${result}`;
}
/**
 * Previews the storage footprint of exactly the snapshot `basis` names. The digest only guards the
 * name when the engine reads it again and grants nothing; the preview is no Keep, reservation or cost.
 * Undefined unless the digest is canonical.
 */
export function retentionCommand(dataset: string, basis: string, result: string): string | undefined {
  if (!SHA256.test(basis)) return undefined;
  return `:dataset retention ${dataset} basis:"${basis}" > ${result}`;
}
/** Both approvals are always written out: neither is ever implied by leaving it unset. */
export const deleteCommand = (plan: string, references: boolean, protectedData: boolean) =>
  `:dataset delete ${plan} references:${references} protected:${protectedData}`;

/** A root that keeps the dataset alive, as the plan lists it. */
export interface PlanReference { readonly identity: string; readonly kind: string; readonly retention: string }
/** The reviewable projection of a live deletion plan. Its authority never reaches the client. */
export interface DeletePlan {
  readonly dataset: string;
  readonly generation: string;
  readonly protectedBytes: string;
  readonly activeReaders: string;
  readonly activeWriter: boolean;
  readonly references: readonly PlanReference[];
  readonly notice: string;
}

const PLAN_KEYS = ["dataset", "generation", "protectedBytes", "activeReaders", "activeWriter", "references", "notice"];
const REFERENCE_KEYS = ["identity", "kind", "retention"];
const DECIMAL = /^(?:0|[1-9][0-9]{0,19})$/;
const SHA256 = /^sha256:[0-9a-f]{64}$/;
/** References one plan may list before the review refuses to draw it as complete. */
const MAX_REFERENCES = 1000;

function exact(value: unknown, keys: readonly string[]): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const own = Object.keys(value);
  return own.length === keys.length && keys.every(key => Object.hasOwn(value, key));
}

/** The plan a `DatasetDeletePlan` value projects, or undefined when it is not exactly one. */
export function deletePlanOf(value: StoredValue | undefined): DeletePlan | undefined {
  if (value?.type.kind !== "meta" || value.type.name !== "DatasetDeletePlan") return undefined;
  const raw = value.data;
  if (!exact(raw, PLAN_KEYS)) return undefined;
  const { dataset, generation, protectedBytes, activeReaders, activeWriter, references, notice } = raw;
  if (typeof dataset !== "string" || typeof notice !== "string" || typeof activeWriter !== "boolean") return undefined;
  for (const count of [generation, protectedBytes, activeReaders]) if (typeof count !== "string" || !DECIMAL.test(count)) return undefined;
  if (!Array.isArray(references) || references.length > MAX_REFERENCES) return undefined;
  const listed: PlanReference[] = [];
  for (const item of references) {
    if (!exact(item, REFERENCE_KEYS) || typeof item.identity !== "string" || typeof item.kind !== "string" || typeof item.retention !== "string") return undefined;
    listed.push({ identity: item.identity, kind: item.kind, retention: item.retention });
  }
  return { dataset, generation: generation as string, protectedBytes: protectedBytes as string, activeReaders: activeReaders as string, activeWriter, references: listed, notice };
}
