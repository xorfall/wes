/**
 * What a presented Dataset may be read through: the stored result that holds it, for one exact
 * session generation. A value block provides this once for everything it draws, so a Dataset at
 * any depth reads through the same source without its callers knowing it is there.
 */
import { createContext, useSyncExternalStore } from "react";
import type { Segment } from "../MonoLine";
import type { Engine } from "../../engine";
import type { Mode } from "../../presentation/types";

export interface DatasetSource {
  readonly engine: Engine;
  /** The stored result's handle. */
  readonly handle: string;
  /** The workspace session generation every read is bound to. */
  readonly generation: string;
}

export interface DatasetHost {
  /** Absent where the shown value is not a stored result (a live sample, a gallery example). */
  readonly source?: DatasetSource;
  /** The workspace name of the result drawn, when it has one: what management commands refer to. */
  readonly name?: string;
  readonly mode: Mode;
  /** Whether the block is collapsed: nothing is read for a collapsed block. */
  readonly collapsed: boolean;
}

export const DatasetHostContext = createContext<DatasetHost | undefined>(undefined);

/**
 * Stored results whose Dataset access was withdrawn in this session. The block holding one
 * replaces everything it showed — descriptor, count, schema, rows — with a generic notice, at
 * every tier, until its handle or the session changes.
 */
class Withdrawals {
  private withdrawn = new Set<string>();
  private listeners = new Set<() => void>();
  private version = 0;
  private key(source: Pick<DatasetSource, "handle" | "generation">) { return `${source.generation}\u0000${source.handle}`; }
  readonly subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  readonly snapshot = () => this.version;
  has(source: Pick<DatasetSource, "handle" | "generation">): boolean { return this.withdrawn.has(this.key(source)); }
  /** Withdraws several handles of one session with one notification. */
  withdrawMany(handles: readonly string[], generation: string): void {
    if (handles.every(handle => this.withdrawn.has(this.key({ handle, generation })))) return;
    for (const old of this.withdrawn) if (!old.startsWith(`${generation}\u0000`)) this.withdrawn.delete(old);
    for (const handle of handles) this.withdrawn.add(this.key({ handle, generation }));
    this.version++;
    for (const listener of this.listeners) listener();
  }
  withdraw(source: Pick<DatasetSource, "handle" | "generation">): void {
    const key = this.key(source);
    if (this.withdrawn.has(key)) return;
    // Only the current session's entries matter; older ones are dropped with it.
    for (const old of this.withdrawn) if (!old.startsWith(`${source.generation}\u0000`)) this.withdrawn.delete(old);
    this.withdrawn.add(key);
    this.version++;
    for (const listener of this.listeners) listener();
  }
}
export const datasetWithdrawals = new Withdrawals();

/** A stored result as one session saw it: the identity withdrawal is recorded under. */
export interface StoredIdentity { readonly handle: string; readonly generation: string }

/**
 * The stored identity of a shown value: only when it was read from that handle in that session.
 * A value with no handle (a live sample, an example) has none, and nothing is read for it.
 */
export function storedIdentity(value: unknown, handle: string | undefined, generation: string | undefined): StoredIdentity | undefined {
  return value !== undefined && handle && generation ? { handle, generation } : undefined;
}

/**
 * A shown value through the withdrawal gate: the value while access stands, nothing once the
 * stored result it came from was withdrawn. Every surface that draws a stored value or anything
 * derived from it (type, JSON, facts, copy text) takes it from here.
 */
export function useStoredGate<T>(value: T | undefined, stored: StoredIdentity | undefined, node?: { readonly accessWithdrawn?: true }): { readonly value: T | undefined; readonly withdrawn: boolean } {
  // Either the engine's notice for the node or a refused read of the handle withdraws it.
  const withdrawn = useWithdrawn()(stored) || node?.accessWithdrawn === true;
  return withdrawn ? { value: undefined, withdrawn } : { value, withdrawn };
}

/**
 * Whether a stored result's Dataset access was withdrawn, re-rendering the caller when that
 * changes. Every surface that shows a result's type, count or facts asks this, so a withdrawal
 * clears them all at once.
 */
export function useWithdrawn(): (stored: StoredIdentity | undefined) => boolean {
  useSyncExternalStore(datasetWithdrawals.subscribe, datasetWithdrawals.snapshot, datasetWithdrawals.snapshot);
  return stored => stored !== undefined && datasetWithdrawals.has(stored);
}

/** The generic header of a withdrawn result: no type structure, metadata, count or facts. */
// Type-free: any stored result can be withdrawn, and its type is itself value-derived.
export const WITHDRAWN_TYPE_LABEL = "Result";
export const WITHDRAWN_FACTS: readonly Segment[] = [{ text: "Access withdrawn", role: "mono-warn" }];
export const WITHDRAWN_TITLE = `${WITHDRAWN_TYPE_LABEL} · Access withdrawn`;

/**
 * A result header with everything the value told it replaced by the generic withdrawal header.
 * Only the value-derived fields change; identity, size and controls stay where they were.
 */
export function withdrawnHeader<T extends { readonly type?: unknown; readonly meta?: unknown; readonly typeLabel?: string; readonly header?: readonly Segment[] }>(header: T): T {
  return { ...header, type: undefined, meta: undefined, typeLabel: WITHDRAWN_TYPE_LABEL, header: WITHDRAWN_FACTS };
}
