import { WebglAddon } from "@xterm/addon-webgl";
import type { Terminal } from "@xterm/xterm";
/** GPU acceleration is optional. Context loss restores xterm's built-in DOM renderer. */
export function accelerateTerminal(terminal: Terminal): () => void {
  let addon: WebglAddon | undefined;
  let loss: { dispose(): void } | undefined;
  const dispose = () => {
    loss?.dispose(); loss = undefined;
    const current = addon; addon = undefined;
    current?.dispose();
  };
  try {
    addon = new WebglAddon();
    loss = addon.onContextLoss(dispose);
    terminal.loadAddon(addon);
  } catch {
    dispose();
  }
  return dispose;
}
