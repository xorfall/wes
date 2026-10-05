/** Keep Unicode scalar boundaries intact when sending pasted terminal input as JSON. */
export function terminalChunks(text: string): string[] {
  const chunks: string[] = [];
  let chunk = "", count = 0;
  for (const scalar of text) {
    chunk += scalar;
    if (++count === 2048) { chunks.push(chunk); chunk = ""; count = 0; }
  }
  if (chunk) chunks.push(chunk);
  return chunks;
}

/** Serialize writes, coalescing events that arrive while their predecessor is in flight.
 * Mouse-reporting TUIs can generate many onData events per gesture. Keeping one
 * promise/request per event makes scrolling trail behind a slow acknowledgement.
 */
export function terminalInput(write: (text: string) => Promise<unknown>, failed: (error: unknown) => void) {
  let chunks: string[] = [], head = 0, tailSize = 0;
  let writing = false, stopped = false;
  const dispose = () => { stopped = true; chunks = []; head = 0; tailSize = 0; };
  const drain = async () => {
    writing = true;
    try {
      while (!stopped && head < chunks.length) {
        const text = chunks[head++]!;
        if (head === chunks.length) { chunks = []; head = 0; tailSize = 0; }
        await write(text);
      }
    } catch (error) {
      if (!stopped) { dispose(); failed(error); }
    } finally { writing = false; }
  };
  return {
    push(text: string) {
      if (stopped || !text) return;
      for (const scalar of text) {
        if (!chunks.length || tailSize === 2048) { chunks.push(""); tailSize = 0; }
        chunks[chunks.length - 1] += scalar;
        tailSize++;
      }
      // No timer on the first keystroke; later input shares the next bounded write.
      if (!writing) void drain();
    },
    dispose,
  };
}
