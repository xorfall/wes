import { beforeEach, expect, it, vi } from "vitest";
import type { Terminal } from "@xterm/xterm";
const state = vi.hoisted(() => ({ dispose: vi.fn(), unsubscribe: vi.fn(), lost: undefined as (() => void) | undefined, fail: false }));
vi.mock("@xterm/addon-webgl", () => ({ WebglAddon: class {
  constructor() { if (state.fail) throw new Error("WebGL unavailable"); }
  dispose = state.dispose;
  onContextLoss(callback: () => void) { state.lost = callback; return { dispose: state.unsubscribe }; }
} }));
import { accelerateTerminal } from "./terminal-renderer";
beforeEach(() => { vi.clearAllMocks(); state.fail = false; state.lost = undefined; });
it("loads acceleration and releases it once on context loss before terminal teardown", () => {
  const loadAddon = vi.fn(); const stop = accelerateTerminal({ loadAddon } as unknown as Terminal);
  expect(loadAddon).toHaveBeenCalledTimes(1); state.lost!(); stop();
  expect(state.dispose).toHaveBeenCalledTimes(1); expect(state.unsubscribe).toHaveBeenCalledTimes(1);
});
it("keeps the default renderer if WebGL construction or activation fails", () => {
  state.fail = true; const loadAddon = vi.fn(); accelerateTerminal({ loadAddon } as unknown as Terminal)();
  expect(loadAddon).not.toHaveBeenCalled();
  state.fail = false;
  const stop = accelerateTerminal({ loadAddon: () => { throw new Error("no GPU"); } } as unknown as Terminal);
  stop(); expect(state.dispose).toHaveBeenCalledTimes(1);
});
