/**
 * The graph's canvas: xyflow and dagre dressed in the surface's roles.
 *
 * Dagre supplies the initial arrangement; manual positions are presentation state only.
 */
import { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { Background, MarkerType, MiniMap, Position, ReactFlow, type Edge, type Node as FlowNode, type NodeChange, type ReactFlowInstance } from "@xyflow/react";
import dagre from "@dagrejs/dagre";
import "@xyflow/react/dist/style.css";
import type { GraphEdge, GraphNode } from "./Graph";

/** The narrowest a node is drawn, and the widest it is allowed to grow to hold its sub line. */
export const NODE_WIDTH = 150;
export const NODE_MAX_WIDTH = 240;
const NODE_HEIGHT = 44;
/** What the node spends on its own padding and hairline, either side. */
const NODE_CHROME = 18;

/**
 * How wide a run of text is in the node's face.
 *
 * Measured where there is a canvas to measure with, and computed from the advance where there is
 * not — both lines are set in the mono face, so an advance is an answer and not an estimate. The
 * fallback is what a test tree gets, and a layout that could not be laid out without a browser
 * would be a layout nothing could check.
 */
const MONO_ADVANCE = 0.6;
let ruler: CanvasRenderingContext2D | null | undefined;

export function textWidth(text: string, size: number): number {
  if (ruler === undefined) {
    ruler = typeof document === "undefined" ? null : document.createElement("canvas").getContext("2d");
  }
  if (ruler === null) return text.length * size * MONO_ADVANCE;
  ruler.font = `${size}px "PT Mono", "JetBrains Mono", ui-monospace, Menlo, monospace`;
  return ruler.measureText(text).width;
}

/**
 * How wide this node needs to be.
 *
 * A node grows to hold its sub line and stops at 240; past that the type is cut with an ellipsis,
 * because a node wider than that is a label and no longer a node. The layout reserves stable
 * 240px slots; drawing a shorter label does not move neighbouring nodes.
 */
export function widthOf(node: GraphNode): number {
  const name = textWidth(node.name, 13);
  const detail = node.detail === undefined ? 0 : textWidth(node.detail, 12.5);
  const wanted = Math.ceil(Math.max(name, detail)) + NODE_CHROME;
  return Math.min(NODE_MAX_WIDTH, Math.max(NODE_WIDTH, wanted));
}

export interface GraphCanvasProps {
  readonly nodes: readonly GraphNode[];
  readonly edges: readonly GraphEdge[];
  readonly direction: "LR" | "TB";
  readonly onSelect?: (id: string) => void;
  readonly layoutId?: string;
  /** Includes nodes hidden by filters; only genuinely removed nodes lose their positions. */
  readonly allNodeIds?: readonly string[];
}

export interface GraphCanvasControl {
  fit(): void;
  reset(): void;
  focus(id: string): void;
}

type Point = { x: number; y: number };
// Coordinates only, for a few recently opened graph scopes in this document. Nothing survives a restart.
const layouts = new Map<string, ReadonlyMap<string, Point>>();
const MAX_LAYOUTS = 16;
export function movedPositions(previous: ReadonlyMap<string, Point>, changes: readonly NodeChange[], live: ReadonlySet<string>): Map<string, Point> {
  const next = new Map([...previous].filter(([id]) => live.has(id)));
  for (const change of changes) if (change.type === "position" && change.position && live.has(change.id)
    && Number.isFinite(change.position.x) && Number.isFinite(change.position.y)) next.set(change.id, { ...change.position });
  return next;
}

const FIT = { maxZoom: 1, padding: 0.2 };
// These dimensions must also size the SVG; a CSS-only resize crops it and misplaces pointer input.
export const MINIMAP_SIZE = { width: 176, height: 112 };

/** Where dagre puts each node and how wide it is, given which way the graph is asked to grow. */
export function laidOut(
  nodes: readonly GraphNode[],
  edges: readonly GraphEdge[],
  direction: "LR" | "TB",
): Map<string, { x: number; y: number; width: number }> {
  const graph = new dagre.graphlib.Graph();
  graph.setDefaultEdgeLabel(() => ({}));
  graph.setGraph({ rankdir: direction, nodesep: 24, ranksep: 56 });
  const widths = new Map(nodes.map((node) => [node.id, widthOf(node)]));
  // Stable slots keep execution/state/type text updates from moving neighbouring nodes.
  for (const node of nodes) graph.setNode(node.id, { width: NODE_MAX_WIDTH, height: NODE_HEIGHT });
  for (const edge of edges) if (edge.from !== edge.to) graph.setEdge(edge.from, edge.to);
  dagre.layout(graph);
  const placed = new Map<string, { x: number; y: number; width: number }>();
  for (const node of nodes) {
    const at = graph.node(node.id);
    const width = widths.get(node.id)!;
    placed.set(node.id, { x: at.x - NODE_MAX_WIDTH / 2, y: at.y - NODE_HEIGHT / 2, width });
  }
  return placed;
}

export const GraphCanvas = forwardRef<GraphCanvasControl, GraphCanvasProps>(function GraphCanvas({ nodes, edges, direction, onSelect, layoutId, allNodeIds }: GraphCanvasProps, ref) {
  const instance = useRef<ReactFlowInstance>();
  const [manual, setManual] = useState<ReadonlyMap<string, Point>>(() => layoutId ? layouts.get(layoutId) ?? new Map() : new Map());
  const [resetVersion, setResetVersion] = useState(0);
  const live = new Set(allNodeIds ?? nodes.map(node => node.id));
  const structure = JSON.stringify([direction, nodes.map(node => node.id), edges.map(edge => [edge.from, edge.to, edge.kind])]);
  const placed = useMemo(() => laidOut(nodes, edges, direction), [structure]);
  const remember = (next: ReadonlyMap<string, Point>) => {
    setManual(next);
    if (!layoutId) return;
    layouts.delete(layoutId);
    if (next.size) layouts.set(layoutId, next);
    if (layouts.size > MAX_LAYOUTS) layouts.delete(layouts.keys().next().value!);
  };
  useEffect(() => {
    if (!resetVersion) return;
    // ReactFlow's child effects have received the restored node positions before this fit.
    const frame = requestAnimationFrame(() => { void instance.current?.fitView(FIT); });
    return () => cancelAnimationFrame(frame);
  }, [resetVersion]);
  useImperativeHandle(ref, () => ({
    fit: () => { void instance.current?.fitView(FIT); },
    reset: () => {
      remember(new Map());
      setResetVersion(version => version + 1);
    },
    focus: id => {
      const node = instance.current?.getNode(id);
      if (node) void instance.current?.setCenter(node.position.x + (node.width ?? NODE_WIDTH) / 2, node.position.y + NODE_HEIGHT / 2, { zoom: 1 });
    },
  }));
  const drawn = useMemo(() => {
    const flowNodes: FlowNode[] = nodes.map((node) => {
      const point = placed.get(node.id) ?? { x: 0, y: 0 };
      const at = { ...point, width: widthOf(node) };
      return {
      id: node.id,
      position: manual.get(node.id) ?? { x: at.x, y: at.y },
      selected: node.selected === true || node.state === "selected",
      sourcePosition: direction === "LR" ? Position.Right : Position.Bottom,
      targetPosition: direction === "LR" ? Position.Left : Position.Top,
      data: {
        state: node.state,
        selected: node.selected === true || node.state === "selected",
        label: (
          <span className="graph-node-said">
            <span className="graph-node-name">{node.name}</span>
            {node.detail && <span className="graph-node-detail">{node.detail}</span>}
          </span>
        ),
      },
      className: `graph-node graph-node-${node.state}${node.selected === true ? " graph-node-active" : ""}`,
      // Given rather than left to the browser: the minimap draws from these, and a measurement
      // that has not happened yet leaves it empty.
      width: at.width,
      height: NODE_HEIGHT,
      style: { width: at.width, height: NODE_HEIGHT },
      };
    });
    const flowEdges: Edge[] = edges.map((edge) => ({
      id: `${edge.from}>${edge.to}:${edge.kind ?? "dependency"}`,
      source: edge.from,
      target: edge.to,
      className: edge.kind === "selection" ? "graph-edge graph-edge-selection" : edge.lifetime === "creation" ? `graph-edge graph-edge-creation${edge.constructed ? " graph-edge-constructed" : ""}`
        : edge.captured ? "graph-edge graph-edge-captured" : "graph-edge",
      ...(edge.kind === "selection" ? { label: "selection", ariaLabel: "Explicit selection activates this call; source updates do not run it" } : edge.lifetime === "creation" ? { label: edge.constructed ? "created from" : "creates", ariaLabel: edge.constructed ? "Created from this input; refreshes do not re-create it" : "Construction waits for this input" }
        : edge.captured ? { label: "captured", ariaLabel: "Input captured when the run started; later updates do not change this run" } : {}),
      markerEnd: { type: MarkerType.ArrowClosed, width: 14, height: 14 },
    }));
    return { flowNodes, flowEdges };
  }, [nodes, edges, direction, manual, placed]);

  return (
    <ReactFlow
      nodes={drawn.flowNodes}
      edges={drawn.flowEdges}
      fitView
      /*
       * Fit, but never magnify. A workspace with three nodes fits at twice the size, and a node
       * drawn at twice the size loses its intended geometry — the font, the border and the
       * corner all double with it. Shrinking to fit a large graph is what fitting is for.
       */
      fitViewOptions={FIT}
      proOptions={{ hideAttribution: true }}
      nodesDraggable
      nodeDragThreshold={4}
      nodesConnectable={false}
      edgesReconnectable={false}
      edgesFocusable={false}
      deleteKeyCode={null}
      minZoom={0.05}
      maxZoom={2}
      onInit={flow => { instance.current = flow; }}
      onNodesChange={changes => {
        for (const change of changes) if (change.type === "select" && change.selected) onSelect?.(change.id);
        if (changes.some(change => change.type === "position" && change.position)) remember(movedPositions(manual, changes, live));
      }}
      onNodeClick={(_, node) => onSelect?.(node.id)}
    >
      <Background gap={16} size={1} />
      {/* The graph in miniature and where the view is; its size and colours are in screens.css. */}
      <MiniMap style={MINIMAP_SIZE} pannable zoomable maskStrokeWidth={1.5} nodeClassName={node => `graph-mini-${node.data.state ?? "ok"}${node.data.selected ? " graph-mini-selected" : ""}`} onNodeClick={(_, node) => onSelect?.(node.id)} ariaLabel="Graph minimap" />
    </ReactFlow>
  );
});
