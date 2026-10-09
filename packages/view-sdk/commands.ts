/** Prepare a typed workspace template call for native host review and the ordinary prompt.
 * This capability never submits, runs a provider, or confirms an external effect. */
export interface ViewCommands {
  prepare(template: string, arguments_: Readonly<Record<string, unknown>>): Promise<void>;
}
export type CommandErrorCode = "unavailable" | "busy" | "invalid" | "changed" | "cancelled" | "failed";
export class ViewCommandError extends Error {
  constructor(readonly code: CommandErrorCode) { super(`Command preparation ${code}`); this.name = "ViewCommandError"; }
}
export function commandRequest(template: unknown, arguments_: unknown): boolean {
  if (typeof template !== "string" || !/^[\p{L}_][\p{L}\p{Nd}_]*$/u.test(template) || template.length > 128) return false;
  if (!arguments_ || typeof arguments_ !== "object" || Array.isArray(arguments_) || Object.keys(arguments_).length > 32) return false;
  let nodes = 0;
  const valid = (value: unknown, depth: number): boolean => {
    if (++nodes > 1024 || depth > 16) return false;
    if (typeof value === "string" || typeof value === "boolean") return true;
    if (typeof value === "number") return Number.isFinite(value) && (!Number.isInteger(value) || Number.isSafeInteger(value));
    if (Array.isArray(value)) return value.every(v => valid(v, depth + 1));
    if (value && typeof value === "object" && Object.getPrototypeOf(value) === Object.prototype) return Object.values(value).every(v => valid(v, depth + 1));
    return false;
  };
  try { return valid(arguments_, 0) && JSON.stringify(arguments_).length <= 16 * 1024; } catch { return false; }
}
export class FrameCommands implements ViewCommands {
  private next = 0;
  private pending?: { request: number; resolve: () => void; reject: (error: ViewCommandError) => void };
  constructor(private readonly send: (message: unknown) => void, private readonly epoch: () => number) {}
  prepare(template: string, arguments_: Readonly<Record<string, unknown>>): Promise<void> {
    if (!commandRequest(template, arguments_)) return Promise.reject(new ViewCommandError("invalid"));
    if (this.pending) return Promise.reject(new ViewCommandError("busy"));
    const request = ++this.next;
    return new Promise((resolve, reject) => {
      this.pending = { request, resolve, reject };
      try { this.send({ kind: "command-prepare", request, epoch: this.epoch(), template, arguments: arguments_ }); }
      catch { this.cancel("failed"); }
    });
  }
  settle(message: Record<string, unknown>): void {
    if (!this.pending || message.request !== this.pending.request) return;
    const pending = this.pending; this.pending = undefined;
    if (message.ok === true) pending.resolve();
    else pending.reject(new ViewCommandError(["unavailable", "busy", "invalid", "changed", "cancelled"].includes(String(message.error)) ? message.error as CommandErrorCode : "failed"));
  }
  cancel(code: CommandErrorCode = "changed"): void {
    const pending = this.pending; this.pending = undefined; pending?.reject(new ViewCommandError(code));
  }
}
