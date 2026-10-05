import { memo, useCallback, useEffect, useLayoutEffect, useState, type Dispatch, type ReactNode, type SetStateAction } from "react";
import type { Engine } from "../engine";
import type { Settings } from "../settings";
import { restoreWorkspace } from "../workspace-binding";
import type { Pane, PaneView, SplitState } from "./split-model";
import { SurfaceApp } from "./SurfaceApp";
import { MonoLine } from "./MonoLine";

export type LayoutChange = SplitState | ((state: SplitState) => SplitState);
export interface WorkspaceHost {
  readonly originWorkspace?: string;
  readonly split: SplitState;
  readonly settings: Settings;
  readonly settingsChange: Dispatch<SetStateAction<Settings>>;
  readonly change: (change: LayoutChange) => Promise<void>;
  readonly engine: (name: string) => Engine;
}
export interface WorkspaceSurface {
  readonly capacity?: import("../protocol").ExecutionCapacity;
  readonly openGraph?: () => void;
  readonly status?: readonly import("./MonoLine").Segment[];
  readonly top?: readonly import("./MonoLine").Segment[];
  readonly newTerminalTab: (pane: Pane) => void;
  readonly content: (pane: PaneView) => ReactNode;
  readonly command: (pane: Pane) => ReactNode;
}

/** One retained controller/subscription per explicit workspace, independent of pane lifetime. */
export function WorkspaceSurfaces({ names, host, children }: {
  readonly names: readonly string[]; readonly host: WorkspaceHost;
  readonly children: (views: ReadonlyMap<string, WorkspaceSurface>) => ReactNode;
}) {
  const [views, setViews] = useState<ReadonlyMap<string, WorkspaceSurface>>(new Map());
  const publish = useCallback((name: string, surface: WorkspaceSurface) => {
    setViews(previous => new Map(previous).set(name, surface));
  }, []);
  // Controllers are siblings of the grid: adding a name never reparents existing panes.
  return <>{names.map(name => <BoundWorkspace key={JSON.stringify([name, host.split.workspaceBindings?.[name]])} name={name} host={host} publish={publish} />)}{children(views)}</>;
}

const BoundWorkspace = memo(function BoundWorkspace({ name, host, publish }: { readonly name: string; readonly host: WorkspaceHost;
  readonly publish: (name: string, surface: WorkspaceSurface) => void }) {
  const [status, setStatus] = useState<string>();
  const identity = host.split.workspaceBindings?.[name];
  const [confirmed, setConfirmed] = useState<string>();
  const ready = identity !== undefined && confirmed === identity;
  const receive = useCallback((surface: WorkspaceSurface) => { publish(name, surface); }, [name, publish]);
  useEffect(() => {
    let alive = true, pending = false, openedSuccessfully = false;
    setStatus(undefined);
    const reopen = () => {
      if (pending || openedSuccessfully) return;
      pending = true;
      void restoreWorkspace(name, identity).then(() => {
        if (alive) { openedSuccessfully = true; setConfirmed(identity); }
      }, error => { if (alive) setStatus(String(error.message ?? error)); }).finally(() => { pending = false; });
    };
    const opened = (event: Event) => { if ((event as CustomEvent<string>).detail === name) reopen(); };
    reopen();
    window.addEventListener?.("wes-workspace-opened", opened);
    return () => { alive = false; window.removeEventListener?.("wes-workspace-opened", opened); };
  }, [name, identity]);
  useLayoutEffect(() => {
    if (!ready) receive({ newTerminalTab: () => {}, command: () => null, content: () => <MonoLine
      segments={[{ text: status ?? `Opening workspace ${name}…`, role: status ? "mono-bad" : "mono-dim" }]} /> });
  }, [ready, receive, status, name]);
  return ready ? <SurfaceApp key={identity} binding={name} host={host} renderWorkspace={receive} /> : null;
});
