import { useEffect, useRef, useState } from "react";
import type { Engine } from "../engine";
import type { Workspace } from "../workspace";
import { readGraph } from "./graph-model";
import { GraphPreview } from "./graph-preview";
import { GraphScreen, type GraphProps } from "./screens/Graph";
import { GraphCanvas, type GraphCanvasControl } from "./screens/GraphCanvas";

type Props = Omit<GraphProps, "nodes" | "edges" | "cycles" | "selected" | "canvas" | "onFit"> & {
  workspace: Workspace; engine: Engine; generation?: string; selectedId?: string; onSelect(id: string): void;
};

/** The same graph controls and read policy in session panes and detached windows. */
export function GraphWorkbench({ workspace, engine, generation, selectedId, onSelect, ...props }: Props) {
  const control = useRef<GraphCanvasControl>(null);
  const focusRequested = useRef<string>();
  const [query, setQuery] = useState("");
  const graph = readGraph(workspace, { selected: selectedId, staleOnly: props.staleOnly, connectedOnly: props.connectedOnly });
  const selected = workspace.nodes.find(node => node.id === selectedId);
  const scope = JSON.stringify([workspace.identity?.name ?? engine.binding, generation, props.direction ?? "LR"]);
  const matches = query.trim() ? workspace.nodes.filter(node => `${node.name ?? ""} ${node.id}`.toLowerCase().includes(query.trim().toLowerCase())).slice(0, 12) : [];
  const relations = selected ? graphRelations(workspace, selected.id) : [];
  const select = (id: string) => {
    if (id === selectedId) { control.current?.focus(id); return; }
    focusRequested.current = id; onSelect(id);
  };
  useEffect(() => {
    if (!focusRequested.current) return;
    const id = focusRequested.current;
    focusRequested.current = undefined;
    const frame = requestAnimationFrame(() => control.current?.focus(id));
    return () => cancelAnimationFrame(frame);
  }, [selectedId]);
  return <GraphScreen {...props} {...graph}
    onRepeat={selected?.selectionSources?.length ? undefined : props.onRepeat}
    onFit={() => control.current?.fit()}
    onReset={() => control.current?.reset()}
    search={query} onSearch={setQuery}
    matches={matches.map(node => ({ id: node.id, name: node.name ? `$${node.name}` : node.id }))}
    onSelect={select}
    relations={relations}
    preview={selected ? <GraphPreview key={`${generation}:${selected.id}:${selected.handle}`} engine={engine} generation={generation} node={selected} /> : undefined}
    canvas={<GraphCanvas key={scope} ref={control} layoutId={scope} nodes={graph.nodes} edges={graph.edges} allNodeIds={workspace.nodes.map(node => node.id)} direction={props.direction ?? "LR"} onSelect={onSelect} />}
  />;
}

export function graphRelations(workspace: Workspace, id: string): { id: string; name: string; label: string }[] {
  const graph = readGraph(workspace);
  return graph.edges.filter(edge => edge.from === id || edge.to === id).flatMap(edge => {
    const incoming = edge.to === id;
    const other = workspace.nodes.find(node => node.id === (incoming ? edge.from : edge.to));
    if (!other) return [];
    const label = edge.kind === "selection" ? incoming ? "activated by selection in" : "selection activates"
      : incoming ? edge.constructed ? "created from" : edge.captured ? "captured input" : "input"
      : edge.constructed ? "created" : edge.captured ? "captured by" : "downstream";
    return [{ id: other.id, name: other.name ? `$${other.name}` : other.id, label }];
  });
}
