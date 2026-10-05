import { canSplitTerminal, closeTerminalTab, openTerminalTab, placeTerminalTab, requireTerminalTab } from "./terminal-tabs";
import type { TerminalTarget } from "../terminal-target";
import { read, words } from "./commands";
import { close, divide, focus, max_panes, nextPane, splitPane, terminalPane, titleOf, type Shown, type SplitState, type ValueBinding } from "./split-model";

/** Both the prompt and wesx use the same pane rules; the latter never submits engine source. */
export function applyPaneCommand(state: SplitState, text: string, source: string,
  resolve: (shown: Shown) => Shown, terminal = false,
  resolveValue?: (name: string) => ValueBinding, terminalTarget?: TerminalTarget, sourceHistory?: string): SplitState {
  if (!state.panes.some(p => p.id === source)) throw new Error("The source pane has closed.");
  const typed = read(text);
  const args = words(text);
  if ((typed.kind === "close" && args.length !== 1) || (typed.kind === "split" && args.length > 2))
    throw new Error("Use one pane command with its documented arguments.");
  if (typed.kind === "trouble") throw new Error(typed.said);
  if (terminal) requireTerminalTab(state, source, sourceHistory);
  if (typed.kind === "terminal-tab") {
    // `/terminal-tab` (and the + button) stays within the calling terminal and selects the new tab.
    if (typed.activate === undefined) return openTerminalTab(state, source, terminalTarget, sourceHistory);
    return placeTerminalTab(state, source, typed.pane, typed.activate, terminalTarget, sourceHistory);
  }
  if (typed.kind === "close") {
    const pane = state.panes.find(p => p.id === source)!;
    // One rule for a terminal tab, whether `wesx exit`, the tab's × or Delete closes it.
    if (pane.terminal) return closeTerminalTab(state, source, sourceHistory ?? pane.history!);
    // Shared with keyboard closure: the final wes command context is a no-op.
    return close(state, source);
  }
  if (typed.kind === "split" || typed.kind === "directional-split") {
    if (state.panes.length >= max_panes() && (typed.kind !== "split" || typeof typed.how === "string"))
      throw new Error("The pane budget is full. Close a pane before splitting.");
    if (typed.kind === "split") return divide(focus(state, source), typed.how);
    const content = typed.content;
    if (content && "workspace" in content) {
      return splitPane(state, typed.direction, { id: nextPane(state), title: "session", workspace: content.workspace }, typed.takeFocus, source);
    }
    if (content && "value" in content) {
      if (!resolveValue) throw new Error("Wait for the workspace connection before opening a value.");
      const value = { ...resolveValue(content.value), ...(content.related ? { related: true as const } : {}) };
      if(value.dashboard&&content.related)throw new Error('Dashboards are UI layouts. Open them without related.');
      return splitPane(state, typed.direction, { id: nextPane(state), title: `${value.related ? "related " : ""}$${value.label}`, value }, typed.takeFocus, source);
    }
    const shown = content && "screen" in content ? resolve(content) : undefined;
    const shell = content && "terminal" in content;
    if (shell) canSplitTerminal(state);
    const pane = shell ? terminalPane(nextPane(state), terminalTarget ? { terminalTarget, terminalLaunch: true } : {})
      : { id: nextPane(state), title: titleOf(shown), ...(shown ? { shows: shown } : {}) };
    return splitPane(state, typed.direction, pane, typed.takeFocus, source);
  }
  throw new Error('wesx --cmd accepts /split, /lsplit, /rsplit, /tsplit, /bsplit (including x variants), /tab xterm, /tabx xterm, /terminal-tab and /close.');
}
