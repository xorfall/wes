import { TerminalUnavailable } from "./terminal-errors";
import type { UiRequest } from "./assistant-ui";
export interface CommandRequest { id: string; text: string; environment?: string | null }
export interface RecoveryCursor { id: string | null; offset: number }
export interface RecoveryPage { id: string; offset: number; next: number; total: number; cols: number; rows: number }
export interface DisplayRead { epoch?: number; recovery?: RecoveryCursor }
export interface TerminalFrame { recovery_error?: string | null; epoch: number; recovery?: RecoveryPage | null; command?: CommandRequest | null; ui?: UiRequest | null; start: number; next: number; data: string; exit: number | null; problem: string | null; closed: boolean }
interface OutputSink {
  poll(cursor: number, signal: AbortSignal, read: DisplayRead): Promise<TerminalFrame>;
  write(bytes: Uint8Array): Promise<void>;
  restore(page: RecoveryPage): void;
  restored(): void;
  ui(request: UiRequest): Promise<void>;
  command?(request: CommandRequest): Promise<void>;
  ended(exit: number | null): void;
  problem(message: string): void;
  refused(message: string): void;
  connection(error?: unknown, operation?: string): void;
  unavailable(message: string): void;
}
class InvalidOutput extends Error {}
function integer(value: number, min = 0, max = Number.MAX_SAFE_INTEGER): boolean { return Number.isSafeInteger(value) && value >= min && value <= max; }
function retryPause(signal: AbortSignal): Promise<void> {
  return new Promise(resolve => {
    const finish = () => { clearTimeout(timer); signal.removeEventListener("abort", finish); resolve(); };
    const timer = setTimeout(finish, 250);
    signal.addEventListener("abort", finish, { once: true });
    if (signal.aborted) finish();
  });
}
/** One frame in flight, then wait for xterm's parser callback before fetching more.
 * Successful reads have no polling timer; the server waits only when output is empty.
 */
export async function terminalOutput(sink: OutputSink, signal: AbortSignal): Promise<void> {
  let cursor = 0;
  let epoch: number | undefined;
  let recovery: RecoveryCursor | undefined = { id: null, offset: 0 };
  let frozen: { id: string; cursor: number; epoch: number; total: number; cols: number; rows: number } | undefined;
  while (!signal.aborted) {
    let operation = "poll";
    try {
      const frame = await sink.poll(cursor, signal, { epoch, recovery });
      if (signal.aborted) return;
      if (frame.recovery_error) { sink.refused(frame.recovery_error); return; }
      operation = "decode output";
      let bytes: Uint8Array;
      try { bytes = Uint8Array.from(atob(frame.data), c => c.charCodeAt(0)); }
      catch { throw new InvalidOutput("Invalid terminal output encoding."); }
      if (!integer(frame.start) || !integer(frame.next) || bytes.length > 65536 || !integer(frame.epoch)) throw new InvalidOutput("Invalid terminal output position.");
      const page = frame.recovery;
      if (page) {
        if (typeof page.id !== "string" || !/^[0-9a-f-]{36}$/.test(page.id) || !integer(page.offset) || !integer(page.next) || !integer(page.total, 1, 256 * 1024 * 1024) || page.next > page.total || page.next - page.offset !== bytes.length || page.next === page.offset || !integer(page.cols, 2, 500) || !integer(page.rows, 2, 300) || frame.start !== frame.next || frame.epoch === undefined) throw new InvalidOutput("Invalid terminal recovery page.");
        if (!frozen || page.id !== frozen.id) {
          if (page.offset !== 0) throw new InvalidOutput("Terminal recovery started in the middle of a screen.");
          frozen = { id: page.id, cursor: frame.next, epoch: frame.epoch, total: page.total, cols: page.cols, rows: page.rows };
          sink.restore(page);
        } else if (page.offset !== recovery?.offset || frame.next !== frozen.cursor || frame.epoch !== frozen.epoch || page.total !== frozen.total || page.cols !== frozen.cols || page.rows !== frozen.rows) throw new InvalidOutput("Terminal recovery changed while being read.");
      } else if (frame.start !== cursor || frame.next !== cursor + bytes.length || (epoch !== undefined && frame.epoch !== epoch) || frozen) throw new InvalidOutput("Terminal output has a gap without a recovery screen.");
      operation = "render output";
      if (bytes.length) await sink.write(bytes);
      if (signal.aborted) return;
      if (page) {
        if (page.next < page.total) { recovery = { id: page.id, offset: page.next }; continue; }
        sink.restored(); frozen = undefined;
      }
      // A UI acknowledgement can fail independently. Do not replay already parsed bytes; a
      // repeated request is answered from the broker's reply cache, never performed again.
      cursor = frame.next;
      epoch = frame.epoch;
      recovery = undefined;
      operation = "pane command acknowledgement";
      if (frame.command) await sink.command?.(frame.command);
      if (signal.aborted) return;
      operation = "UI acknowledgement";
      if (frame.ui) await sink.ui(frame.ui);
      if (signal.aborted) return;
      sink.connection();
      if (frame.problem) sink.problem(frame.problem);
      if (frame.closed && !page && bytes.length === 0) { sink.ended(frame.exit); return; }
    } catch (error) {
      if (signal.aborted) return;
      if (error instanceof TerminalUnavailable) { sink.unavailable(error.message); return; }
      if (error instanceof InvalidOutput) { sink.refused(error.message); return; }
      sink.connection(error, operation);
      // Reads are replayable; input writes are deliberately outside this retry loop.
      await retryPause(signal);
    }
  }
}
