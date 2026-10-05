import { max_terminal_tabs, type TerminalTab } from "./terminal-tabs";
import { readTerminalTarget } from "../terminal-target";
import { max_workspace_tabs, viewKey } from "./workspace-tabs";
/** Untrusted persisted descriptors are validated before they can control layout or open content. */
import { max_panes, historyId, oneP, SESSION_PANE, titleOf, type Layout, type Pane, type Shown, type SplitState, type ValueBinding, type WorkspaceTab } from "./split-model";
import { workspaceIdentity, workspaceName } from "../workspace-binding";
const record = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
const text = (value: unknown): value is string => typeof value === "string" && value.length <= 512 && !/[\u0000-\u001f]/.test(value);
const screens = ["dashboard", "spec", "graph", "stale", "env", "settings", "open", "edit"];
const tabs = ["result", "json", "details", "source"];
const sections = ["appearance", "editor", "results", "keys", "connections", "aliases", "data", "limits"];
function valueBinding(value: unknown): ValueBinding | undefined {
  if (!record(value) || !text(value.node) || !value.node || !text(value.generation) || !value.generation
    || !text(value.label) || !value.label || (value.dashboard !== undefined && value.dashboard !== true) || (value.dashboard && value.related) || (value.related !== undefined && value.related !== true) || (value.tab !== undefined && (!text(value.tab) || !value.tab))) return;
  return { node: value.node, generation: value.generation, label: value.label, ...(value.dashboard?{dashboard:true}:{}), ...(value.related ? { related: true } : {}), ...(value.tab === undefined ? {} : { tab: value.tab }) };
}
export function restoreSplit(value: unknown): SplitState {
  const fallback = () => oneP(SESSION_PANE);
  if (!record(value) || value.version !== 1 || !Array.isArray(value.panes) || value.panes.length < 1 || value.panes.length > max_panes()) return fallback();
  const panes: Pane[] = [];
  const histories = new Set<string>();
  for (const p of value.panes) {
    if (!record(p) || typeof p.id !== "string" || !/^p[1-9][0-9]{0,8}$/.test(p.id) || panes.some(other => other.id === p.id)) return fallback();
    if (p.component !== undefined) return fallback();
    if (p.cwd !== undefined || p.terminalTarget !== undefined || p.terminalLaunch !== undefined) return fallback();
    if (p.terminal !== undefined && p.terminal !== true) return fallback();
    if (p.workspace !== undefined && !workspaceName(p.workspace)) return fallback();
    const value = p.value === undefined ? undefined : valueBinding(p.value);
    if (p.value !== undefined && (!value || p.terminal || p.shows)) return fallback();
    let shows: Shown | undefined;
    if (p.shows !== undefined) {
      const s = p.shows;
      if (p.terminal || !record(s) || !screens.includes(String(s.screen)) ||
        (s.node !== undefined && !text(s.node)) || (s.tab !== undefined && !tabs.includes(String(s.tab))) ||
        (s.section !== undefined && !sections.includes(String(s.section)))) return fallback();
      shows = { screen: s.screen as Shown["screen"], ...(s.node === undefined ? {} : { node: s.node as string }),
        ...(s.tab === undefined ? {} : { tab: s.tab as Shown["tab"] }), ...(s.section === undefined ? {} : { section: s.section as Shown["section"] }) };
    }
    let workspaceTabs: Pane["tabs"];
    if (p.tabs !== undefined) {
      if (p.terminal || p.shows || !Array.isArray(p.tabs) || !p.tabs.length || p.tabs.length > max_workspace_tabs()) return fallback();
      const seen = new Set<string>();
      const parsed: WorkspaceTab[] = [];
      for (const tab of p.tabs) {
        if (!record(tab) || (tab.workspace !== undefined && !workspaceName(tab.workspace))) return fallback();
        const result = tab.value === undefined ? undefined : valueBinding(tab.value);
        if (tab.value !== undefined && !result) return fallback();
        const descriptor: WorkspaceTab = { ...(tab.workspace === undefined ? {} : { workspace: tab.workspace as string }), ...(result ? { value: result } : {}) };
        if (seen.has(viewKey(descriptor))) return fallback();
        seen.add(viewKey(descriptor)); parsed.push(descriptor);
      }
      const active = parsed.find(tab => viewKey(tab) === viewKey({ workspace: p.workspace as string | undefined, value }));
      if (!active || JSON.stringify(active.value) !== JSON.stringify(value)) return fallback();
      workspaceTabs = parsed;
    }
    // Each tab owns one history. Restore never carries explicit target-launch permission.
    let terminalTabDescriptors: TerminalTab[] | undefined;
    let history: string | undefined;
    const ownedHistory = (value: unknown) => {
      if (!historyId(value) || histories.has(value.toLowerCase())) return;
      const next = value.toLowerCase();
      histories.add(next); return next;
    };
    if (p.terminal) {
      if (!historyId(p.history) || !Array.isArray(p.terminalTabs)
        || !p.terminalTabs.length || p.terminalTabs.length > max_terminal_tabs()) return fallback();
      terminalTabDescriptors = [];
      for (const tab of p.terminalTabs) {
        if (!record(tab) || (tab.cwd !== undefined && (!text(tab.cwd) || !tab.cwd.startsWith("/")))) return fallback();
        const target = tab.terminalTarget === undefined ? undefined : readTerminalTarget(tab.terminalTarget);
        if (tab.terminalTarget !== undefined && !target) return fallback();
        const tabHistory = ownedHistory(tab.history);
        if (!tabHistory) return fallback();
        if (history === undefined && tabHistory === p.history.toLowerCase()) history = tabHistory;
        terminalTabDescriptors.push({ history: tabHistory, ...(typeof tab.cwd === "string" && !target ? { cwd: tab.cwd } : {}),
          ...(target ? { terminalTarget: target } : {}) });
      }
      if (!history) return fallback();
    } else if (p.history !== undefined || p.terminalTabs !== undefined) return fallback();
    if (histories.size > max_terminal_tabs()) return fallback();
    panes.push({ id: p.id, ...(terminalTabDescriptors ? { terminalTabs: terminalTabDescriptors } : {}), ...(workspaceTabs ? { tabs: workspaceTabs } : {}), ...(value ? { value } : {}), title: p.terminal ? "xterm" : value && !workspaceTabs ? `${value.related ? "related " : ""}$${value.label}` : titleOf(shows), ...(p.workspace === undefined ? {} : { workspace: p.workspace }), ...(p.terminal ? { terminal: true, history } : {}),
      ...(shows ? { shows } : {}), ...(text(p.command) ? { command: p.command } : {}) });
  }
  const seen = new Set<string>();
  function tree(v: unknown, depth: number): Layout | undefined {
    if (!record(v) || depth >= max_panes()) return;
    if (typeof v.pane === "string") {
      if (seen.has(v.pane) || !panes.some(p => p.id === v.pane)) return;
      seen.add(v.pane); return { pane: v.pane };
    }
    if ((v.axis !== "right" && v.axis !== "down") || typeof v.ratio !== "number" || !Number.isFinite(v.ratio) || v.ratio < 0.1 || v.ratio > 0.9) return;
    const first = tree(v.first, depth+1), second = tree(v.second, depth+1);
    if (first && second) return { axis: v.axis, ratio: v.ratio, first, second };
  }
  const layout = tree(value.layout, 0);
  if (!layout || seen.size !== panes.length || typeof value.focused !== "string" || !seen.has(value.focused)) return fallback();
  const remembered = Object.fromEntries(record(value.remembered) ? Object.entries(value.remembered).filter(([k,v]) => text(k) && typeof v === "string" && seen.has(v)).slice(0, 16).map(([k, v]) => [k, String(v)]) : []);
  const visited = Array.isArray(value.visited) ? [...new Set(value.visited.filter((p): p is string => typeof p === "string" && seen.has(p)))] : [];
  const largest = Math.max(...panes.map(p => Number(p.id.slice(1))));
  const boundNames = new Set(panes.flatMap(pane => [pane.workspace, ...(pane.tabs ?? []).map(tab => tab.workspace)].filter((name): name is string => name !== undefined)));
  const workspaceBindings = record(value.workspaceBindings) ? Object.fromEntries(
    Object.entries(value.workspaceBindings).filter(([name, identity]) => boundNames.has(name) && workspaceName(name) && workspaceIdentity(identity)),
  ) as Record<string, string> : undefined;
  return { ...(workspaceBindings ? { workspaceBindings } : {}), version: 1, panes, layout, axis: "axis" in layout ? layout.axis : "right", focused: value.focused, remembered,
    nextId: Number.isSafeInteger(value.nextId) && Number(value.nextId) > largest && Number(value.nextId) < 1e9 ? Number(value.nextId) : largest + 1,
    visited: [...visited.filter(id => id !== value.focused), value.focused] };
}
