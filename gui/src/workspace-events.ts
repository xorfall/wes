import { parseExactJson, stringifyExactJson } from "./exact-json";
/** Read-only observations must not occupy HTTP/1 slots needed by terminal input.
 * Reconnect receives a fresh projection; it never resubmits accepted work.
 */
export class WorkspaceEvents {
  onmessage?: (event: { data: string }) => void;
  onerror?: () => void;
  private socket?: WebSocket;
  private timer?: ReturnType<typeof setTimeout>;
  private stopped = false;
  private delay = 500;
  private readonly url: string;

  constructor(path: string) {
    const url = new URL(path, window.location.href);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    url.pathname = "/events/socket";
    this.url = url.href;
    this.connect();
  }

  private connect(): void {
    if (this.stopped) return;
    let socket: WebSocket;
    try { socket = new WebSocket(this.url); }
    catch { this.reconnect(); return; }
    this.socket = socket;
    const current = () => !this.stopped && this.socket === socket;
    socket.onmessage = event => {
      if (!current() || typeof event.data !== "string") return;
      this.delay = 500;
      try {
        const batch = parseExactJson(event.data) as { sequence: number; events: unknown[] };
        if (!Number.isSafeInteger(batch.sequence) || !Array.isArray(batch.events)) throw new Error("Invalid observation batch");
        for (const item of batch.events) {
          if (!current()) return;
          this.onmessage?.({ data: stringifyExactJson(item) });
        }
        // Receipt follows application of the complete batch, never just socket arrival.
        if (current()) socket.send(`ack:${batch.sequence}`);
      } catch { failed(); }
    };
    const failed = () => {
      if (!current()) return;
      this.socket = undefined;
      socket.onmessage = socket.onerror = socket.onclose = null;
      socket.close();
      this.reconnect();
    };
    socket.onerror = failed;
    socket.onclose = failed;
  }

  private reconnect(): void {
    if (this.stopped) return;
    this.onerror?.();
    if (this.stopped) return;
    this.timer = setTimeout(() => { this.timer = undefined; this.connect(); }, this.delay);
    this.delay = Math.min(5000, this.delay * 2);
  }

  close(): void {
    this.stopped = true;
    clearTimeout(this.timer);
    const socket = this.socket;
    this.socket = undefined;
    if (socket) {
      socket.onmessage = socket.onerror = socket.onclose = null;
      socket.close();
    }
  }
}
