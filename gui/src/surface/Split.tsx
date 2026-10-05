/** A stable grid for session, terminal and screen panes, with spatial keyboard focus. */
import { useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode, useEffect } from "react";
import { CapacityStatus } from "./CapacityStatus";
import { ApplicationLogs } from "./ApplicationLogs";
import { focusPaneInput, paneView, visibleTarget } from "./pane-focus";
import { MonoLine, type Segment } from "./MonoLine";
import { chordKey, composing } from "../platform-keys";
import { arrangement, clearPane, close, cycle, focus, focusNth, geometry, navigate, type Pane, type PaneView, type SplitState } from "./split-model";
import "./surface.css";
import "./split.css";
import { allTerminals, max_terminal_tabs, terminalTabs, selectTerminalTab, closeTerminalTab } from "./terminal-tabs";
import { activeView, paneTabs, viewKey, tabViews, selectViewTab, closeViewTab } from "./workspace-tabs";

export interface SplitProps {
  readonly capacity?: import("../protocol").ExecutionCapacity;
  readonly originWorkspace?: string;
  readonly active?: boolean;
  readonly state: SplitState;
  /** What each pane holds. A pane with nothing to draw still says what it is. */
  readonly content?: (pane: PaneView) => ReactNode;
  readonly top: readonly Segment[];
  readonly prompt: readonly Segment[];
  readonly context: readonly Segment[];
  /** Summary of the workspace in the focused pane. */
  readonly status?: readonly Segment[];
  readonly onChange?: (state: SplitState) => void;
  readonly command?: (pane: Pane) => ReactNode;
  readonly onNewTerminalTab?: (pane: Pane) => void;
  /** Header shortcuts use the same navigation as their slash commands. */
  readonly onGraph?: () => void;
  readonly onSettings?: () => void;
}

/** `▸ session` when the caret is in it, `  /graph $orders` when it is not. */
export function paneTitle(pane: Pane, focused: boolean): Segment[] {
  return [
    { text: focused ? "▸ " : "  ", role: "mono-ref" },
    { text: pane.workspace === undefined ? pane.title : `${pane.workspace} · ${pane.title}`, role: focused ? "mono-ink" : "mono-dim" },
  ];
}

/** Navigation hints followed by the focused workspace summary. */
export function splitKeys(expandedPaneCount?: number, status?: readonly Segment[]): Segment[] {
  const dot: Segment = { text: "  ·  ", role: "mono-faint" };
  const keys: Segment[] = expandedPaneCount ? [
    { text: "esc", role: "mono-ref" }, { text: ` show all ${expandedPaneCount} panes`, role: "mono-dim" },
  ] : [];
  return status?.length ? [...keys, ...(keys.length ? [dot] : []), ...status] : keys;
}

/** `wes / sales-api · 3 panes` — the top line says how many there are. */
export function splitTop(top: readonly Segment[], panes: number): Segment[] {
  return [
    ...top.slice(0, -2),
    { text: "  ·  ", role: "mono-faint" },
    { text: `${panes} pane${panes === 1 ? "" : "s"}`, role: "mono-meta" },
  ];
}

/** Hidden retained sessions remain in the DOM, but never own keyboard focus. */

const tabLabel = (workspace?: string, origin?: string) => workspace ?? origin ?? "workspace";

/** A keyboard user's place in a tab row is theirs; a mouse click on a tab may hand focus on to the input. */
function tabRowHasKeyboardFocus(row: Element | null | undefined): boolean {
  const held = document.activeElement;
  return !!row && !!held && row.contains(held)
    && typeof CSS !== "undefined" && CSS.supports("selector(:focus-visible)") && held.matches(":focus-visible");
}

interface WorkspaceTabsProps {
  readonly originWorkspace?: string;
  readonly pane: Pane;
  readonly state: SplitState;
  readonly onChange?: (state: SplitState) => void;
  readonly onNewTerminalTab?: (pane: Pane) => void;
}

/**
 * A pane's workspace tabs: one mono line of names in the pane's own voice, the selected one in ink
 * with the pane's rail beneath it. Arrow keys, Home and End move between tabs (Enter or Space
 * selects); Delete closes the tab that has focus. Closing only removes the view.
 */
function WorkspaceTabs({ pane, state, onChange, onNewTerminalTab, originWorkspace }: WorkspaceTabsProps) {
  const row = useRef<HTMLDivElement>(null);
  const descriptors = paneTabs(pane);
  const valueLabel = (tab: typeof descriptors[number]) => `${tab.value?.related ? "related " : ""}$${tab.value?.label}`;
  const label = (tab: typeof descriptors[number]) => {
    if (!tab.value) return tabLabel(tab.workspace, originWorkspace);
    const same = descriptors.filter(other => other.value && valueLabel(other) === valueLabel(tab));
    if (same.length === 1) return valueLabel(tab);
    const workspace = tabLabel(tab.workspace, originWorkspace);
    return `${workspace} · ${valueLabel(tab)} · ${tab.value.node}${same.some(other => other !== tab && other.workspace === tab.workspace && other.value?.node === tab.value?.node) ? ` · ${tab.value.generation.slice(0, 8)}` : ""}`;
  };
  const tabs = pane.terminal
    ? terminalTabs(pane).map((tab, at) => ({ key: `terminal:${tab.history}`, name: `Terminal ${at + 1}`, selected: pane.history === tab.history,
        select: () => selectTerminalTab(state, pane.id, tab.history), close: () => closeTerminalTab(state, pane.id, tab.history) }))
    : descriptors.map(tab => ({ key: viewKey(tab), name: label(tab), selected: activeView(pane) === viewKey(tab),
        select: () => selectViewTab(state, pane.id, viewKey(tab)), close: () => closeViewTab(state, pane.id, viewKey(tab)) }));
  // A terminal's last tab closes with its pane; a workspace pane keeps its last tab.
  const closable = tabs.length > 1 || !!pane.terminal || !!pane.value;
  useLayoutEffect(() => {
    row.current?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
  }, [pane.workspace, pane.value, pane.history, tabs.length]);
  const tabElements = () => Array.from(row.current?.querySelectorAll<HTMLElement>('[role="tab"]') ?? []);
  const closeAt = (at: number, viaKeyboard: boolean) => {
    if (!closable) return;
    // Keyboard focus moves to the neighbour before the closed tab unmounts, so it is never dropped on the body.
    if (viaKeyboard) { const all = tabElements(); (all[at + 1] ?? all[at - 1])?.focus(); }
    onChange?.(tabs[at]!.close());
  };
  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    const all = tabElements();
    const at = all.indexOf(event.target as HTMLElement);
    if (at < 0) return;
    const move = (to: number) => { event.preventDefault(); event.stopPropagation(); all[(to + all.length) % all.length]?.focus(); };
    if (event.key === "ArrowRight") return move(at + 1);
    if (event.key === "ArrowLeft") return move(at - 1);
    if (event.key === "Home") return move(0);
    if (event.key === "End") return move(all.length - 1);
    if (event.key === "Delete" && closable) { event.preventDefault(); event.stopPropagation(); closeAt(at, true); }
  };
  return (
    <div className="workspace-tabs" role="tablist" aria-label={pane.terminal ? "Terminals" : paneTabs(pane).some(tab => tab.value) ? "Workspaces and values" : "Workspaces"} ref={row} onKeyDown={onKeyDown}>
      {tabs.map((tab, at) => {
        const { selected, name } = tab;
        return (
          <span className="workspace-tab-label" role="presentation" key={tab.key}>
            <button type="button" role="tab" className="workspace-tab" aria-description={name}
              id={`${pane.id}:tab:${tab.key}`} aria-controls={`${pane.id}:view:${tab.key}`}
              aria-selected={selected} tabIndex={selected ? 0 : -1}
              onClick={event => { event.stopPropagation(); onChange?.(tab.select()); }}>{name}</button>
            {closable && <button type="button" className="workspace-tab-close" aria-label={`Close ${name} tab`}
              disabled={!pane.terminal && tab.close() === state}
              tabIndex={selected ? 0 : -1}
              onClick={event => { event.stopPropagation(); closeAt(at, event.detail === 0); }}>×</button>}
          </span>
        );
      })}
      {pane.terminal && <button type="button" className="workspace-tab" aria-label="New terminal tab"
        disabled={allTerminals(state).length >= max_terminal_tabs()}
        onClick={event => { event.stopPropagation(); onNewTerminalTab?.(pane); }}>+</button>}
    </div>
  );
}

export function Split({ state, content, top, prompt, status, capacity, onChange, command, onNewTerminalTab, onSettings, onGraph, originWorkspace, active = true }: SplitProps) {
  const root = useRef<HTMLDivElement>(null);
  // Expansion changes only the viewport. Never replace/snapshot the authoritative layout
  // or unmount hidden panes: their tabs, drafts, streams and shells still belong to them.
  const [expandedId, setExpandedId] = useState<string>();
  const expandedPane = expandedId === state.focused && state.panes.length > 1 ? expandedId : undefined;
  useLayoutEffect(() => {
    if (expandedId && (expandedId !== state.focused || state.panes.length === 1)) setExpandedId(undefined);
  }, [expandedId, state.focused, state.panes.length]);
  useLayoutEffect(() => {
    if (!active) return;
    const pane = root.current && paneView(root.current, state.focused);
    if (!pane || tabRowHasKeyboardFocus(pane.closest(".split-pane")?.querySelector(".workspace-tabs"))) return;
    // Keyboard navigation always returns to input, not the last result/action clicked.
    // Focus already inside the pane came from a mouse click; leave that control alone.
    focusPaneInput(root.current!, state.focused);
  }, [state.focused, state.panes.find(p => p.id === state.focused)?.workspace, state.panes.find(p => p.id === state.focused) && activeView(state.panes.find(p => p.id === state.focused)!), state.panes.find(p => p.id === state.focused)?.history, active, expandedPane]);
  /*
   * `⌘L` and `⌘arrows` are heard at the document, not at this element: focus may be nowhere — on
   * the body, after a screen inside a pane has left — and a key pressed there reaches nothing
   * below the document. The same handler answers a React capture for the tests.
   */
  const onKeyDownCapture = (event: Pick<KeyboardEvent<HTMLElement>, "target" | "metaKey" | "altKey" | "ctrlKey" | "shiftKey" | "key" | "repeat" | "keyCode" | "nativeEvent" | "preventDefault" | "stopPropagation">) => {
    // A key inside an input method's composition, Escape included, belongs to the text being composed.
    if (!active || composing(event)) return;
    if ((event.target as HTMLElement | undefined)?.closest?.(".application-logs")) return;
    if (event.metaKey && event.shiftKey && !event.altKey && !event.ctrlKey && event.key.toLowerCase() === "f") {
      if (state.panes.length > 1) {
        event.preventDefault(); event.stopPropagation();
        if (!event.repeat) setExpandedId(was => was ? undefined : state.focused);
      }
      return;
    }
    // Restore before the pane's editor/completion/shell consumes Escape. The next
    // Escape belongs to that surface again, and must never close it in this event.
    if (expandedPane && event.key === "Escape" && !event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey) {
      event.preventDefault(); event.stopPropagation();
      setExpandedId(undefined);
      return;
    }
    if (!event.metaKey || event.altKey || event.ctrlKey || event.shiftKey) return;
    if (event.key.toLowerCase() === "l") {
      const pane = root.current && paneView(root.current, state.focused);
      const prompt = pane && visibleTarget(pane, ".prompt-field");
      if (active && prompt) {
        event.preventDefault(); event.stopPropagation();
        prompt.focus({ preventScroll: true });
      }
      return;
    }
    const direction = ({ ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" } as const)[event.key as "ArrowLeft"];
    if (!direction) return;
    event.preventDefault(); event.stopPropagation();
    onChange?.(navigate(state, direction));
  };
  const latestCapture = useRef(onKeyDownCapture);
  latestCapture.current = onKeyDownCapture;
  useEffect(() => {
    if (typeof document === "undefined" || typeof document.addEventListener !== "function") return;
    const listen = (event: globalThis.KeyboardEvent) => {
      // Inside this split the React capture below has already answered; only keys from elsewhere are new.
      if (root.current && event.target instanceof Node && root.current.contains(event.target)) return;
      latestCapture.current(event as unknown as KeyboardEvent<HTMLElement>);
    };
    document.addEventListener("keydown", listen, true);
    return () => document.removeEventListener("keydown", listen, true);
  }, []);
  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    // A surface that answered the key first has answered it: the editor's own `esc` is not ours.
    // A hidden split owns no keys, and a key inside a composition, Escape included, belongs to its text.
    if (event.defaultPrevented || !active || composing(event)) return;
    const act = (next: SplitState) => {
      event.preventDefault();
      event.stopPropagation();
      onChange?.(next);
    };
    /*
     * `esc` closes the screen in this pane and leaves the pane standing.
     *
     * A pane is where somebody put a surface; closing what is in it is not the same act as giving
     * the room back, and `/close` is the one that gives the room back.
     */
    if (event.key === "Escape" && (state.panes.find(p => p.id === state.focused)?.shows || state.panes.find(p => p.id === state.focused)?.value)) return act(clearPane(state, state.focused));
    if (event.ctrlKey && event.key === "Tab") return act(cycle(state, event.shiftKey ? -1 : 1));
    if (!event.altKey || event.ctrlKey || event.metaKey) return;
    /*
     * Native word movement/selection doesn't prevent default in a text field, and on a Mac Option
     * with a letter or digit types a character (`∑`, `¡`). The enclosing pane leaves those keys
     * with their editing owner.
     */
    const target = event.target as HTMLElement | undefined;
    if (target?.isContentEditable || target?.closest?.("input, textarea")) return;
    if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
      return act(cycle(state, event.key === "ArrowRight" ? 1 : -1));
    }
    if (event.shiftKey) return;
    const key = chordKey(event);
    if (key === "w") return act(close(state, state.focused));
    if (/^[1-4]$/.test(key)) return act(focusNth(state, Number(key)));
  };

  return (
    <div className="split" ref={root} onKeyDownCapture={onKeyDownCapture} onKeyDown={onKeyDown} tabIndex={0} aria-label={`${state.panes.length} panes`}>
      <div className="split-top surface-sunk">
        <MonoLine segments={splitTop(top, state.panes.length)} className="split-top-line" />
        {onGraph && <button type="button" className="split-top-action" aria-label="Graph" aria-description="/graph"
          onClick={event => { event.stopPropagation(); onGraph(); }}>
          <svg className="split-top-glyph" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false"
            fill="none" stroke="currentColor" strokeWidth="1.2">
            <path d="M4.6 7.2 10.4 3.8M4.6 8.8 10.4 12.2" strokeLinecap="round" />
            <circle cx="3" cy="8" r="1.8" /><circle cx="12" cy="3" r="1.8" /><circle cx="12" cy="13" r="1.8" />
          </svg>
        </button>}
        {onSettings && (
          <button type="button" className="split-top-action" aria-label="Settings" aria-description="/settings"
            onClick={event => { event.stopPropagation(); onSettings(); }}>
            <svg className="split-top-glyph" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false">
              <path fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round"
                d="M6.6 1.5h2.8l.4 1.8 1.3.7 1.7-.7 1.4 2.4-1.4 1.2v1.5l1.4 1.2-1.4 2.4-1.7-.7-1.3.7-.4 1.8H6.6l-.4-1.8-1.3-.7-1.7.7-1.4-2.4 1.4-1.2V6.9L1.8 5.7l1.4-2.4 1.7.7 1.3-.7z" />
              <circle fill="none" stroke="currentColor" strokeWidth="1.2" cx="8" cy="8" r="2.2" />
            </svg>
          </button>
        )}
      </div>

      <div className="split-panes" data-panes={expandedPane ? 1 : arrangement(state)} data-expanded-pane={expandedPane}
        style={expandedPane ? { gridTemplateAreas: `"${expandedPane}"`, gridTemplateColumns: "minmax(0, 1fr)", gridTemplateRows: "minmax(0, 1fr)" } : geometry(state)}>
        {state.panes.map((pane) => {
          const here = pane.id === state.focused;
          const plain = !pane.shows && !pane.value;
          return (
            <section
              key={pane.id}
              className={`split-pane ${plain ? "split-pane-plain" : here ? "surface-sunk" : "surface-terminal"}`}
              style={{ gridArea: pane.id }}
              data-pane-id={pane.id}
              hidden={expandedPane !== undefined && !here}
              data-terminal={pane.terminal ? "true" : undefined}
              aria-label={pane.workspace === undefined ? pane.title : `${pane.workspace} · ${pane.title}`}
              data-workspace={pane.workspace}
              aria-current={here ? "true" : undefined}
              tabIndex={0}
              onFocus={() => onChange?.(focus(state, pane.id))}
              onClick={() => onChange?.(focus(state, pane.id))}
            >
              {!plain && !pane.value && <MonoLine segments={paneTitle(pane, here)} className="split-pane-title" />}
              {(pane.tabs || pane.terminal || pane.value) && <WorkspaceTabs originWorkspace={originWorkspace} pane={pane} state={state} onChange={onChange} onNewTerminalTab={onNewTerminalTab} />}
              {tabViews(pane).map(view => <div className="workspace-tab-view" key={view.terminal ? `terminal:${view.history}` : viewKey(view)} tabIndex={-1} hidden={view.terminal ? view.history !== pane.history : viewKey(view) !== activeView(pane)}
                data-view-key={viewKey(view)} data-view-workspace={view.workspace ?? ""}
                {...((pane.tabs || pane.terminal || pane.value) ? { role: "tabpanel", id: `${pane.id}:view:${view.terminal ? `terminal:${view.history}` : viewKey(view)}`, "aria-labelledby": `${pane.id}:tab:${view.terminal ? `terminal:${view.history}` : viewKey(view)}` } : {})}>
                <div className="split-pane-body">{content?.(view)}</div>
                {command?.(view)}
              </div>)}
            </section>
          );
        })}
      </div>

      <div className="split-prompt">
        {/* A split whose panes carry their own prompts has no prompt of its own to draw. */}
        {prompt.length > 0 && <MonoLine segments={prompt} className="split-prompt-line" />}
        <div className="split-key-row"><div className="split-status-group"><MonoLine segments={splitKeys(expandedPane ? state.panes.length : undefined, status)} className="split-keys" /><CapacityStatus capacity={capacity} /></div><ApplicationLogs /></div>
      </div>
    </div>
  );
}
