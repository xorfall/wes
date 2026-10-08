import { budget } from "./limits/policy";
import { readExactJson, stringifyExactJson } from "./exact-json";
import type { StoredValue } from "./protocol";
import { withValidMeta } from "./value-meta";
import { workspaceHeaders } from "./workspace-binding";

export interface ValueReadProblem {
  code: string; message: string; retryable: boolean; retryAfterMs?: number | null;
  context: { operation: "read-value"; handle: string | null };
}
export class ResultReadError extends Error {
  constructor(readonly status: number, readonly problem?: ValueReadProblem) {
    super(problem ? `${problem.code}: ${problem.message}` : `Result read failed (HTTP ${status}); the server supplied no structured explanation.`);
    this.name = "ResultReadError";
  }
}
async function problemOf(response: Response): Promise<ValueReadProblem | undefined> {
  let body: { error?: Partial<ValueReadProblem> };
  try { body = await readExactJson(response); } catch { return undefined; }
  const error = body?.error;
  if (!error || typeof error.code !== "string" || typeof error.message !== "string"
    || typeof error.retryable !== "boolean" || error.context?.operation !== "read-value"
    || (error.context.handle !== null && typeof error.context.handle !== "string")) return undefined;
  return error as ValueReadProblem;
}

/** Withdrawn handles remembered by one client; older ones fall back to the server's own refusal. */
const MAX_WITHDRAWN = 4096;
/** A read of a result whose access was withdrawn: says nothing about the value. */
export class ResultWithdrawnError extends Error {
  constructor() { super("Result · Access withdrawn"); this.name = "ResultWithdrawnError"; }
}

/**
 * Whether a structured refusal is the precise access withdrawal of this handle: `403
 * VALUE_ACCESS_WITHDRAWN` or `410 VALUE_PRIVATE_UNAVAILABLE`. Missing (404), busy and server
 * failures are not withdrawals.
 */
function accessWithdrawn(status: number, problem: ValueReadProblem, handle: string): boolean {
  if (problem.retryable || (problem.context.handle !== null && problem.context.handle !== handle)) return false;
  return (status === 403 && problem.code === "VALUE_ACCESS_WITHDRAWN") || (status === 410 && problem.code === "VALUE_PRIVATE_UNAVAILABLE");
}

/** Local transport budget, independent of engine execution and shared server admission. */
function max_concurrent_reads():number { return budget("ui.result.reads"); }
export class ResultReader {
  private active = 0;
  private queue: (() => void)[] = [];
  private pending = new Map<string, Promise<StoredValue>>();
  /**
   * Handles whose access the engine withdrew. Bounded and kept across sessions: a withdrawn handle
   * is never read again, and a reply already on its way is dropped, never returned.
   */
  private withdrawn = new Set<string>();
  /**
   * One fence per read in flight, by handle. Withdrawal closes the fences of that handle's reads, so
   * each of those requests stays refused to its end even after the bounded tombstone above evicts
   * the handle. A fence lives exactly as long as its request.
   */
  private fences = new Map<string, Set<{ closed: boolean }>>();
  /** Refuses `handle` from now on and drops any read of it in flight. */
  withdraw(handle: string): void {
    this.withdrawn.delete(handle);
    this.withdrawn.add(handle);
    if (this.withdrawn.size > MAX_WITHDRAWN) this.withdrawn.delete(this.withdrawn.values().next().value!);
    for (const fence of this.fences.get(handle) ?? []) fence.closed = true;
    for (const key of [...this.pending.keys()]) if (JSON.parse(key)[1] === handle) this.pending.delete(key);
  }
  isWithdrawn(handle: string): boolean { return this.withdrawn.has(handle); }
  /** Every handle withdrawn so far, newest last. */
  withdrawnHandles(): readonly string[] { return [...this.withdrawn]; }
  /**
   * `withdrawn` hears every handle the server refused as withdrawn, with the session the read was
   * for, so the shared gate can clear what other surfaces still hold of it.
   */
  constructor(private generation: () => string | undefined, private timeout: () => number, private workspace: () => string | undefined = () => undefined,
    private refused: (handle: string, generation: string | undefined) => void = () => undefined) {}

  read(handle: string): Promise<StoredValue> {
    if (this.withdrawn.has(handle)) return Promise.reject(new ResultWithdrawnError());
    const generation = this.generation();
    const key = stringifyExactJson([generation, handle]);
    const pending = this.pending.get(key);
    if (pending) return pending;
    const fence = { closed: false };
    const fences = this.fences.get(handle) ?? new Set();
    fences.add(fence);
    this.fences.set(handle, fences);
    const current = () => {
      if (this.generation() !== generation) throw new Error("Workspace changed; the result read was discarded.");
      if (fence.closed || this.withdrawn.has(handle)) throw new ResultWithdrawnError();
    };
    const admitted = new Promise<void>(resolve => {
      const start = () => { this.active++; resolve(); };
      if (this.active < max_concurrent_reads()) start(); else this.queue.push(start);
    });
    const reading = admitted.then(async () => {
      // Retry only read-admission refusal. Never submit a command or retry 404/410.
      const delays = [150, 400, 900];
      for (let attempt = 0; ; attempt++) {
        current();
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort(), this.timeout());
        try {
          const response = await window.fetch(`/values/${encodeURIComponent(handle)}`, { signal: controller.signal, headers: { ...workspaceHeaders(this.workspace()), ...(generation === undefined ? {} : { "X-Wes-Session": generation }) } });
          current();
          if (response.ok) {
            const value = withValidMeta(await readExactJson(response) as StoredValue);
            current();
            return value;
          }
          const problem = await problemOf(response);
          current();
          // The server's precise withdrawal: refuse this handle everywhere, never retry, draw nothing.
          if (problem && accessWithdrawn(response.status, problem, handle)) {
            this.withdraw(handle);
            this.refused(handle, generation);
            throw new ResultWithdrawnError();
          }
          if (response.status === 503 && problem?.code === "VALUE_READ_BUSY" && problem.retryable && attempt < delays.length) {
            clearTimeout(timer);
            const guidance = typeof problem.retryAfterMs === "number" && Number.isFinite(problem.retryAfterMs) ? problem.retryAfterMs : 0;
            await new Promise(resolve => setTimeout(resolve, Math.min(2000, Math.max(delays[attempt]!, guidance))));
            continue;
          }
          throw new ResultReadError(response.status, problem);
        } catch (error) {
          current();
          if (controller.signal.aborted) throw new Error("Reading the result timed out. This does not mean the command failed or the result was deleted.");
          throw error;
        } finally {
          clearTimeout(timer);
          controller.abort(); // Release even an unread response after generation change.
        }
      }
    }).finally(() => {
      this.active--;
      fences.delete(fence);
      if (fences.size === 0 && this.fences.get(handle) === fences) this.fences.delete(handle);
      if (this.pending.get(key) === reading) this.pending.delete(key);
      this.queue.shift()?.();
    });
    this.pending.set(key, reading);
    return reading;
  }
}
