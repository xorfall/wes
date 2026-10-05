/**
 * Where keyboard input in a pane goes, decided once for everyone who moves the caret: the split
 * when the focused pane changes, a screen when it leaves the pane it covered, `⌘L` from anywhere.
 *
 * The order is the pane's own input first — the prompt, a terminal, a command field, an editor —
 * then a screen, then anything focusable, then the pane itself, so that focus never falls to the
 * document body, where no pane hears a key.
 */
const INPUTS = [".prompt-field", ".xterm-helper-textarea", ".pane-command input", ".cm-content", ".screen", "textarea, input, [contenteditable=true], button"];

/** The first visible element in `pane` matching `selector`: hidden branches never take focus. */
export const visibleTarget = (pane: HTMLElement, selector: string) =>
  pane.querySelector<HTMLElement>(`:is(${selector}):not([hidden], [hidden] *)`);

/** The pane's shown tab view: the element the pane's input lives in. */
export function paneView(root: ParentNode, paneId: string): HTMLElement | null {
  return root.querySelector<HTMLElement>(`[data-pane-id="${paneId}"] .workspace-tab-view:not([hidden], [hidden] *)`);
}

/** The element the pane's keyboard input goes to. */
export function inputOf(pane: HTMLElement): HTMLElement {
  for (const selector of INPUTS) {
    const found = visibleTarget(pane, selector);
    if (found) return found;
  }
  return pane;
}

/**
 * Puts the caret on the pane's input unless focus is already inside the pane: focus that is
 * already there came from a click or a screen, and is left alone.
 */
export function focusPaneInput(root: ParentNode, paneId: string): boolean {
  const pane = paneView(root, paneId);
  if (!pane) return false;
  if (typeof document !== "undefined" && pane.contains(document.activeElement)) return true;
  inputOf(pane).focus({ preventScroll: true });
  return true;
}

/** After the next paint, when what left has left the document and what stays is visible again. */
export function returnFocusToPane(paneId: string): void {
  if (typeof document === "undefined" || typeof document.querySelector !== "function" || typeof requestAnimationFrame !== "function") return;
  requestAnimationFrame(() => { focusPaneInput(document, paneId); });
}
