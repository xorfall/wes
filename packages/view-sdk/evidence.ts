/** Independent typed inputs of explicitly connected child slots. No source is started. */
export interface EvidenceInput {
  readonly available: boolean;
  /** False for missing input, linked fields, running query or delivery cautions. */
  readonly complete: boolean;
  /** Wes display envelope, retaining the independently captured type and native descriptors. */
  readonly input: {readonly type: unknown; readonly data: unknown} | null;
  readonly reference: unknown;
  readonly delivery: "finite" | "window";
  readonly problem: string | null;
  readonly cautions: readonly string[];
  readonly queryRunning: boolean;
  readonly linkedFields: readonly string[];
  readonly consistency: string;
}
export interface ViewEvidence { read(slot: string, ordinal?: number): Promise<EvidenceInput> }
export class ViewEvidenceError extends Error {
  constructor(readonly code: string) { super(`Evidence read ${code}`); this.name = "ViewEvidenceError"; }
}
export function evidenceRequest(slot: unknown, ordinal: unknown): boolean {
  return typeof slot === "string" && /^[A-Za-z][A-Za-z0-9_]{0,127}$/.test(slot) && Number.isSafeInteger(ordinal) && Number(ordinal) >= 0 && Number(ordinal) < 32;
}
export class FrameEvidence implements ViewEvidence {
  private next = 0;
  private pending = new Map<number, {resolve(value: EvidenceInput): void; reject(error: Error): void}>();
  constructor(private readonly send: (message: unknown) => void, private readonly epoch: () => number) {}
  read(slot: string, ordinal = 0): Promise<EvidenceInput> {
    if (!evidenceRequest(slot, ordinal)) return Promise.reject(new ViewEvidenceError("invalid"));
    if (this.pending.size >= 2) return Promise.reject(new ViewEvidenceError("busy"));
    const request = ++this.next;
    return new Promise((resolve, reject) => {
      this.pending.set(request, {resolve, reject});
      try { this.send({kind:"evidence-read", request, epoch:this.epoch(), slot, ordinal}); }
      catch { this.pending.delete(request); reject(new ViewEvidenceError("failed")); }
    });
  }
  settle(message: Record<string, unknown>): void {
    const request = Number(message.request), pending = this.pending.get(request);
    if (!pending) return;
    this.pending.delete(request);
    if (message.ok === true) pending.resolve(message.result as EvidenceInput);
    else pending.reject(new ViewEvidenceError(["invalid","busy","changed","unavailable","limit"].includes(String(message.error)) ? String(message.error) : "failed"));
  }
  cancel(): void { for (const pending of this.pending.values()) pending.reject(new ViewEvidenceError("changed")); this.pending.clear(); }
}
