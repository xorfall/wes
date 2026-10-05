import { issueText, locationText } from "./failure-text";
/** One shared recording boundary for engine requests; never receives source/body fields as context. */
import { applicationLog, markProblemReported, type LogContext } from "./application-log";
import { StorageError } from "./storage-error";
import type { Request } from "./protocol";

/** The engine's own statement that a refused submission never started; nothing else proves it. */
export const SUBMISSION_OUTCOME_HEADER = "X-Wes-Submission-Outcome";
export type SubmissionOutcome = "not-started";

export class EngineRefusal extends Error {
  /**
   * `submissionOutcome` is set only from the engine's authoritative header. Without it a refusal
   * says nothing about whether the submission ran, whatever its status or text.
   */
  constructor(readonly status: number, readonly detail: string, operation: string, readonly submissionOutcome?: SubmissionOutcome) {
    super(`Engine refused ${operation} (HTTP ${status}). ${detail}`);
    this.name = "EngineRefusal";
  }
}
export function requestOperation(request: Request): string {
  if (request.request === "submit") return request.repeat ? "Run again" : request.revision_of ? "Change command" : "Submit command";
  return request.request;
}
export function recordEngineFailure(request: Request, error: unknown, context: LogContext, timedOut = false) {
  markProblemReported(error);
  const refused = error instanceof EngineRefusal;
  const storage = error instanceof StorageError;
  const sensitive = request.request === "secret" || request.request === "input";
  const operation = requestOperation(request);
  applicationLog.add({ ...context, level: "error", operation,
    cell: request.request === "submit" ? request.repeat ?? request.revision_of ?? request.cell
      : "cell" in request ? request.cell : "origin" in request ? request.origin : undefined,
    node: "node" in request ? request.node : undefined,
    code: refused ? `ENGINE_HTTP_${error.status}` : timedOut ? "ENGINE_TIMEOUT" : storage ? "ENGINE_STORAGE_REFUSED" : "ENGINE_REQUEST_FAILED",
    source: refused || storage ? "Engine" : "Browser → engine",
    message: refused ? `Engine refused ${operation} (HTTP ${error.status}).`
      : timedOut ? `${operation} did not return before the client deadline; completion is unknown.`
      : storage ? `${operation} was refused by the engine.` : `${operation} could not be confirmed.`,
    notice: refused ? sensitive ? `${operation} refused. See Logs.` : `${operation} refused. ${error.detail}` : undefined,
    detail: sensitive ? "Request and response details withheld for secret/input operations."
      : refused ? error.detail : error instanceof Error ? error.message : "No further error detail was provided.",
    next: refused || storage ? request.request === "submit" && request.repeat
      ? "Inspect the original cell and its declaration. Run again requires a successfully declared command with unchanged definition; submit edited source as new work."
      : "Review the engine's reason before trying again. No automatic retry was made."
      : "Inspect the workspace before retrying; losing the reply does not prove the operation was rejected. No automatic retry was made.",
  });
}

/** Failure IDs are stable across event replay; only new execution failures create log entries. */
const recordedFailures = new Set<string>();
export function recordExecutionFailure(event: Extract<import("./protocol").Event, { event: "failed" }>, context: LogContext) {
  const key = JSON.stringify([context.workspace, context.generation, event.error.id]);
  if (recordedFailures.has(key)) return;
  recordedFailures.add(key);
  if (recordedFailures.size > 2048) recordedFailures.delete(recordedFailures.values().next().value!);
  applicationLog.add({ ...context, node: event.node, source: "Execution", operation: "Run", level: "error",
    code: event.error.code, message: event.error.message, notice: event.error.message, detail: [...(event.error.locations ?? []).map(locationText), ...event.error.issues.map(issueText)].join("\n") || undefined });
}
