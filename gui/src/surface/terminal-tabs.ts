import { budget } from "../limits/policy";
import { targetKey, type TerminalTarget } from "../terminal-target";
import { close, focus, max_panes, nextPane, splitPane, terminalPane, type Pane, type PaneView, type SplitState } from "./split-model";

export interface TerminalTab {
  readonly history: string;
  readonly cwd?: string;
  readonly terminalTarget?: TerminalTarget;
  readonly terminalLaunch?: boolean;
}
export function max_terminal_tabs():number { return budget("ui.terminal.tabs"); }
export function terminalTabs(pane: Pane): readonly TerminalTab[] {
  return !pane.terminal ? [] : pane.terminalTabs!;
}
export const terminalViews = (pane: Pane): PaneView[] => terminalTabs(pane).map(tab => ({ ...pane, ...tab }));
export const allTerminals = (state: SplitState): PaneView[] => state.panes.flatMap(terminalViews);
export function requireTerminalTab(state: SplitState, paneId: string, history?: string): Pane {
  const pane = state.panes.find(p => p.id === paneId);
  if (!pane?.terminal || (history !== undefined && !terminalTabs(pane).some(t => t.history === history)))
    throw new Error("The source terminal tab has closed.");
  return pane;
}
/** Every terminal, in any pane or workspace, counts against one budget. */
export function requireTerminalCapacity(state: SplitState): void {
  if (allTerminals(state).length >= max_terminal_tabs()) throw new Error("The terminal budget is full. Close a terminal tab or pane first.");
}
export function canOpenTerminalTab(state: SplitState, paneId: string, history?: string): Pane {
  const pane = requireTerminalTab(state, paneId, history);
  requireTerminalCapacity(state);
  return pane;
}
/** A terminal split adds both a pane and a terminal; neither budget may be exceeded. */
export function canSplitTerminal(state: SplitState): void {
  if (state.panes.length >= max_panes()) throw new Error("The pane budget is full. Close a pane before splitting.");
  requireTerminalCapacity(state);
}

/**
 * Where `/tab xterm` and `/tabx xterm` put the new terminal: an existing terminal pane of the
 * source's workspace (`pane` set), or a new terminal pane to the right of the source (`pane` absent).
 * Both the prompt's preflight and the reducer compute this from the same state, so they agree.
 */
export interface TerminalPlacement { readonly source: Pane; readonly pane?: Pane }
export function terminalPlacement(state: SplitState, sourceId: string, requested?: string): TerminalPlacement {
  const source = state.panes.find(p => p.id === sourceId);
  if (!source) throw new Error("The source pane has closed.");
  const sameWorkspace = (p: Pane) => p.terminal === true && p.workspace === source.workspace;
  if (requested !== undefined) {
    const pane = state.panes.find(p => p.id === requested);
    if (!pane || !sameWorkspace(pane)) throw new Error(`pane:${requested} is not a terminal pane in this workspace. Choose an existing terminal pane, or omit pane:.`);
    requireTerminalCapacity(state);
    return { source, pane };
  }
  const recency = (p: Pane) => state.visited.indexOf(p.id);
  const pane = source.terminal ? source
    : state.panes.filter(sameWorkspace).reduce<Pane | undefined>((best, p) => !best || recency(p) > recency(best) ? p : best, undefined);
  if (!pane && state.panes.length >= max_panes()) throw new Error("The pane budget is full. Close a pane before opening a terminal pane.");
  requireTerminalCapacity(state);
  return pane ? { source, pane } : { source };
}
/**
 * The same destination: same pane, workspace, selected tab, and tabs with the same targets and
 * launch consent. A cwd report replaces the pane object without changing any of these, so it
 * must not refuse the command.
 */
function sameDestination(a: Pane | undefined, b: Pane | undefined): boolean {
  if (!a || !b) return a === b;
  const tabs = (p: Pane) => JSON.stringify(terminalTabs(p).map(t => [t.history, targetKey(t.terminalTarget), t.terminalLaunch === true]));
  return a.id === b.id && a.terminal === b.terminal && a.workspace === b.workspace && a.history === b.history && tabs(a) === tabs(b);
}
const destinationChanged = () => new Error("The terminal destination changed while resolving the terminal; retry the terminal command.");
/** Refuses, rather than reroutes, when the source or destination changed after `expected` was chosen. */
export function requireTerminalPlacement(state: SplitState, expected: TerminalPlacement, requested?: string): TerminalPlacement {
  if (state.panes.find(p => p.id === expected.source.id) !== expected.source)
    throw new Error("The source pane changed while resolving the terminal; retry the terminal command.");
  // A chosen pane that closed or was retired is a changed destination, not an invalid request.
  if (expected.pane && !sameDestination(state.panes.find(p => p.id === expected.pane!.id), expected.pane)) throw destinationChanged();
  const current = terminalPlacement(state, expected.source.id, requested);
  if (!sameDestination(current.pane, expected.pane)) throw destinationChanged();
  return current;
}
/** Adds the tab described by `/tab xterm` or `/tabx xterm` at its placement; `/tab` keeps focus and selection. */
export function placeTerminalTab(state: SplitState, sourceId: string, requested: string | undefined, activate: boolean,
  target?: TerminalTarget, sourceHistory?: string): SplitState {
  const { pane } = terminalPlacement(state, sourceId, requested);
  if (pane) return openTerminalTab(state, pane.id, target, pane.id === sourceId ? sourceHistory : undefined, { activate });
  return splitPane(state, "right", terminalPane(nextPane(state), target ? { terminalTarget: target, terminalLaunch: true } : {}), activate, sourceId);
}
/**
 * Adds an independent terminal to an existing terminal pane. An unbound local cwd is inherited
 * from `sourceHistory`, or else from the pane's selected tab; a target never inherits one.
 */
export function openTerminalTab(state: SplitState, paneId: string, target?: TerminalTarget, sourceHistory?: string,
  { activate = true }: { readonly activate?: boolean } = {}): SplitState {
  const pane = canOpenTerminalTab(state, paneId, sourceHistory);
  const source = terminalTabs(pane).find(t => t.history === (sourceHistory ?? pane.history))!;
  const tab: TerminalTab = { history: crypto.randomUUID(), ...(target ? { terminalTarget: target, terminalLaunch: true } : {}),
    ...(!target && !source.terminalTarget && source.cwd ? { cwd: source.cwd } : {}) };
  const next = { ...state, panes: state.panes.map(p => p.id === paneId ? { ...pane,
    ...(activate ? { history: tab.history } : {}), terminalTabs: [...terminalTabs(pane), tab] } : p) };
  return activate ? focus(next, paneId) : next;
}
export function selectTerminalTab(state: SplitState, paneId: string, history: string): SplitState {
  const pane = requireTerminalTab(state, paneId, history);
  return focus({ ...state, panes: state.panes.map(p => p === pane ? { ...p, history } : p) }, paneId);
}
/**
 * Closing the last tab closes the pane, the way the last tab of a terminal window closes the
 * window; a terminal that was the whole window becomes the session instead, since the window
 * itself never closes. A workspace pane keeps its last tab: its session is where commands go.
 */
export function closeTerminalTab(state: SplitState, paneId: string, history: string): SplitState {
  const pane = requireTerminalTab(state, paneId, history), tabs = terminalTabs(pane);
  if (tabs.length === 1) {
    if (state.panes.length === 1) return { ...state, panes: [{ id: paneId, title: "session", ...(pane.workspace === undefined ? {} : { workspace: pane.workspace }) }], remembered: {} };
    return close(state, paneId);
  }
  const at = tabs.findIndex(t => t.history === history), next = tabs.filter(t => t.history !== history);
  return { ...state, panes: state.panes.map(p => p === pane ? { ...p, terminalTabs: next,
    history: p.history === history ? next[Math.min(at, next.length - 1)]!.history : p.history } : p) };
}
export function terminalDirectory(state: SplitState, paneId: string, history: string, cwd: string): SplitState {
  return { ...state, panes: state.panes.map(p => p.id !== paneId || !p.terminal ? p
    : { ...p, terminalTabs: terminalTabs(p).map(t => t.history === history && !t.terminalTarget ? { ...t, cwd } : t) }) };
}

/** Compare-and-replace only the reviewed tab; an approval cannot retarget another terminal. */
export function acceptTerminalTarget(state: SplitState, paneId: string, history: string, previous: TerminalTarget, next: TerminalTarget): SplitState {
  const pane = requireTerminalTab(state, paneId, history);
  const tab = terminalTabs(pane).find(t => t.history === history)!;
  if (targetKey(tab.terminalTarget) !== targetKey(previous) || previous.environment !== next.environment || previous.target !== next.target)
    throw new Error("The terminal selection changed. Review its current destination again.");
  return { ...state, panes: state.panes.map(p => p !== pane ? p
    : { ...p, terminalTabs: terminalTabs(p).map(t => t.history === history ? { ...t, cwd: undefined, terminalTarget: next } : t) }) };
}
