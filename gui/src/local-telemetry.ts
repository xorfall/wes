/** Closed-schema, local diagnostics. Never accepts Error, message, source, URL or result data. */
export type Mode = "off" | "basic" | "diagnostic";
export interface DiagnosticsStatus {
  schema: 1; generation: string; mode: Mode; saved_mode: "off" | "basic"; forced: boolean;
  remaining_ms: number; dropped: number; suppressed: number; write_errors: number;
  recent_count: number; previous_unclean: boolean; writer_stopping: boolean;
  metrics: { operations: { operation: string; outcomes: [string, number][]; sum_us: number; buckets: number[] }[] };
}
type ClientEvent = { kind: "error" | "rejection" | "render" | "reconnect" } | { kind: "submit"; elapsed_us: number; outcome: "ok" | "error" };
export type DiagnosticsAction = { action: "set"; mode: "off" | "basic" } | { action: "start" | "stop" | "clear" | "export" };
let current: DiagnosticsStatus | undefined;
let initialized = false;
let batch: ClientEvent[] = [];
let timer: ReturnType<typeof setTimeout> | undefined;
let expiry: ReturnType<typeof setTimeout> | undefined;
let request: AbortController | undefined;
let channel: BroadcastChannel | undefined;
let revision = 0;
const listeners = new Set<() => void>();
export function diagnosticsStatus() { return current; }
export function subscribeDiagnostics(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; }
function publish(status: DiagnosticsStatus) {
  if (current?.generation !== status.generation || current?.mode !== status.mode) {
    revision++; batch = []; if (timer) clearTimeout(timer); timer = undefined; request?.abort();
  }
  current = status;
  if (expiry) clearTimeout(expiry);
  expiry = status.mode === "diagnostic" ? setTimeout(() => { void refreshDiagnostics().catch(() => {}); }, Math.max(1, status.remaining_ms) + 50) : undefined;
  listeners.forEach(listener => listener());
}
export async function refreshDiagnostics(): Promise<void> {
  const observed = revision;
  const response = await fetch("/diagnostics", { cache: "no-store", signal: AbortSignal.timeout(3000) });
  if (!response.ok) throw new Error("Local diagnostics are unavailable for this process.");
  const status = await response.json() as DiagnosticsStatus;
  if (observed !== revision) return;
  if (status.schema !== 1 || !["off", "basic", "diagnostic"].includes(status.mode) || typeof status.generation !== "string") throw new Error("Unsupported diagnostics response.");
  publish(status);
}
export async function controlDiagnostics(action: DiagnosticsAction): Promise<void> {
  const status = current;
  if (!status) throw new Error("Refresh diagnostics before changing its settings.");
  const response = await fetch("/diagnostics", {
    method: "POST", headers: { "Content-Type": "application/json", "X-Wes-Session": status.generation },
    body: JSON.stringify(action), signal: AbortSignal.timeout(15000),
  });
  if (!response.ok) throw new Error("Diagnostics change was not confirmed. Refresh its status before trying again.");
  if (action.action === "export") {
    const blob = await response.blob();
    const url = URL.createObjectURL(blob); const link = document.createElement("a");
    link.href = url; link.download = "wes-diagnostics.json"; link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  } else {
    revision++; publish(await response.json() as DiagnosticsStatus);
    channel?.postMessage("changed");
  }
}
export function clientDiagnostic(kind: "error" | "rejection" | "render" | "reconnect") { enqueue({ kind }); }
function enqueue(event: ClientEvent) {
  if (!initialized || !current || current.mode === "off" || batch.length >= 16) return;
  batch.push(event);
  if (timer === undefined) timer = setTimeout(() => { timer = undefined; void flush(); }, 1000);
}
async function flush() {
  const status = current;
  if (!status || status.mode === "off") { batch = []; return; }
  const events = batch; batch = [];
  if (!events.length || request) return; // Never queue behind a stalled diagnostics transport.
  const pending = new AbortController(); request = pending;
  try {
    await fetch("/diagnostics/client", { method: "POST", headers: { "Content-Type": "application/json", "X-Wes-Session": status.generation }, body: JSON.stringify({ events }), signal: AbortSignal.any([pending.signal, AbortSignal.timeout(3000)]) });
  } catch { /* Telemetry failure never recursively reports itself or changes business results. */ }
  finally { if (request === pending) request = undefined; }
}
export function timeClientSubmit(): (outcome: "ok" | "error") => void {
  if (!initialized || !current || current.mode === "off") return () => {};
  const start = performance.now(), at = revision;
  return outcome => { if (at === revision) enqueue({ kind: "submit", elapsed_us: Math.min(3_600_000_000, Math.max(0, Math.round((performance.now() - start) * 1000))), outcome }); };
}
export function diagnosticsSession(generation: string) {
  if (initialized && current?.generation !== generation) {
    revision++; batch = []; request?.abort(); current = undefined;
    void refreshDiagnostics().catch(() => {});
  }
}
/** Called once by the actual client entry, never by component tests or embedded Engine instances. */
export function initializeDiagnostics(): () => void {
  if (initialized) return () => {};
  initialized = true;
  const error = () => clientDiagnostic("error"), rejection = () => clientDiagnostic("rejection");
  const visible = () => { if (document.visibilityState === "visible") void refreshDiagnostics().catch(() => {}); };
  window.addEventListener("error", error); window.addEventListener("unhandledrejection", rejection);
  document.addEventListener("visibilitychange", visible);
  if (typeof BroadcastChannel !== "undefined") { channel = new BroadcastChannel("wes-diagnostics"); channel.onmessage = () => { void refreshDiagnostics().catch(() => {}); }; }
  void refreshDiagnostics().catch(() => {});
  return () => {
    initialized = false; revision++; current = undefined; batch = [];
    if (timer) clearTimeout(timer); if (expiry) clearTimeout(expiry); timer = undefined; expiry = undefined;
    request?.abort(); request = undefined; channel?.close(); channel = undefined;
    window.removeEventListener("error", error); window.removeEventListener("unhandledrejection", rejection); document.removeEventListener("visibilitychange", visible);
  };
}
