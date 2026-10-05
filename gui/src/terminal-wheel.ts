import type { Terminal } from "@xterm/xterm";

/** Quantize distance, never event frequency. No timer or synthetic momentum. */
export function wheelDistance() {
  let remainder = 0, direction = 0, context = "";
  return {
    reset() { remainder = 0; direction = 0; context = ""; },
    consume(pixels: number, rowHeight: number, mode: string): number {
      if (!Number.isFinite(pixels) || !Number.isFinite(rowHeight) || rowHeight <= 0) return 0;
      if (!pixels) return 0;
      const nextDirection = Math.sign(pixels), nextContext = `${mode}:${rowHeight}`;
      if (nextDirection !== direction || nextContext !== context) remainder = 0;
      direction = nextDirection; context = nextContext;
      const distance = Math.abs(pixels) / rowHeight + remainder;
      const whole = Math.floor(distance);
      remainder = distance >= 128 ? 0 : distance - whole;
      // A malformed/huge event must not create an unbounded synchronous dispatch or backlog.
      return direction * Math.min(128, whole);
    },
  };
}

/** Public xterm + DOM adapter. xterm still owns all terminal protocol encoding. */
export function normalizeTerminalWheel(terminal: Terminal): () => void {
  const element = terminal.element!;
  const screen = element.querySelector<HTMLElement>(".xterm-screen")!;
  const distance = wheelDistance();
  const forwarded = new WeakSet<Event>();
  const handle = (event: WheelEvent) => {
    if (forwarded.has(event)) return;
    const tracking = terminal.modes.mouseTrackingMode;
    const buffer = terminal.buffer.active.type;
    const application = tracking !== "none" && tracking !== "x10" || buffer === "alternate";
    if (!application || event.deltaMode !== 0 || event.shiftKey || !event.deltaY || Math.abs(event.deltaX) > Math.abs(event.deltaY)) {
      distance.reset(); return;
    }
    const rowHeight = screen.getBoundingClientRect().height / terminal.rows;
    if (!(rowHeight > 0) || !Number.isFinite(event.deltaY)) { distance.reset(); return; }
    const lines = distance.consume(event.deltaY, rowHeight, `${buffer}:${tracking}`);
    event.preventDefault(); event.stopImmediatePropagation();
    for (let line = 0; line < Math.abs(lines); line++) {
      const normalized = new WheelEvent("wheel", {
        deltaY: Math.sign(lines), deltaMode: 1,
        clientX: event.clientX, clientY: event.clientY,
        ctrlKey: event.ctrlKey, altKey: event.altKey, metaKey: event.metaKey,
        bubbles: true, cancelable: true,
      });
      forwarded.add(normalized);
      element.dispatchEvent(normalized);
    }
  };
  // Capture precedes xterm's viewport and mouse listeners, so only one path sees each event.
  element.addEventListener("wheel", handle, { capture: true, passive: false });
  const bufferChange = terminal.buffer.onBufferChange(() => distance.reset());
  const resize = terminal.onResize(() => distance.reset());
  return () => {
    element.removeEventListener("wheel", handle, true);
    bufferChange.dispose(); resize.dispose(); distance.reset();
  };
}
