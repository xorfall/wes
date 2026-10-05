/**
 * Small, pure readings shared by the /spec tables: what a closed row may say about its inputs and
 * returns, how nullability reads off a type expression, and how much evidence a target has. Each one
 * only restates what the descriptor already knows; anything it cannot know stays unknown.
 */
import { useState } from "react";
import { NO_PROVENANCE, type ProvenanceBasis, type SchemaProvenance } from "../api-library";
import type { PreviewOperation, PreviewResponse } from "../draft-api";

/** Longer referenced type names read as their shape in a closed row; the exact name stays in the details. */
export const LONG_TYPE_NAME = 24;
const UNKNOWN = "unknown";
const EVIDENCE_ORDER: readonly ProvenanceBasis[] = ["documented", "example", "inferred", "unknown"];
const KIB = 1024;

function isRecord(value: unknown): value is Record<string, unknown> { return !!value && typeof value === "object" && !Array.isArray(value); }

/** A named type's own shape, as the descriptor states it, or `unknown` when it does not. */
function shapeOf(name: string, types: Record<string, unknown>): string {
  const definition = types[name];
  if (typeof definition === "string") return definition;
  return isRecord(definition) && typeof definition.base === "string" ? definition.base : UNKNOWN;
}

/**
 * The type expression for a closed row: long named types are replaced by their known shape, so a
 * generated identifier never crowds the table. Short or unresolved names are kept exactly.
 */
export function returnShape(expression: string, types: Record<string, unknown>): string {
  return expression.replace(/[A-Za-z_][A-Za-z0-9_]*/g, name => name.length > LONG_TYPE_NAME && Object.hasOwn(types, name) ? shapeOf(name, types) : name);
}

/** What a response returns in one word or expression: a body type, an empty body, or unknown. */
export function responseShape(response: PreviewResponse, types: Record<string, unknown>): string {
  if (response.type === null) return "empty body";
  if (response.type === undefined) return "unknown shape";
  return returnShape(response.type, types);
}

/** `ok` for a 2xx status, `warn` for any other, `dim` when the status itself is unknown. */
export function statusTone(status: number | null): string {
  if (status === null) return "mono-dim";
  return status >= 200 && status < 300 ? "mono-ok" : "mono-warn";
}

export function methodTone(method: string): string {
  switch (method.toUpperCase()) {
    case "GET": case "HEAD": return "mono-meta";
    case "POST": return "mono-ok";
    case "PUT": case "PATCH": return "mono-warn";
    case "DELETE": return "mono-bad";
    default: return "mono-dim";
  }
}

/**
 * A closed row's inputs: names in order, `?` only after an input the descriptor says is optional
 * (unknown requiredness gets no mark), and a request body as `body`.
 */
export function inputsSummary(parameters: PreviewOperation["parameters"]): string {
  const names = parameters.filter(p => p.location !== "body").map(p => p.required === false ? `${p.name}?` : p.name);
  return [...names, ...(parameters.some(p => p.location === "body") ? ["body"] : [])].join(", ");
}

/**
 * `Option<T>` read as T plus nullability, only when that is lossless: the whole expression is one
 * balanced `Option<…>`. Anything else keeps its full expression and is not called nullable.
 */
export function nullableParts(expression: string): { inner: string; nullable: boolean } {
  const match = /^Option<(.+)>$/.exec(expression.trim());
  if (!match) return { inner: expression, nullable: false };
  let depth = 0;
  for (const char of match[1]!) {
    if (char === "<") depth++;
    else if (char === ">" && --depth < 0) return { inner: expression, nullable: false };
  }
  return depth === 0 ? { inner: match[1]!, nullable: true } : { inner: expression, nullable: false };
}

/** The recorded bases under one target, strongest first, e.g. `documented 4 · inferred 1`. */
export function evidenceSummary(provenance: SchemaProvenance | undefined, target: string): string {
  const entries = provenance?.entries.filter(e => e.target === target || e.target.startsWith(`${target}/`)) ?? [];
  const counts = EVIDENCE_ORDER.map(basis => [basis, entries.filter(e => e.basis === basis).length] as const).filter(([, n]) => n > 0);
  const text = counts.length ? counts.map(([basis, n]) => `${basis} ${n}`).join(" · ") : NO_PROVENANCE;
  return provenance?.status === "stale" ? `${text} · historical` : text;
}

/** A byte count for a heading; the exact count belongs in the details. */
export function formatBytes(bytes: number): string {
  if (bytes < KIB) return `${bytes} ${bytes === 1 ? "byte" : "bytes"}`;
  const [value, unit] = bytes < KIB * KIB ? [bytes / KIB, "KB"] : [bytes / (KIB * KIB), "MB"];
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${unit}`;
}

type CopyState = "idle" | "copied" | "failed";
const COPY_TEXT: Record<CopyState, string> = { idle: "copy", copied: "copied", failed: "copy failed" };

/** Copies exact text. Absent where the platform has no clipboard, so it never promises what it cannot do. */
export function CopyButton({ text, label }: { text: string; label: string }) {
  const [state, setState] = useState<CopyState>("idle");
  const clipboard = typeof navigator === "undefined" ? undefined : navigator.clipboard;
  if (!clipboard) return null;
  return <button type="button" className="spec-t-linkbtn" aria-label={label} onClick={() => void clipboard.writeText(text).then(() => setState("copied"), () => setState("failed"))}>{COPY_TEXT[state]}</button>;
}
