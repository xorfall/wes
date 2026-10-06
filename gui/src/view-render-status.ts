import { createContext } from "react";
import type { Mode } from "./presentation/types";
import type { ViewFrame } from "./value-views/instances";

/**
 * Delivery receipts for mounted live ViewInstance canvases in this document. A receipt proves
 * lifecycle delivery (sent, acknowledged as drawn), never visual correctness. It carries only
 * payload-independent fields: no messages, pixels, input, state, events, source or sizes.
 */
export type RenderStatus = "loading" | "ready" | "drawn" | "failed";
export type RenderError = "assets_unavailable" | "renderer_failed" | "communication_rejected" | "draw_timeout" | "navigation_rejected" | "delivery_failed";
export interface RenderScope { readonly workspace: string; readonly generation: string; readonly node: string; readonly instance: string }
export interface RenderReceipt {
  readonly host: string; readonly digest: string; readonly mode: Mode; readonly status: RenderStatus;
  readonly requestedInputRevision: string | null; readonly drawnInputRevision: string | null;
  readonly sentSequence: number; readonly ackSequence: number; readonly error: RenderError | null;
}
export interface RenderStatusReader { receipts(scope: RenderScope): readonly RenderReceipt[] }

export const MAX_SCOPE_HOSTS = 32;
const MAX_TRACKED_HOSTS = 256;
const MAX_SCOPE_FIELD_BYTES = 256;
const SCOPE_FIELDS = ["generation", "instance", "node", "workspace"] as const;
const INVALID_SCOPE = "Invalid view render status request.";

const scopeKey = (scope: RenderScope) => JSON.stringify([scope.workspace, scope.generation, scope.node, scope.instance]);

/** One mounted canvas. A restart begins a new channel; failure is final until the next restart. */
export class RenderHost {
  readonly id = crypto.randomUUID();
  private status: RenderStatus = "loading";
  private error: RenderError | null = null;
  private requested: string | null = null;
  private drawn: string | null = null;
  private sent = 0;
  private acknowledged = 0;
  constructor(private readonly describe: () => { digest: string; mode: Mode }) {}
  restart(): void { this.status = "loading"; this.error = null; this.requested = null; this.drawn = null; this.sent = 0; this.acknowledged = 0; }
  ready(): void { if (this.status === "loading") this.status = "ready"; }
  /** Called after the snapshot was actually posted, with the revision of that exact snapshot. */
  delivered(sequence: number, revision: string | null): void {
    if (this.status === "failed") return;
    this.sent = sequence; this.requested = revision;
  }
  /** Called only for the acknowledgement of the current flight, with that flight's revision. */
  drew(sequence: number, revision: string | null): void {
    if (this.status === "failed") return;
    this.acknowledged = sequence; this.drawn = revision; this.status = "drawn";
  }
  failed(error: RenderError): void {
    if (this.status === "failed") return;
    this.status = "failed"; this.error = error;
  }
  receipt(): RenderReceipt {
    const { digest, mode } = this.describe();
    return Object.freeze({ host: this.id, digest, mode, status: this.status, requestedInputRevision: this.requested,
      drawnInputRevision: this.drawn, sentSequence: this.sent, ackSequence: this.acknowledged, error: this.error });
  }
}

/** Holds only currently registered canvases; release removes them. Reading never creates entries. */
export class RenderStatusRegistry implements RenderStatusReader {
  private readonly scopes = new Map<string, Set<RenderHost>>();
  private total = 0;
  get scopeCount(): number { return this.scopes.size; }
  /** Returns the release. Hosts beyond capacity stay unobserved rather than displacing others. */
  register(scope: RenderScope, host: RenderHost): () => void {
    const key = scopeKey(scope);
    const existing = this.scopes.get(key);
    if (existing?.has(host)) return () => {};
    if ((existing?.size ?? 0) >= MAX_SCOPE_HOSTS || this.total >= MAX_TRACKED_HOSTS) return () => {};
    const hosts = existing ?? new Set<RenderHost>();
    hosts.add(host); this.scopes.set(key, hosts); this.total++;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      const current = this.scopes.get(key);
      if (!current?.delete(host)) return;
      this.total--;
      if (!current.size) this.scopes.delete(key);
    };
  }
  receipts(scope: RenderScope): readonly RenderReceipt[] {
    const hosts = this.scopes.get(scopeKey(scope));
    return hosts ? [...hosts].slice(0, MAX_SCOPE_HOSTS).map(host => host.receipt()) : [];
  }
}

/** Per-document registry; the terminal bridge of each workspace engine reads it by full scope. */
export const documentRenderStatus = new RenderStatusRegistry();

const scopeField = (value: unknown): value is string =>
  typeof value === "string" && value.length > 0 && new TextEncoder().encode(value).length <= MAX_SCOPE_FIELD_BYTES
    && !/[\u0000-\u001f\u007f-\u009f]/u.test(value);

/** Parses a decoded broker scope: exactly workspace, generation, node and instance strings. */
export function parseRenderScope(input: unknown): RenderScope {
  if (!input || typeof input !== "object" || Array.isArray(input)) throw new Error(INVALID_SCOPE);
  const fields = input as Record<string, unknown>;
  const keys = Object.keys(fields).sort();
  if (keys.length !== SCOPE_FIELDS.length || keys.some((key, index) => key !== SCOPE_FIELDS[index]) || !SCOPE_FIELDS.every(key => scopeField(fields[key])))
    throw new Error(INVALID_SCOPE);
  return { workspace: fields.workspace as string, generation: fields.generation as string, node: fields.node as string, instance: fields.instance as string };
}

/** What a canvas needs to find its live instance scope. Absent for ordinary value renderers. */
export interface RenderObservation {
  readonly registry: RenderStatusRegistry;
  scope(path: string, instance: string | undefined): RenderScope | undefined;
}
export const RenderObservationContext = createContext<RenderObservation | undefined>(undefined);

/** Only frame entries rendered at their own `view/<id>` path with the matching instance are observed. */
export function frameRenderObservation(registry: RenderStatusRegistry, workspace: string, generation: string, frame: ViewFrame): RenderObservation {
  const entries = new Map(frame.instances.map(entry => [`view/${entry.id}`, entry]));
  return {
    registry,
    scope: (path, instance) => {
      const entry = entries.get(path);
      return entry && instance !== undefined && entry.instance === instance ? { workspace, generation, node: entry.id, instance } : undefined;
    },
  };
}
