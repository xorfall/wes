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
  const [workspace, setWorkspace] = useState<Workspace>(emptyWorkspace);
  const [cells, setCells] = useState<readonly Cell[]>([]);
  const [connection, setConnection] = useState<Connection>("connecting");
  const [generation, setGeneration] = useState<string>();
  const [trouble, setTrouble] = useState<string>();
  useEffect(
    () => engine.listen((event) => {
      setCells(previous => event.event === "workspace-closed" || event.event === "projection-unavailable" ? [] : sharedCellEvent(previous, event));
      if (event.event === "session") setGeneration(event.generation);
      setWorkspace((was) => apply(was, event));
    }, setTrouble, setConnection),
    [engine],
  );
  return { engine, workspace, cells, connection, generation, trouble };
}
