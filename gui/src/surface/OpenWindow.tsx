/**
 * One result, in a window of its own.
 *
 * The same client and result screen, drawing one
 * node instead of the session. It keeps its own subscription rather than sharing the session's,
 * because it is a different document — a second tab in a browser, a second window in the desktop
 * app — and the two cannot share anything but the engine.
 *
 * The session it came from is untouched: it was never navigated, never re-rendered, and still has
 * whatever was half-typed at its prompt. `esc` closes this window and gives the focus back.
 */
import { useEffect, useState } from "react";
import { useWindowWorkspace } from "./window-workspace";
import { load, resolveSurfacePalette, type Settings, surfaceTypeStyle } from "../settings";
import { useResults } from "./results";
import type { ViewSubject } from "./result-views";
import { ObservationStatus } from "./ObservationStatus";
import { ReadStatus } from "./ReadStatus";
import { MonoLine } from "./MonoLine";
import { OpenScreen, type OpenTab } from "./screens/Open";
import { peekOf, PeekScreen } from "./screens/Peek";
import { Hints } from "./Hints";
import { couldNotDraw, readOpen } from "./open-model";
import { topLine, type SessionContext } from "./session-model";
import type { OpenRoute } from "./open-route";
import { announceSettings, followSettings } from "./settings-channel";
import { useTableViewOwner } from "./table-view-owner";
import "./surface.css";

export function OpenWindow({ route }: { readonly route: OpenRoute }) {
  const { engine, workspace, cells, connection, generation, trouble } = useWindowWorkspace(route.workspace);
  const [tab, setTab] = useState<OpenTab>(route.tab);
  const [settings, setSettings] = useState<Settings>(load);
  // Arrangements made here reach the session, which persists them; the session's reach here.
  useEffect(() => followSettings((change) => setSettings((was) => ({ ...was, ...change }))), []);
  useTableViewOwner(settings.tables, (tables) => { announceSettings({ tables }); setSettings((was) => ({ ...was, tables })); });

  const cell = route.cell === undefined ? undefined : cells.find(it => it.id === route.cell);
  const node = route.node === undefined ? undefined : workspace.nodes.find((it) => it.id === route.node || it.name === route.node);
  const handle = node?.handle;
  const { held, reads, observations, retry } = useResults(engine, generation, handle ? [handle] : [], node ? [node] : []);
  const observation = node ? observations.get(node.id) : undefined;
  const stored = observation?.value ?? (handle ? held.get(handle) : undefined);

  /* The window's own name in the switcher and the dock, which is what somebody is looking for. */
  useEffect(() => {
    document.title = cell ? `${cell.id} · source · wes` : node ? `${node.name ? `$${node.name}` : node.id} · wes` : "wes";
  }, [node, cell]);

  /* What every view is asked about. The engine is here too: a traced node's trace is still coming. */
  const viewing: ViewSubject = {
    ...(stored ? { value: stored } : {}),
    ...(node ? { node } : {}),
    engine,
    ...(generation ? { generation } : {}),
  };

  const context: SessionContext = {
    workspace: workspace.identity?.name ?? "workspace",
    environment: node?.environment?.environment,
    connection,
  };
  /*
   * The window may be aimed at a result that is not there.
   *
   * It is a document of its own with its own subscription, so between the session opening it and
   * this drawing, the workspace can have been reloaded and the result let go. Until the first
   * `session` event there is simply nothing yet, which is waiting rather than absence — the two
   * read differently and are said differently.
   */
  const drawn = readOpen({
    ...(node ? { node } : {}),
    workspace,
    context,
    client: engine.client,
    ...(stored ? { stored } : {}),
  });
  const missing = (route.cell === undefined ? node === undefined : cell === undefined) && connection === "connected" && workspace.identity !== undefined;
  const palette = settings.surfacePalette === "system" ? resolveSurfacePalette("system") : settings.surfacePalette;

  return (
    <div
      className="wes-terminal surface-terminal surface-app"
      data-palette={palette}
      data-density={settings.surfaceDensity}
      style={surfaceTypeStyle(settings)}
    >
      <Hints />
      {route.peek ? <PeekScreen engine={engine}
        top={topLine(context)}
        subject={missing ? couldNotDraw("subject", `no ${route.cell === undefined ? "result" : "cell"} called ${route.cell ?? route.node} is here any more`) : [{ text: node?.name ? `$${node.name}` : route.cell ?? route.node ?? "", role: "mono-ref" }]}
        what={route.peek}
        {...peekOf(node, stored)}
        {...(route.cell === undefined ? {} : { source: cell?.document?.source ?? cell?.text })}
        readStatus={observation && observation.state !== "current" ? <div className="result-observation"><ObservationStatus observation={observation} onRetry={handle ? () => retry(handle) : undefined} /></div> : handle && !stored ? <ReadStatus problem={reads.get(handle)?.problem} onRetry={() => retry(handle)} /> : undefined}
        onClose={() => window.close()}
      /> : <OpenScreen
        top={topLine(context)}
        subject={missing ? couldNotDraw("subject", `no result called ${route.node} is here any more`) : drawn.subject}
        tab={tab}
        onTab={setTab}
        value={stored}
        viewing={viewing}
        json={drawn.json}
        details={drawn.details}
        readStatus={observation && observation.state !== "current" ? <div className="result-observation"><ObservationStatus observation={observation} onRetry={handle ? () => retry(handle) : undefined} /></div> : handle && !stored ? <ReadStatus problem={reads.get(handle)?.problem} onRetry={() => retry(handle)} /> : undefined}
        onClose={() => window.close()}
      />}
      {trouble && <MonoLine segments={[{ text: trouble, role: "mono-bad" }]} className="surface-trouble" />}
    </div>
  );
}
