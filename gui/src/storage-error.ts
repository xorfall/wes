import type { WorkBlocker } from "./protocol";

/** Preserves authoritative blockers instead of inferring eligibility from UI state. */
export class StorageError extends Error {
  constructor(message: string, readonly blockers: readonly WorkBlocker[] = [],
    readonly code?: string, readonly mayHaveApplied?: boolean) {
    super(message);
    this.name = "StorageError";
  }
}

export function storageError(text: string): StorageError {
  try {
    const value = JSON.parse(text) as { message?: unknown; blockers?: unknown; code?: unknown; mayHaveApplied?: unknown };
    const blockers = Array.isArray(value.blockers) ? value.blockers.filter((item): item is WorkBlocker =>
      item && (item.node === null || typeof item.node === "string") && Array.isArray(item.cells)
      && item.cells.every((cell: unknown) => typeof cell === "string")
      && typeof item.state === "string" && typeof item.reason === "string") : [];
    return new StorageError(typeof value.message === "string" ? value.message : text, blockers,
      typeof value.code === "string" ? value.code : undefined,
      typeof value.mayHaveApplied === "boolean" ? value.mayHaveApplied : undefined);
  } catch {
    return new StorageError(text || "Storage request could not be confirmed; do not retry automatically.");
  }
}
