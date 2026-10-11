/**
 * A screen in a window of its own: the graph, or a settings section.
 *
 * The same client, told to draw one screen. It subscribes to the workspace on its own engine and
 * shares nothing else with the session: the graph's jump and repeat act on the session's cells
 * and stay in the session's graph screen; a choice made here is announced to the session, which
 * owns and persists the preferences. `esc` is the screen's own close, which here closes the window.
 */
import { useEffect, useRef, useState } from "react";
import { load, resolveSurfacePalette, type Settings, surfaceTypeStyle } from "../settings";
import { language, type Language } from "./language";
import { GraphWorkbench } from "./GraphWorkbench";
import { newCell } from "../cells";
import { SpecScreen } from "./screens/Spec";
import { SettingsScreen } from "./screens/Settings";
import { chose, previewOf, readSection, SECTIONS, sectionNamed } from "./settings-model";
import { announceSettings, changed, followSettings } from "./settings-channel";
import { useTableViewOwner } from "./table-view-owner";
import { openRoute, openDetached, screenRoute, type ScreenRoute } from "./open-route";
import { topLine, type SessionContext } from "./session-model";
import { useWindowWorkspace } from "./window-workspace";
import { Hints } from "./Hints";
import { WindowWorkspacePicker } from "./WindowWorkspacePicker";
import { MonoLine } from "./MonoLine";
import "./surface.css";

export function ScreenWindow({ route }: { readonly route: ScreenRoute }) {
  const { engine, workspace, connection, generation, trouble } = useWindowWorkspace(route.workspace);
  const submitted = useRef<{ source: string; cell: string; generation: string }>();
  const [settings, setSettings] = useState<Settings>(load);
  const [pack, setPack] = useState<Language>();
  const [chosenNode, setChosenNode] = useState<string>();
  const [staleOnly, setStaleOnly] = useState(route.screen === "stale");
  const [connectedOnly, setConnectedOnly] = useState(false);
  useEffect(() => { void language().then(setPack); }, []);
  // Another window's choice reaches this one too, so two open settings windows agree.
  useEffect(() => followSettings((change) => setSettings((was) => ({ ...was, ...change }))), []);
  useTableViewOwner(settings.tables, (tables) => { announceSettings({ tables }); setSettings((was) => ({ ...was, tables })); });

  const context: SessionContext = { workspace: workspace.identity?.name ?? "workspace", connection };
  const top = topLine(context);
  const close = () => window.close();
  const submitSpec = async (source: string): Promise<string> => {
    if (!generation || connection !== "connected") throw new Error("Wait for the workspace connection before submitting.");
    if (submitted.current && submitted.current.generation !== generation) throw new Error("Workspace changed; reopen /spec before submitting.");
    // A lost reply retains its attempt identity, so retry cannot duplicate an accepted command.
    if (submitted.current?.source !== source) submitted.current = { source, cell: newCell(source).lastRun, generation };
    const attempt = submitted.current;
    await engine.submit(attempt.cell, source);
    return attempt.cell;
  };
  const choose = (next: Settings) => {
    announceSettings(changed(settings, next));
    setSettings(next);
  };

  // The window opened on a section; its tabs move between them without a new window.
  const [section, setSection] = useState(() => sectionNamed(route.section));
  useEffect(() => { setSection(sectionNamed(route.section)); }, [route.section]);
  useEffect(() => { setStaleOnly(route.screen === "stale"); }, [route.screen]);
  useEffect(() => {
    document.title = `${route.screen === "settings" ? `/settings ${section}` : route.screen === "spec" ? "/spec" : "/graph"} · wes`;
  }, [route.screen, section]);

  const palette = settings.surfacePalette === "system" ? resolveSurfacePalette("system") : settings.surfacePalette;
  const selectedNode = workspace.nodes.some(node => node.id === chosenNode) ? chosenNode : workspace.nodes[workspace.nodes.length - 1]?.id;
  const view = readSection(section, { settings, workspace, ...(pack ? { pack } : {}) });

  return (
    <div
      className="wes-terminal surface-terminal surface-app"
      data-palette={palette}
      data-density={settings.surfaceDensity}
      data-focus={settings.surfaceFocus}
      style={surfaceTypeStyle(settings)}
    >
      <Hints />
      <WindowWorkspacePicker key={route.workspace} binding={route.workspace} workspace={workspace} onChoose={name => window.__WES_RETARGET__?.(screenRoute(route.screen === "graph" || route.screen === "stale" ? staleOnly ? "stale" : "graph" : route.screen, name, section))} />
      {route.screen === "spec" ? <SpecScreen {...(workspace.identity?.name&&generation?{binding:{workspace:workspace.identity.name,generation}}:{})} top={top} onClose={close} onSubmit={submitSpec} /> : route.screen === "settings" ? (
        <SettingsScreen
          top={top}
          sections={[...SECTIONS]}
          section={section}
          rows={view.rows}
          facts={view.facts}
          {...(view.empty === undefined ? {} : { empty: view.empty })}
          {...(section === "appearance" ? { preview: previewOf(settings) } : {})}
          onChoose={(row, option) => choose(chose(settings, row, option))}
          onSection={(name) => setSection(sectionNamed(name))}
          onClose={close}
        />
      ) : (
        <GraphWorkbench
          top={top} workspace={workspace} engine={engine} generation={generation} selectedId={selectedNode} onSelect={setChosenNode}
          staleOnly={staleOnly}
          connectedOnly={connectedOnly}
          direction={settings.direction}
          onStaleOnly={setStaleOnly}
          onConnectedOnly={setConnectedOnly}
          onDirection={(direction) => choose({ ...settings, direction })}
          onOpenResult={() => { if (selectedNode) openDetached(openRoute(selectedNode, "result", engine.binding ?? route.workspace ?? workspace.identity?.name)); }}
          onClose={close}
        />
      )}
      {trouble && <MonoLine segments={[{ text: trouble, role: "mono-bad" }]} className="surface-trouble" />}
    </div>
  );
}
