import { budget } from "../limits/policy";
import type { TerminalTab } from "./terminal-tabs";
import type { TerminalTarget } from "../terminal-target";
/** Stable pane identities and a persisted binary tree; geometry never depends on pane count. */
import type { SectionName } from "./settings-model";
import type { EditFileContext, Summoned } from "./commands";
import type { OpenTab } from "./screens/Open";
export type Axis = "right" | "down";
export type SplitDirection = Axis | "left" | "up";
export type Layout = { readonly pane: string } | {
  readonly axis: Axis; readonly ratio: number; readonly first: Layout; readonly second: Layout;
};
export interface Shown {
  readonly screen: Summoned; readonly node?: string; readonly tab?: OpenTab; readonly section?: SectionName;
  /** `/edit env|types` in this pane: which buffer, and the name it was given, if it was. */
  readonly fileContext?: EditFileContext; readonly fileName?: string;
}
/** A view of one existing node, never authority to rerun or retain its output. */
export interface ValueBinding {
  readonly dashboard?:true;
  readonly related?: true;
  readonly node: string;
  readonly generation: string;
  readonly label: string;
  readonly tab?: OpenTab;
}
export interface WorkspaceTab { readonly workspace?: string; readonly value?: ValueBinding }
export interface Pane {
  readonly id: string; readonly title: string; readonly command?: string; readonly shows?: Shown;
  /** Explicit workspace binding; absent means the default workspace view. */
  readonly workspace?: string;
  /** Session and node views retained inside this pane; workspace and value select the active view. */
  readonly tabs?: readonly WorkspaceTab[];
  /** Active value view; absent when this pane shows a workspace session. */
  readonly value?: ValueBinding;
  readonly terminal?: true;
  /** Active tab's durable history identity; independent of pane label and process id. */
  readonly history?: string;
  /** The sole terminal representation; required for a terminal pane, even with one tab. */
  readonly terminalTabs?: readonly TerminalTab[];
}
/** Derived rendering input. Per-tab fields are never stored on the pane itself. */
export interface PaneView extends Pane {
  readonly cwd?: string;
  readonly terminalTarget?: TerminalTarget;
  readonly terminalLaunch?: boolean;
}
export interface SplitState {
  /** Stable workspace identities travel with their saved pane/tab layout. */
  readonly workspaceBindings?: Readonly<Record<string, string>>;
  readonly version: 1; readonly nextId: number; readonly panes: readonly Pane[]; readonly focused: string;
  readonly remembered: Readonly<Record<string, string>>;
  readonly axis: Axis; readonly layout: Layout; readonly visited: readonly string[];
}
export function max_panes():number { return budget("ui.panes"); }
export const SESSION_PANE: Pane = { id: "p1", title: "session" };
export const historyId = (value: unknown): value is string => typeof value === "string" && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value) && value !== "00000000-0000-0000-0000-000000000000";
/** Only an explicit creation gesture allocates a terminal identity. */
export function terminalPane(id: string, tab: Omit<TerminalTab, "history"> = {}): Pane {
  const history = crypto.randomUUID();
  return { id, title: "xterm", terminal: true, history, terminalTabs: [{ history, ...tab }] };
}
export function oneP(pane: Pane): SplitState {
  return { version: 1, nextId: Number(pane.id.slice(1)) + 1, panes: [pane], focused: pane.id, remembered: {}, axis: "right", layout: { pane: pane.id }, visited: [pane.id] };
}
export function nextPane(state: SplitState): string {
  return `p${state.nextId}`;
}
function replace(tree: Layout, id: string, replacement: Layout): Layout {
  return "pane" in tree ? tree.pane === id ? replacement : tree
    : { ...tree, first: replace(tree.first, id, replacement), second: replace(tree.second, id, replacement) };
}
export function focus(state: SplitState, id: string): SplitState {
  if (state.focused === id || !state.panes.some(p => p.id === id)) return state;
  return { ...state, focused: id, visited: [...state.visited.filter(p => p !== id), id] };
}
/** Only the source leaf changes. Refusing at capacity must never replace existing work. */
export function splitPane(state: SplitState, direction: SplitDirection, pane: Pane,
  takeFocus = false, source = state.focused): SplitState {
  if (state.panes.length >= max_panes() || state.panes.some(p => p.id === pane.id) || !state.panes.some(p => p.id === source)) return state;
  const inherited = state.panes.find(p => p.id === source)?.workspace;
  if (pane.workspace === undefined && inherited !== undefined) pane = { ...pane, workspace: inherited };
  const axis = direction === "left" || direction === "right" ? "right" : "down";
  const before = direction === "left" || direction === "up";
  const previous = { pane: source }, added = { pane: pane.id };
  const layout = replace(state.layout, source, { axis, ratio: 0.5, first: before ? added : previous, second: before ? previous : added });
  const next: SplitState = { ...state, layout, axis: "axis" in layout ? layout.axis : state.axis,
    nextId: Math.max(state.nextId, Number(pane.id.slice(1)) + 1), panes: [...state.panes, pane], remembered: pane.command ? { ...state.remembered, [rememberedKey(pane.command, pane.workspace)]: pane.id } : state.remembered };
  return focus(next, takeFocus ? pane.id : source);
}
export function open(state: SplitState, pane: Pane, axis: Axis = "right"): SplitState {
  if (state.panes.some(p => p.id === pane.id)) return focus({ ...state,
    panes: state.panes.map(p => p.id === pane.id ? pane : p),
    remembered: pane.command ? { ...state.remembered, [rememberedKey(pane.command, pane.workspace)]: pane.id } : state.remembered }, pane.id);
  return splitPane(state, axis, pane, true);
}
/** Compatibility with /split: add up to the requested count, without discarding existing panes. */
export function divide(state: SplitState, how: Axis | 3 | 4): SplitState {
  if (how === "right" || how === "down") return splitPane(state, how, { id: nextPane(state), title: "session" });
  let next = state;
  while (next.panes.length < how) {
    const boxes = rectangles(next);
    const largest = [...boxes].sort((a, b) => b.width * b.height - a.width * a.height)[0]!;
    next = splitPane(next, largest.width > largest.height ? "right" : "down", { id: nextPane(next), title: "session" }, false, largest.id);
  }
  return focus(next, state.focused);
}
export function arrangement(state: SplitState): 1 | 2 | 3 | 4 { return state.panes.length as 1 | 2 | 3 | 4; }
function rememberedKey(command: string, workspace?: string): string {
  return workspace === undefined ? command : JSON.stringify([workspace, command]);
}
export function paneFor(state: SplitState, command: string, workspace?: string): string | undefined {
  const id = state.remembered[rememberedKey(command, workspace)];
  return state.panes.some(p => p.id === id && p.workspace === workspace) ? id : undefined;
}
export function openRemembered(state: SplitState, pane: Pane, axis: Axis = "right"): SplitState {
  return open(state, { ...pane, id: (pane.command && paneFor(state, pane.command, pane.workspace)) || pane.id }, axis);
}
export function titleOf(shown: Shown | undefined): string {
  if (!shown) return "session";
  return `/${shown.screen}${shown.node !== undefined ? ` $${shown.node}` : shown.section !== undefined ? ` ${shown.section}` : ""}`;
}
export function sendToPane(state: SplitState, shown: Shown): SplitState {
  const workspace = state.panes.find(p => p.id === state.focused)?.workspace;
  return openRemembered(state, { id: paneFor(state, shown.screen, workspace) ?? nextPane(state), title: titleOf(shown), command: shown.screen, shows: shown, ...(workspace === undefined ? {} : { workspace }) });
}
export function clearPane(state: SplitState, id: string): SplitState {
  return { ...state, panes: state.panes.map(p => {
    if (p.id !== id || p.terminal) return p;
    if (p.value && p.tabs) {
      const tabs = p.tabs.filter(tab => tab.value?.node !== p.value!.node || tab.value?.generation !== p.value!.generation || tab.value?.related !== p.value!.related || tab.workspace !== p.workspace);
      const next = tabs.find(tab => !tab.value) ?? tabs[0];
      if (next) return { ...p, tabs, workspace: next.workspace, value: next.value };
    }
    return { id, title: "session", ...(p.tabs && !p.value ? { tabs: p.tabs } : {}), ...(p.workspace === undefined ? {} : { workspace: p.workspace }) };
  }) };
}
function remove(tree: Layout, id: string): Layout | undefined {
  if ("pane" in tree) return tree.pane === id ? undefined : tree;
  const first = remove(tree.first, id), second = remove(tree.second, id);
  return !first ? second : !second ? first : { ...tree, first, second };
}
export function close(state: SplitState, id: string): SplitState {
  if (state.panes.length <= 1 || !state.panes.some(p => p.id === id)) return state;
  // Screens can return to their wes prompt; terminal and related-work panes cannot.
  // Keep the last command context intact, including its current screen, draft and layout.
  const sessions = state.panes.filter(p => !p.terminal && (!p.value || p.tabs?.some(tab => !tab.value)));
  if (sessions.length === 1 && sessions[0]!.id === id) return state;
  const panes = state.panes.filter(p => p.id !== id);
  const layout = remove(state.layout, id)!;
  const next = { ...state, panes, layout, axis: "axis" in layout ? layout.axis : "right" as Axis,
    remembered: Object.fromEntries(Object.entries(state.remembered).filter(([, v]) => v !== id)),
    visited: state.visited.filter(p => p !== id) };
  const neighbor = rectangles(state).filter(p => p.id !== id).sort((a, b) => {
    const origin = rectangles(state).find(p => p.id === id)!;
    return Math.hypot(a.x + a.width / 2 - origin.x - origin.width / 2, a.y + a.height / 2 - origin.y - origin.height / 2)
      - Math.hypot(b.x + b.width / 2 - origin.x - origin.width / 2, b.y + b.height / 2 - origin.y - origin.height / 2);
  })[0]!;
  return state.focused === id ? focus(next, neighbor.id) : next;
}
export function cycle(state: SplitState, step: 1 | -1): SplitState {
  return focus(state, state.panes[(state.panes.findIndex(p => p.id === state.focused) + step + state.panes.length) % state.panes.length]!.id);
}
export function focusNth(state: SplitState, n: number): SplitState { return state.panes[n - 1] ? focus(state, state.panes[n - 1]!.id) : state; }
export interface Rect { id: string; x: number; y: number; width: number; height: number }
export function rectangles(state: SplitState): Rect[] {
  const result: Rect[] = [];
  const visit = (node: Layout, x: number, y: number, width: number, height: number) => {
    if ("pane" in node) { result.push({ id: node.pane, x, y, width, height }); return; }
    if (node.axis === "right") {
      visit(node.first, x, y, width * node.ratio, height);
      visit(node.second, x + width * node.ratio, y, width * (1 - node.ratio), height);
    } else {
      visit(node.first, x, y, width, height * node.ratio);
      visit(node.second, x, y + height * node.ratio, width, height * (1 - node.ratio));
    }
  };
  visit(state.layout, 0, 0, 1, 1); return result;
}
export function navigate(state: SplitState, direction: SplitDirection): SplitState {
  const boxes = rectangles(state), origin = boxes.find(p => p.id === state.focused)!;
  const horizontal = direction === "left" || direction === "right";
  const leading = direction === "left" || direction === "up";
  const edge = horizontal ? origin.x + (leading ? 0 : origin.width) : origin.y + (leading ? 0 : origin.height);
  const neighbors = boxes.filter(p => p.id !== origin.id &&
    Math.abs((horizontal ? p.x + (leading ? p.width : 0) : p.y + (leading ? p.height : 0)) - edge) < 1e-8 &&
    (horizontal ? Math.min(p.y + p.height, origin.y + origin.height) - Math.max(p.y, origin.y)
      : Math.min(p.x + p.width, origin.x + origin.width) - Math.max(p.x, origin.x)) > 1e-8);
  neighbors.sort((a, b) => state.visited.indexOf(b.id) - state.visited.indexOf(a.id) || a.y - b.y || a.x - b.x);
  return neighbors[0] ? focus(state, neighbors[0].id) : state;
}
/** Flat keyed grid keeps terminals/editors mounted when ancestors in the tree change. */
export function geometry(state: SplitState) {
  const boxes = rectangles(state);
  // Nested ratios can express the same shared edge as e.g. 0.2077 and
  // 0.20770000000000002. Merge roundoff before creating grid tracks: that
  // phantom interval has no representable interior point or owning pane.
  const edges = (values: number[]) => values.sort((a, b) => a - b)
    .filter((value, i, sorted) => i === 0 || value - sorted[i - 1]! > 1e-12);
  const xs = edges(boxes.flatMap(p => [p.x, p.x + p.width]));
  const ys = edges(boxes.flatMap(p => [p.y, p.y + p.height]));
  const rows = ys.slice(0,-1).map((y, j) => xs.slice(0,-1).map((x, i) => {
    const cx = (x + xs[i+1]!) / 2, cy = (y + ys[j+1]!) / 2;
    return boxes.find(p => cx > p.x && cx < p.x + p.width && cy > p.y && cy < p.y + p.height)!.id;
  }).join(" "));
  const tracks = (values: number[]) => values.slice(1).map((v,i) => `minmax(0, ${v-values[i]!}fr)`).join(" ");
  return { gridTemplateAreas: rows.map(r => `"${r}"`).join(" "), gridTemplateColumns: tracks(xs), gridTemplateRows: tracks(ys) };
}
export function template(state: SplitState): string { return geometry(state).gridTemplateAreas; }
