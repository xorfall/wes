/**
 * What the run-state readers are given about a node's value, and the surface's number and padding
 * rules. The choosing that used to live here — which form a value takes — is the presentation
 * layer's now (`presentation/present.ts`).
 */
import type { TypeShape } from "../../protocol";

/** A node's value as the run-state readers see it. */
export interface FormValue {
  readonly type: TypeShape;
  readonly data: unknown;
  /** What the node is doing. */
  readonly state?: "ready" | "running" | "failed" | "stale" | "planned";
  /** The command made an HTTP call and the engine kept its inspection. */
  readonly http?: boolean;
  /** The process is asking something in place, and this is what it asked. */
  readonly asking?: string;
  /** What the command has written so far, while it is still running. */
  readonly wrote?: string;
}

/** A number the way the surface writes one: a thin space every three digits, never a comma. */
export function spaced(value: number): string {
  return value.toLocaleString("en-US").replace(/,/g, " ");
}

/** A name padded to a column, so the values beside it start where the eye expects them. */
export function padded(text: string, width: number): string {
  return text.length >= width ? text : text + " ".repeat(width - text.length);
}
