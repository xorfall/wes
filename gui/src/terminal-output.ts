import { TerminalUnavailable } from "./terminal-errors";
import type { EditorRequest } from "./assistant-editor";
export interface CommandRequest { id: string; text: string; environment?: string | null }
export interface TerminalFrame { command?: CommandRequest | null; editor?: EditorRequest | null; start: number; next: number; data: string; exit: number | null; problem: string | null; closed: boolean }
interface OutputSink {
  poll(cursor: number, signal: AbortSignal): Promise<TerminalFrame>;
  write(bytes: Uint8Array): Promise<void>;
  trimmed(): void;
  editor(request: EditorRequest): Promise<void>;
  command?(request: CommandRequest): Promise<void>;
  ended(exit: number | null): void;
  problem(message: string): void;
  connection(error?: unknown, operation?: string): void;
  unavailable(message: string): void;
}
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
  while (!signal.aborted) {
    let operation = "poll";
    try {
      const frame = await sink.poll(cursor, signal);
      if (signal.aborted) return;
      if (frame.start !== cursor) sink.trimmed();
      operation = "decode output";
      const bytes = Uint8Array.from(atob(frame.data), c => c.charCodeAt(0));
      operation = "render output";
      if (bytes.length) await sink.write(bytes);
      if (signal.aborted) return;
      // Editor acknowledgement can fail independently. Do not replay already parsed bytes.
      cursor = frame.next;
      operation = "pane command acknowledgement";
      if (frame.command) await sink.command?.(frame.command);
      if (signal.aborted) return;
      operation = "editor acknowledgement";
      if (frame.editor) await sink.editor(frame.editor);
      if (signal.aborted) return;
      sink.connection();
      if (frame.problem) sink.problem(frame.problem);
      if (frame.closed && bytes.length === 0) { sink.ended(frame.exit); return; }
    } catch (error) {
      if (signal.aborted) return;
      if (error instanceof TerminalUnavailable) { sink.unavailable(error.message); return; }
      sink.connection(error, operation);
      // Reads are replayable; input writes are deliberately outside this retry loop.
      await retryPause(signal);
    }
  }
}
