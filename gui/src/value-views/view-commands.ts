import { commandRequest } from "@wes/view-sdk";
import type { ViewBinding } from "./view-bindings";
import type { Composer } from "../surface/dataset-management";
export interface CommandReview { readonly source: string; adopt(): void; cancel(): void }
/** A drawing holds one prepare/review. Only the native review invokes adopt. */
export class CommandBridge {
  private epoch = 0;
  private closed = false;
  private pending?: { request: number; abort: AbortController };
  constructor(private readonly send: (message: unknown) => void, private readonly review: (review: CommandReview | undefined) => void) {}
  current() { return this.epoch; }
  reset() { this.finish(false, "changed"); this.epoch++; }
  close() { this.reset(); this.closed = true; }
  private finish(ok: boolean, error?: string) {
    const pending = this.pending; this.pending = undefined;
    this.review(undefined);
    if (!pending) return;
    pending.abort.abort();
    if (!this.closed) this.send({kind: "command-reply", request: pending.request, ok, ...(error ? {error} : {})});
  }
  handle(message: Record<string, unknown>, binding: ViewBinding | undefined, composer: Composer | undefined): void {
    const {request, epoch, template, arguments: arguments_} = message;
    if (!Number.isSafeInteger(request) || Number(request) < 1) throw new Error("Invalid command request");
    const reject = (error: string) => this.send({kind: "command-reply", request, ok: false, error});
    if (this.closed || !binding || !composer?.reviewed) return reject("unavailable");
    if (Object.keys(message).some(k => !["kind", "request", "epoch", "template", "arguments"].includes(k)) || !commandRequest(template, arguments_)) return reject("invalid");
    if (epoch !== this.epoch) return reject("changed");
    if (this.pending) return reject("busy");
    const abort = new AbortController(), pending = {request: Number(request), abort};
    this.pending = pending;
    const captured = binding.engine.captureComposition();
    void binding.engine.prepareViewCommand(binding, String(template), arguments_ as Record<string, unknown>, captured, abort.signal).then(source => {
      if (this.pending !== pending || this.closed) return;
      this.review({source, cancel: () => { if (this.pending === pending) this.finish(false, "cancelled"); }, adopt: () => {
        if (this.pending !== pending || this.closed) return;
        try { composer.reviewed!(source, captured); this.finish(true); }
        catch { this.finish(false, "changed"); }
      }});
    }).catch(() => { if (this.pending === pending) this.finish(false, "failed"); });
  }
}
