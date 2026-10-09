import {evidenceRequest} from "@wes/view-sdk";
import type {ViewBinding} from "./view-bindings";
/** Per-drawing bounded reads. Addresses and session identities come only from the host binding. */
export class EvidenceBridge {
  private epoch = 0;
  private closed = false;
  private pending = new Map<number, AbortController>();
  constructor(private readonly send: (message: unknown) => void) {}
  current() { return this.epoch; }
  reset() { for (const [request, controller] of this.pending) {controller.abort(); this.send({kind:"evidence-reply",request,ok:false,error:"changed"});} this.pending.clear(); this.epoch++; }
  close() { this.reset(); this.closed = true; }
  handle(message: Record<string, unknown>, binding?: ViewBinding): void {
    const {request, epoch, slot, ordinal} = message;
    const fail = (error: string) => this.send({kind:"evidence-reply", request, ok:false, error});
    if (!Number.isSafeInteger(request) || Number(request) < 1 || !evidenceRequest(slot, ordinal) || Object.keys(message).some(k => !["kind","request","epoch","slot","ordinal"].includes(k))) return fail("invalid");
    if (this.closed || !binding?.authorityEpoch) return fail("unavailable");
    if (epoch !== this.epoch) return fail("changed");
    if (this.pending.size >= 2 || this.pending.has(Number(request))) return fail("busy");
    const abort = new AbortController(); this.pending.set(Number(request), abort);
    void binding.engine.readViewEvidence(binding, String(slot), Number(ordinal), abort.signal).then(result => {
      if (this.pending.get(Number(request)) !== abort || abort.signal.aborted) return;
      this.pending.delete(Number(request)); this.send({kind:"evidence-reply", request, ok:true, result});
    }).catch(() => {if (this.pending.get(Number(request)) !== abort) return; this.pending.delete(Number(request)); fail("failed");});
  }
}
