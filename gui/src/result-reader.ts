import { budget } from "./limits/policy";
import { readExactJson, stringifyExactJson } from "./exact-json";
import type { StoredValue } from "./protocol";
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

/** Local transport budget, independent of engine execution and shared server admission. */
function max_concurrent_reads():number { return budget("ui.result.reads"); }
export class ResultReader {
  private active = 0;
  private queue: (() => void)[] = [];
  private pending = new Map<string, Promise<StoredValue>>();
  constructor(private generation: () => string | undefined, private timeout: () => number, private workspace: () => string | undefined = () => undefined) {}

  read(handle: string): Promise<StoredValue> {
    const generation = this.generation();
    const key = stringifyExactJson([generation, handle]);
    const pending = this.pending.get(key);
    if (pending) return pending;
    const current = () => {
      if (this.generation() !== generation) throw new Error("Workspace changed; the result read was discarded.");
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
            const value = await readExactJson(response) as StoredValue;
            current();
            return value;
          }
          const problem = await problemOf(response);
          current();
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
      this.pending.delete(key);
      this.queue.shift()?.();
    });
    this.pending.set(key, reading);
    return reading;
  }
}
