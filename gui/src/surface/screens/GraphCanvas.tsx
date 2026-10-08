/**
 * The graph's canvas: xyflow and dagre dressed in the surface's roles.
 *
 * Laid out rather than arranged. Nobody drew this graph — it followed from what was typed — so
 * nobody should have to place it either. A node says what it is with the same colour the scrollback
 * would use for it, which is the whole reason `graph-node-*` exists as roles rather than as a
 * palette of its own.
 */
import { useMemo } from "react";
import { Background, MarkerType, MiniMap, ReactFlow, type Edge, type Node as FlowNode } from "@xyflow/react";
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
 * because a node wider than that is a label and no longer a node. The layout is given the width it
 * will be drawn at, so the boxes it places are the boxes on screen.
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
}

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
  for (const node of nodes) graph.setNode(node.id, { width: widths.get(node.id)!, height: NODE_HEIGHT });
  for (const edge of edges) if (edge.from !== edge.to) graph.setEdge(edge.from, edge.to);
  dagre.layout(graph);
  const placed = new Map<string, { x: number; y: number; width: number }>();
  for (const node of nodes) {
    const at = graph.node(node.id);
    const width = widths.get(node.id)!;
    placed.set(node.id, { x: at.x - width / 2, y: at.y - NODE_HEIGHT / 2, width });
  }
  return placed;
}

export function GraphCanvas({ nodes, edges, direction, onSelect }: GraphCanvasProps) {
  const drawn = useMemo(() => {
    const placed = laidOut(nodes, edges, direction);
    const flowNodes: FlowNode[] = nodes.map((node) => {
      const at = placed.get(node.id) ?? { x: 0, y: 0, width: NODE_WIDTH };
      return {
      id: node.id,
      position: { x: at.x, y: at.y },
      data: {
        label: (
          <span className="graph-node-said">
            <span className="graph-node-name">{node.name}</span>
            {node.detail && <span className="graph-node-detail">{node.detail}</span>}
          </span>
        ),
      },
      className: `graph-node graph-node-${node.state}`,
      // Given rather than left to the browser: the minimap draws from these, and a measurement
      // that has not happened yet leaves it empty.
      width: at.width,
      height: NODE_HEIGHT,
      style: { width: at.width, height: NODE_HEIGHT },
      };
    });
    const flowEdges: Edge[] = edges.map((edge) => ({
      id: `${edge.from}>${edge.to}`,
      source: edge.from,
      target: edge.to,
      className: edge.lifetime === "creation" ? `graph-edge graph-edge-creation${edge.constructed ? " graph-edge-constructed" : ""}`
        : edge.captured ? "graph-edge graph-edge-captured" : "graph-edge",
      ...(edge.lifetime === "creation" ? { label: edge.constructed ? "created from" : "creates", ariaLabel: edge.constructed ? "Created from this input; refreshes do not re-create it" : "Construction waits for this input" }
        : edge.captured ? { label: "captured", ariaLabel: "Input captured when the run started; later updates do not change this run" } : {}),
      markerEnd: { type: MarkerType.ArrowClosed, width: 14, height: 14 },
    }));
    return { flowNodes, flowEdges };
  }, [nodes, edges, direction]);

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
      fitViewOptions={{ maxZoom: 1, padding: 0.2 }}
      proOptions={{ hideAttribution: true }}
      nodesDraggable={false}
      onNodeClick={(_, node) => onSelect?.(node.id)}
    >
      <Background gap={16} size={1} />
      {/* The graph in miniature and where the view is; its size and colours are in screens.css. */}
      <MiniMap pannable zoomable ariaLabel="Graph minimap" />
    </ReactFlow>
  );
}
