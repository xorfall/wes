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
import { readGraph } from "./graph-model";
import { useResults } from "./results";
import { GraphScreen } from "./screens/Graph";
import { GraphCanvas } from "./screens/GraphCanvas";
import { newCell } from "../cells";
import { SpecScreen } from "./screens/Spec";
import { SettingsScreen } from "./screens/Settings";
import { chose, previewOf, readSection, SECTIONS, sectionNamed } from "./settings-model";
import { announceSettings, changed, followSettings } from "./settings-channel";
import { useTableViewOwner } from "./table-view-owner";
import { openRoute, type ScreenRoute } from "./open-route";
import { topLine, type SessionContext } from "./session-model";
import { useWindowWorkspace } from "./window-workspace";
import { Hints } from "./Hints";
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

  const handles = (route.screen === "spec" ? [] : workspace.nodes).flatMap((node) => (node.handle ? [node.handle] : []));
  const { held } = useResults(engine, generation, handles);
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
  useEffect(() => {
    document.title = `${route.screen === "settings" ? `/settings ${section}` : route.screen === "spec" ? "/spec" : "/graph"} · wes`;
  }, [route.screen, section]);

  const palette = settings.surfacePalette === "system" ? resolveSurfacePalette("system") : settings.surfacePalette;
  const selectedNode = chosenNode ?? workspace.nodes[workspace.nodes.length - 1]?.id;
  const graph = readGraph(workspace, { selected: selectedNode, staleOnly, connectedOnly, held });
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
      {route.screen === "spec" ? <SpecScreen key={`${workspace.identity?.name}:${generation}`} {...(workspace.identity?.name&&generation?{binding:{workspace:workspace.identity.name,generation}}:{})} top={top} onClose={close} onSubmit={submitSpec} /> : route.screen === "settings" ? (
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
        <GraphScreen
          top={top}
          nodes={graph.nodes}
          edges={graph.edges}
          cycles={graph.cycles}
          {...(graph.selected ? { selected: graph.selected } : {})}
          staleOnly={staleOnly}
          connectedOnly={connectedOnly}
          {...(graph.hidden ? { hidden: graph.hidden } : {})}
          direction={settings.direction}
          onStaleOnly={setStaleOnly}
          onConnectedOnly={setConnectedOnly}
          onDirection={(direction) => choose({ ...settings, direction })}
          onOpenResult={() => { if (selectedNode) window.open(openRoute(selectedNode, "result", route.workspace), "_blank"); }}
          onClose={close}
          canvas={<GraphCanvas nodes={graph.nodes} edges={graph.edges} direction={settings.direction} onSelect={setChosenNode} />}
        />
      )}
      {trouble && <MonoLine segments={[{ text: trouble, role: "mono-bad" }]} className="surface-trouble" />}
    </div>
  );
}
