import { expect, it, vi } from "vitest";
import type { Terminal } from "@xterm/xterm";
import { normalizeTerminalWheel, wheelDistance } from "./terminal-wheel";

it("conserves distance across slow, fast and decelerating event partitions", () => {
  for (const events of [Array(200).fill(1), [100, 100], [50, 49, 40, 30, 20, 10, 1]]) {
    const wheel = wheelDistance();
    expect(events.reduce((sum, delta) => sum + wheel.consume(delta, 16, "mouse"), 0)).toBe(12);
  }
  const wheel = wheelDistance();
  for (let i = 0; i < 15; i++) expect(wheel.consume(1, 16, "mouse")).toBe(0);
  expect(wheel.consume(1, 16, "mouse")).toBe(1);
});
it("does not make a reverse gesture pay the old direction's fractional debt", () => {
  const wheel = wheelDistance();
  expect(wheel.consume(15, 16, "mouse")).toBe(0);
  expect(wheel.consume(-16, 16, "mouse")).toBe(-1);
  expect(wheel.consume(16, 16, "mouse")).toBe(1);
});
it("keeps constant memory and gain after long repeated gestures", () => {
  const wheel = wheelDistance();
  for (let round = 0; round < 100; round++) {
    let total = 0;
    for (let i = 0; i < 1600; i++) total += wheel.consume(1, 16, "mouse");
    expect(total).toBe(100);
    expect(wheel.consume(0, 16, "mouse")).toBe(0);
  }
});
it("resets fractions with mode, geometry and explicit owner changes", () => {
  const wheel = wheelDistance();
  wheel.consume(15, 16, "mouse");
  expect(wheel.consume(1, 16, "arrows")).toBe(0);
  expect(wheel.consume(16, 32, "arrows")).toBe(0);
  wheel.reset();
  expect(wheel.consume(16, 32, "arrows")).toBe(0);
});
it("bounds individual work without storing a backlog; invalid geometry cannot emit", () => {
  const wheel = wheelDistance();
  expect(wheel.consume(1e9, 16, "mouse")).toBe(128);
  expect(wheel.consume(16, 16, "mouse")).toBe(1);
  expect(wheel.consume(Number.MAX_VALUE, Number.MIN_VALUE, "mouse")).toBe(128);
  expect(wheel.consume(16, 16, "mouse")).toBe(1);
  for (const height of [0, -1, Infinity, NaN]) expect(wheel.consume(100, height, "mouse")).toBe(0);
  expect(wheel.consume(Infinity, 16, "mouse")).toBe(0);
});
function setup() {
  class Wheel extends Event {
    deltaY = 0; deltaX = 0; deltaMode = 0; clientX = 0; clientY = 0;
    shiftKey = false; ctrlKey = false; altKey = false; metaKey = false;
    constructor(type: string, init: WheelEventInit = {}) { super(type, init); for (const key of Object.keys(this)) if (key in init) Object.defineProperty(this, key, { value: init[key as keyof WheelEventInit] }); }
  }
  vi.stubGlobal("WheelEvent", Wheel);
  let capture: ((event: WheelEvent) => void) | undefined;
  const forwarded: WheelEvent[] = [];
  const screen = { getBoundingClientRect: () => ({ height: 480 }) };
  const element = {
    querySelector: () => screen,
    addEventListener: vi.fn((_type, handler) => { capture = handler; }),
    removeEventListener: vi.fn(() => { capture = undefined; }),
    dispatchEvent: (event: WheelEvent) => { capture?.(event); forwarded.push(event); },
  };
  const bufferDispose = vi.fn(), resizeDispose = vi.fn();
  const terminal = { element, rows: 30, modes: { mouseTrackingMode: "vt200" },
    buffer: { active: { type: "alternate" }, onBufferChange: () => ({ dispose: bufferDispose }) },
    onResize: () => ({ dispose: resizeDispose }),
  };
  const dispose = normalizeTerminalWheel(terminal as unknown as Terminal);
  const send = (init: WheelEventInit = {}) => {
    const event = new Wheel("wheel", { deltaY: 16, cancelable: true, ...init }) as unknown as WheelEvent;
    capture?.(event); return event;
  };
  return { terminal, element, forwarded, send, dispose, bufferDispose, resizeDispose };
}
it("forwards whole rows once, with coordinates/modifiers, and removes all listeners", () => {
  const h = setup();
  try {
    const event = h.send({ deltaY: -49, clientX: 80, clientY: 120, ctrlKey: true, altKey: true, metaKey: true });
    expect(event.defaultPrevented).toBe(true);
    expect(h.forwarded).toHaveLength(3);
    for (const e of h.forwarded) expect(e).toMatchObject({ deltaY: -1, deltaMode: 1, clientX: 80, clientY: 120, ctrlKey: true, altKey: true, metaKey: true });
    h.dispose(); h.send();
    expect(h.forwarded).toHaveLength(3);
    expect(h.element.removeEventListener).toHaveBeenCalledWith("wheel", expect.any(Function), true);
    expect(h.bufferDispose).toHaveBeenCalledOnce(); expect(h.resizeDispose).toHaveBeenCalledOnce();
  } finally { vi.unstubAllGlobals(); }
});
it("passes local scrollback, X10, line/page, shift and horizontal events to xterm unchanged", () => {
  const h = setup();
  try {
    for (const init of [{ deltaMode: 1 }, { deltaMode: 2 }, { shiftKey: true }, { deltaX: 40 }, { deltaY: 0 }])
      expect(h.send(init).defaultPrevented).toBe(false);
    h.terminal.buffer.active.type = "normal";
    for (const tracking of ["none", "x10"]) {
      h.terminal.modes.mouseTrackingMode = tracking;
      expect(h.send().defaultPrevented).toBe(false);
    }
    expect(h.forwarded).toHaveLength(0);
    h.terminal.buffer.active.type = "alternate";
    h.terminal.modes.mouseTrackingMode = "none";
    expect(h.send().defaultPrevented).toBe(true);
    expect(h.forwarded).toHaveLength(1);
  } finally { h.dispose(); vi.unstubAllGlobals(); }
});
