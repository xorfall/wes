/**
 * A window of its own is a document of its own: it subscribes to the workspace on an engine
 * connection of its own and shares nothing else with the session. Result windows and screen
 * windows both start here.
 */
import { useEffect, useMemo, useState } from "react";
import { Engine } from "../engine";
import { apply, emptyWorkspace, type Workspace } from "../workspace";
import type { Cell } from "../cells";
import { sharedCellEvent } from "./component-model";

export type Connection = "connecting" | "connected" | "reconnecting";

export function useWindowWorkspace(binding?: string) {
  const engine = useMemo(() => new Engine(binding), [binding]);
  type Snapshot = { engine: Engine; workspace: Workspace; cells: readonly Cell[]; connection: Connection; generation?: string; trouble?: string };
  const fresh = (): Snapshot => ({ engine, workspace: emptyWorkspace, cells: [], connection: "connecting" });
  const [snapshot, setSnapshot] = useState<Snapshot>(fresh);
  useEffect(() => {
    let current = true;
    setSnapshot(fresh());
    const stop = engine.listen(event => {
      if (!current) return;
      setSnapshot(previous => {
        const was = previous.engine === engine ? previous : fresh();
        const cleared = event.event === "workspace-closed" || event.event === "projection-unavailable";
        return { ...was, workspace: apply(was.workspace, event),
          cells: cleared ? [] : sharedCellEvent(was.cells, event),
          generation: event.event === "session" ? event.generation : cleared ? undefined : was.generation };
      });
    }, trouble => { if (current) setSnapshot(was => ({ ...(was.engine === engine ? was : fresh()), trouble })); },
    connection => { if (current) setSnapshot(was => ({ ...(was.engine === engine ? was : fresh()), connection })); });
    return () => { current = false; stop(); };
  }, [engine]);
  // The binding changes during render, before cleanup. Never expose the previous binding for one frame.
  return snapshot.engine === engine ? snapshot : fresh();
}
