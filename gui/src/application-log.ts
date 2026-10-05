/** Diagnostics for this application window, shared by its retained workspace controllers.
 * Deliberately separate from engine values and terminal scrollback; never record request bodies.
 */
export interface LogContext { workspace?: string; pane?: string; terminal?: string; generation?: string; cell?: string; node?: string }
export interface LogMessage extends LogContext {
  level: "error" | "warning" | "info";
  code: string;
  source: string;
  operation: string;
  message: string;
  detail?: string;
  /** Optional concise notice when the detail is a technical trace rather than a short cause. */
  notice?: string;
  next?: string;
}
export interface LogRecord extends LogMessage { id: number; first: string; time: string; count: number }
export class ApplicationLog {
  private records: readonly LogRecord[] = [];
  private listeners = new Set<() => void>();
  private sequence = 0;
  constructor(readonly limit = 500) {
    if (!Number.isInteger(limit) || limit < 1) throw new Error("Log limit must be a positive integer.");
  }
  snapshot = () => this.records;
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  private publish(records: readonly LogRecord[]) { this.records = records; this.listeners.forEach(listener => listener()); }
  clear = () => this.publish([]);
  add(message: LogMessage) {
    const bounded = Object.fromEntries(Object.entries(message).filter(([, value]) => value !== undefined)
      .map(([key, value]) => {
        const limit = key === "detail" ? 32768 : 2000;
        const suffix = "… [truncated; inspect the cell for full details]";
        return [key, value.length <= limit ? value : value.slice(0, limit - suffix.length) + suffix];
      })) as unknown as LogMessage;
    const previous = this.records.at(-1);
    const time = new Date().toISOString();
    const { id: _id, first: _first, time: _time, count: _count, ...last } = previous ?? {};
    if (previous && JSON.stringify(last) === JSON.stringify(bounded)) {
      this.publish([...this.records.slice(0, -1), { ...previous, time, count: previous.count + 1 }]);
    } else this.publish([...this.records, { ...bounded, id: ++this.sequence, first: time, time, count: 1 }].slice(-this.limit));
  }
}
export const applicationLog = new ApplicationLog();
const reportedProblems = new WeakSet<object>();
export function markProblemReported(problem: unknown) {
  if (problem !== null && typeof problem === "object") reportedProblems.add(problem);
}
/** Surface catches preserve error identity so a transport diagnostic is recorded only once. */
export function reportApplicationProblem(problem: unknown, context: LogContext) {
  if (problem === undefined || problem === null || problem === "") return;
  if (typeof problem === "object" && reportedProblems.has(problem)) return;
  applicationLog.add({ ...context, source: "Workspace", operation: "Action", level: "error",
    code: "WORKSPACE_ACTION", message: problem instanceof Error ? problem.message : String(problem) });
  markProblemReported(problem);
}
export function formatLogs(records: readonly LogRecord[]): string {
  return records.map(({ first, time, count, level, code, source, operation, workspace, pane, terminal, generation, cell, node, message, detail, next }) =>
    [`${time} ${level.toUpperCase()} ${code} · ${source} · ${operation}`,
      `Workspace: ${workspace ?? "unknown"} · Pane: ${pane ?? "—"} · Terminal: ${terminal ?? "not created"} · Session: ${generation ?? "—"}`,
      (cell || node) && `Cell: ${cell ?? "—"} · Node: ${node ?? "—"}`,
      message, detail && `Cause: ${detail}`, next && `Next: ${next}`, count > 1 && `Repeated ${count} times since ${first}`].filter(Boolean).join("\n")
  ).join("\n\n");
}
