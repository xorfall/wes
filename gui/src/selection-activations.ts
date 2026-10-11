import type { EnvironmentContext, Event, SelectionInput } from "./protocol";

export interface SelectionCellUpdate {
  readonly cell: string;
  readonly source: string;
  readonly attempt: string;
  readonly origin?: string;
  readonly state: "waiting" | "running" | "unanswered" | "answered";
  readonly error?: string;
}
interface Binding {
  readonly cell: string;
  readonly text: string;
  readonly source: string;
  readonly context: EnvironmentContext | undefined;
  origin?: string;
  nextFirst?: string;
  creating?: string;
  latest?: SelectionInput;
}
/** Presentation intent only. The engine owns type checking, cancellation and execution. */
export class SelectionActivations {
  private bindings = new Map<string, Binding>();
  private generation: string | undefined;
  private listeners = new Set<(update: SelectionCellUpdate) => void>();
  constructor(private readonly send: (attempt: string, text: string, selection: SelectionInput, context: EnvironmentContext | undefined, origin?: string) => Promise<void>,
    private readonly identity: () => string = () => crypto.randomUUID()) {}
  subscribe = (listener: (update: SelectionCellUpdate) => void): (() => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  private emit(update: SelectionCellUpdate) { for (const listener of this.listeners) listener(update); }
  wait(cell: string, text: string, source: string, context: EnvironmentContext | undefined) {
    this.bindings.set(cell, { cell, text, source, context });
    this.emit({ cell, source, attempt: cell, state: "waiting" });
  }
  hasSource(source: string): boolean { return [...this.bindings.values()].some(binding => binding.source === source); }
  reset() { this.bindings.clear(); this.generation = undefined; }
  cancel(cell: string): boolean {
    const binding = this.bindings.get(cell);
    if (!binding) return false;
    binding.latest = undefined;
    if (!binding.origin && !binding.creating) { this.bindings.delete(cell); this.emit({cell, source:binding.source, attempt:cell, state:"answered", error:"Waiting selection command cancelled."}); return true; }
    return false;
  }
  activate(selection: SelectionInput) {
    for (const binding of this.bindings.values()) {
      if (binding.source !== selection.source) continue;
      if (binding.creating) { binding.latest = selection; continue; }
      this.run(binding, selection);
    }
  }
  private run(binding: Binding, selection: SelectionInput) {
    const attempt = binding.origin ? this.identity() : binding.nextFirst ?? binding.cell;
    if (!binding.origin) binding.creating = attempt;
    this.emit({ cell: binding.cell, source: binding.source, attempt, ...(binding.origin ? { origin: binding.origin } : {}), state: "running" });
    void this.send(attempt, binding.text, selection, binding.context, binding.origin).catch((error: Error) => {
      // A missing reply cannot authorize another creation. Only a planned acknowledgement can.
      this.emit({ cell: binding.cell, source: binding.source, attempt, state: "unanswered", error: error.message });
    });
  }
  observe(event: Event) {
    if (event.event === "workspace-closed") { this.reset(); return; }
    if (event.event === "session") {
      if (this.generation !== event.generation) this.reset();
      this.generation = event.generation;
      for (const [cell, binding] of this.bindings) {
        // A reconnect preserves local drafts and unresolved creation identities,
        // but never carries queued execution intent across a lost connection.
        binding.latest = undefined;
        if (binding.origin && event.cells && !event.cells.includes(binding.origin)) this.bindings.delete(cell);
      }
      return;
    }
    if (event.event === "work-retired") {
      for(const [cell,binding] of this.bindings) if(event.cells.includes(binding.origin ?? cell))this.bindings.delete(cell);
      return;
    }
    if (event.event !== "planned") return;
    const alreadyOwned = this.bindings.has(event.cell) || [...this.bindings.values()].some(binding => binding.creating === event.cell || binding.origin === event.cell);
    if (event.selectionSource && event.nodes.length === 1 && !event.repeatOf && !event.revisionOf && !event.failure && !event.diagnostics?.some(d => d.severity === "error") && !alreadyOwned) {
      this.bindings.set(event.cell, { cell: event.cell, text: event.text, source: event.selectionSource, origin: event.cell, context: undefined });
    }
    for (const binding of this.bindings.values()) {
      if (binding.creating !== event.cell) continue;
      if (event.failure || event.nodes.length !== 1 || event.diagnostics?.some(d => d.severity === "error")) {
        // Explicit refusal also requires a new user decision, never automatic replay.
        binding.latest = undefined;
        binding.creating = undefined;
        binding.nextFirst = this.identity();
        continue;
      }
      binding.origin = event.cell;
      binding.creating = undefined;
      const next = binding.latest; binding.latest = undefined;
      if (next) queueMicrotask(() => { if (this.bindings.get(binding.cell) === binding) this.run(binding, next); });
    }
  }
}
