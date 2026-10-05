import { applicationLog, type ApplicationLog, type LogContext } from "./application-log";
import { TerminalRequestError, TerminalUnavailable } from "./terminal-errors";

/** Classify observations, not guesses about an underlying network or shell failure. */
export class TerminalDiagnostics {
  private transient = new Map<string, string>();
  constructor(private readonly log: ApplicationLog = applicationLog) {}
  reset() { this.transient.clear(); }
  error(operation: string, error: unknown, context: LogContext, next: string, level: "error" | "warning" = "error") {
    const classified = error instanceof TerminalRequestError;
    this.log.add({ ...context, level,
      code: classified ? error.code : error instanceof TerminalUnavailable ? "TERM_UNAVAILABLE" : "TERM_OPERATION_FAILED",
      source: classified ? error.source : operation === "server report" || error instanceof TerminalUnavailable ? "Terminal server" : "Terminal client",
      operation: classified ? error.operation : operation,
      message: classified || error instanceof TerminalUnavailable ? error.message : `Terminal ${operation} failed.`,
      detail: classified ? error.detail : error instanceof Error ? error.message : String(error), next });
  }
  connection(channel: "output" | "resize", error: unknown | undefined, operation: string, context: LogContext) {
    if (error !== undefined) {
      const key = JSON.stringify([operation, error instanceof Error ? error.name : "", error instanceof Error ? error.message : String(error)]);
      // An outage is one observation, even when another pane interleaves log events.
      if (this.transient.get(channel) !== key) this.error(operation, error, context,
        channel === "output" ? "Output processing will retry automatically. Shell input is not replayed." : "Size will be sent again on the next terminal resize.", "warning");
      this.transient.set(channel, key);
    } else if (this.transient.delete(channel)) {
      this.log.add({ ...context, level: "info", code: "TERM_RECOVERED", source: "Terminal client", operation,
        message: channel === "output" ? "Terminal output processing recovered." : "Terminal size update succeeded.",
        next: "No action needed for this recovery. Other errors remain in the log." });
    }
  }
}
