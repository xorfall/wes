import { budget } from "../limits/policy";
import { terminalViews } from "./terminal-tabs";
import { close, focus, type Pane, type PaneView, type SplitState, type WorkspaceTab, type ValueBinding } from "./split-model";
import { workspaceIdentity, workspaceName } from "../workspace-binding";

export function max_workspace_tabs():number { return budget("ui.workspace.tabs"); }
export const tabKey = (workspace?: string) => workspace === undefined ? "origin" : `workspace:${workspace}`;
export const viewKey = (tab: WorkspaceTab) => tab.value ? JSON.stringify([tabKey(tab.workspace), tab.value.generation, tab.value.node, tab.value.dashboard?'dashboard':tab.value.related ? "related" : "value"]) : tabKey(tab.workspace);
export const paneTabs = (pane: Pane): readonly WorkspaceTab[] => pane.tabs ?? [{ workspace: pane.workspace, ...(pane.value ? { value: pane.value } : {}) }];
export const activeView = (pane: Pane) => viewKey(pane);
export function tabViews(pane: Pane): PaneView[] {
  if (pane.terminal) return terminalViews(pane);
  return paneTabs(pane).map(tab => ({ ...pane, workspace: tab.workspace, value: tab.value }));
}
export const workspaceViews = (state: SplitState) => state.panes.flatMap(tabViews);
export function openWorkspaceTab(state: SplitState, paneId: string, workspace: string, activate = false): SplitState {
  if (!workspaceName(workspace)) throw new Error("Invalid workspace name.");
  const pane = state.panes.find(p => p.id === paneId);
  if (!pane || pane.terminal || pane.shows) throw new Error("Choose an existing wes session pane for the workspace tab.");
  const tabs = paneTabs(pane);
  const exists = tabs.some(tab => !tab.value && tab.workspace === workspace);
  if (!exists && tabs.length >= max_workspace_tabs()) throw new Error("This pane has reached its workspace tab budget. Close a tab first.");
  const next = { ...state, panes: state.panes.map(p => p.id === paneId ? { ...p, title: "session",
    tabs: exists ? tabs : [...tabs, { workspace }], ...(activate ? { workspace, value: undefined } : {}) } : p) };
  return activate ? focus(next, paneId) : next;
}
export function selectWorkspaceTab(state: SplitState, paneId: string, workspace?: string): SplitState {
  const pane = state.panes.find(p => p.id === paneId);
  if (!pane || !paneTabs(pane).some(tab => !tab.value && tab.workspace === workspace)) return state;
  return selectViewTab(state, paneId, tabKey(workspace));
}
export function closeWorkspaceTab(state: SplitState, paneId: string, workspace?: string): SplitState {
  return closeViewTab(state, paneId, tabKey(workspace));
}

export function openValueTab(state: SplitState, paneId: string, workspace: string | undefined, value: ValueBinding, activate = false): SplitState {
  const pane = state.panes.find(p => p.id === paneId);
  if (!pane || pane.terminal || pane.shows) throw new Error("Choose an existing wes session or value pane for the value tab.");
  const tabs = paneTabs(pane), key = viewKey({ workspace, value });
  const exists = tabs.some(tab => viewKey(tab) === key);
  if (!exists && tabs.length >= max_workspace_tabs()) throw new Error("This pane has reached its tab budget. Close a tab first.");
  const next = { ...state, panes: state.panes.map(p => p.id === paneId ? { ...p, title: "session",
    tabs: exists ? tabs : [...tabs, { workspace, value }] } : p) };
  return activate ? selectViewTab(next, paneId, key) : next;
}
export function selectViewTab(state: SplitState, paneId: string, key: string): SplitState {
  const pane = state.panes.find(p => p.id === paneId), tab = pane && paneTabs(pane).find(tab => viewKey(tab) === key);
  if (!tab) return state;
  return focus({ ...state, panes: state.panes.map(p => p.id === paneId ? { ...p, workspace: tab.workspace, value: tab.value } : p) }, paneId);
}
export function closeViewTab(state: SplitState, paneId: string, key: string): SplitState {
  const pane = state.panes.find(p => p.id === paneId);
  if (!pane || !paneTabs(pane).some(tab => viewKey(tab) === key)) return state;
  const tabs = paneTabs(pane);
  const closing = tabs.find(tab => viewKey(tab) === key)!;
  const sessions = state.panes.filter(p => !p.terminal).flatMap(paneTabs).filter(tab => !tab.value);
  if (!closing.value && sessions.length <= 1) return state;
  if (tabs.length === 1) return tabs[0]!.value ? close(state, paneId) : state;
  const next = tabs.filter(tab => viewKey(tab) !== key);
  const at = tabs.findIndex(tab => viewKey(tab) === key), active = activeView(pane) === key ? next[Math.min(at, next.length - 1)]! : pane;
  return { ...state, panes: state.panes.map(p => p.id === paneId ? { ...p, tabs: next, workspace: active.workspace, value: active.value } : p) };
}
export function updateValueTab(state: SplitState, paneId: string, key: string, tab: ValueBinding["tab"]): SplitState {
  return { ...state, panes: state.panes.map(p => p.id === paneId ? { ...p,
    ...(activeView(p) === key && p.value ? { value: { ...p.value, tab } } : {}),
    ...(p.tabs ? { tabs: p.tabs.map(t => viewKey(t) === key && t.value ? { ...t, value: { ...t.value, tab } } : t) } : {}) } : p) };
}

/** Remove only references to the retired workspace. Independent pane history is preserved. */
export function removeWorkspaceViews(state: SplitState, name: string): SplitState {
 return {...state, ...(state.workspaceBindings ? { workspaceBindings: Object.fromEntries(Object.entries(state.workspaceBindings).filter(([key]) => key !== name)) } : {}), panes:state.panes.map(p=>{
  const tabs=paneTabs(p).filter(t=>t.workspace!==name);
  if(p.workspace!==name&&tabs.length===paneTabs(p).length)return p;
  if(!tabs.length)return {id:p.id,title:"Session"};
  return {...p,tabs,...(p.workspace===name?{workspace:tabs[0]!.workspace,value:tabs[0]!.value}:{})};
 })};
}

/** Identity and layout are published/saved in one transition, only after an explicit open. */
export function bindWorkspace(state: SplitState, name: string, identity: string): SplitState {
  if (!workspaceName(name) || !workspaceIdentity(identity)) throw new Error("Invalid workspace binding.");
  return { ...state, workspaceBindings: { ...state.workspaceBindings, [name]: identity } };
}
