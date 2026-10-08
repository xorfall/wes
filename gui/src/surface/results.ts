import { budget } from "../limits/policy";
import { useEffect, useMemo, useReducer } from "react";
import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";
import type { WorkspaceNode } from "../workspace";
import { ResultWithdrawnError } from "../result-reader";

export interface ResultRead {
  value?: StoredValue;
  problem?: string;
  /** The read was refused because access to the result was withdrawn; reading again cannot help. */
  withdrawn?: boolean;
  pending?: Promise<void>;
}

/** Only current demand is cached. Busy readers do not queue superseded snapshots. */
export class ResultDemand {
  readonly reads = new Map<string, ResultRead>();
  private active = 0;
  private wanted = new Set<string>();
  private disposed = false;
  constructor(private fetch: (handle: string) => Promise<StoredValue>, private changed: () => void) {}
  update(handles: readonly string[]) {
    this.wanted = new Set(handles);
    for (const handle of this.reads.keys()) if (!this.wanted.has(handle)) this.reads.delete(handle);
    this.pump();
  }
  private pump() {
    if (this.disposed) return;
    for (const handle of this.wanted) {
      if (this.active >= budget("ui.result.reads")) break;
      if (this.reads.has(handle)) continue;
      const entry: ResultRead = {};
      this.reads.set(handle, entry);
      this.active++;
      entry.pending = this.fetch(handle).then(value => { entry.value = value; },
        error => { entry.problem = error instanceof Error ? error.message : String(error); entry.withdrawn = error instanceof ResultWithdrawnError; })
        .finally(() => {
          entry.pending = undefined;
          this.active--;
          if (this.disposed) return;
          // An obsolete completion owns no cache slot and can never overwrite a newer read.
          if (this.reads.get(handle) === entry) this.changed();
          this.pump();
        });
    }
  }
  retry(handle: string) {
    if (this.reads.get(handle)?.pending) return;
    this.reads.delete(handle); this.pump(); this.changed();
  }
  dispose() { this.disposed = true; this.reads.clear(); this.wanted.clear(); }
  resume() { this.disposed = false; }
}

export interface ResultObservation {
  readonly value: StoredValue;
  readonly handle: string;
  readonly state: "current" | "updating" | "stale";
  readonly staleReason?: string;
  readonly problem?: string;
  readonly withdrawn?: boolean;
}

/** Presentation only: one public last observation per node, never a calculation input. */
export class ResultObservations {
  private last = new Map<string, { node: WorkspaceNode; value: StoredValue; handle: string }>();
  select(nodes: readonly WorkspaceNode[], held: ReadonlyMap<string, StoredValue>, reads: ReadonlyMap<string, ResultRead>) {
    const wanted = new Set(nodes.map(node => node.id));
    for (const id of this.last.keys()) if (!wanted.has(id)) this.last.delete(id);
    const result = new Map<string, ResultObservation>();
    for (const node of nodes) {
      const value = node.handle ? held.get(node.handle) : undefined;
      const previous = this.last.get(node.id);
      // Evidence of either kind is fixed display data, never a "previous result" for a later run.
      const permitted = !node.private && !node.evidence && !node.doubt && !node.accessWithdrawn
        && ["ready", "running", "pending", "stale"].includes(node.state)
        && node.publication?.state !== "unavailable";
      if (!permitted || previous && (previous.node.command !== node.command
        || JSON.stringify(previous.node.environment) !== JSON.stringify(node.environment))) this.last.delete(node.id);
      const staleReason = node.state === "stale" ? node.staleReason?.message ?? "The result is no longer current." : undefined;
      if (value && node.handle && !node.doubt) {
        result.set(node.id, { value, handle: node.handle, state: staleReason ? "stale" : node.state === "pending" || node.state === "running" || node.publication?.state === "pending" ? "updating" : "current", staleReason });
        if (permitted) this.last.set(node.id, { node, value, handle: node.handle });
      } else {
        const last = this.last.get(node.id);
        const replacing = node.state === "stale" || node.handle !== undefined || node.publication?.state === "pending" || node.state === "pending" || node.state === "running";
        if (last && replacing) result.set(node.id, { value: last.value, handle: last.handle, state: staleReason ? "stale" : "updating", staleReason,
          problem: node.handle ? reads.get(node.handle)?.problem : undefined, withdrawn: node.handle ? reads.get(node.handle)?.withdrawn === true : false });
        else this.last.delete(node.id);
      }
    }
    return result;
  }
}

/** Generation-scoped demand and presentation observations; no retained handle history. */
export function useResults(engine: Engine, generation: string | undefined, handles: readonly string[], nodes: readonly WorkspaceNode[] = []) {
  const [revision, redraw] = useReducer((n: number) => n + 1, 0);
  const demand = useMemo(() => new ResultDemand(handle => engine.fetch(handle), redraw), [engine, generation]);
  const observations = useMemo(() => new ResultObservations(), [engine, generation]);
  const wanted = JSON.stringify([...new Set(handles)]);
  useEffect(() => { demand.resume(); return () => demand.dispose(); }, [demand]);
  useEffect(() => {
    demand.update(generation === undefined ? [] : JSON.parse(wanted) as string[]);
  }, [demand, generation, wanted, revision]);
  const held = new Map<string, StoredValue>();
  // Never return a value no longer requested, even before effect cleanup.
  for (const handle of handles) {
    const value = demand.reads.get(handle)?.value;
    if (value) held.set(handle, value);
  }
  return { held, reads: demand.reads as ReadonlyMap<string, ResultRead>,
    observations: observations.select(nodes, held, demand.reads), retry: (handle: string) => demand.retry(handle) };
}
