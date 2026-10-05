/**
 * A dependency graph, said rather than drawn.
 *
 * Forty-two nodes will not fit in six lines, and a picture of them squeezed into six would be worse
 * than no picture: it would look like an answer. So the scrollback says the two things worth saying
 * from here — what the heaviest node is and whether there are cycles — and offers the canvas.
 */
import { MonoLine, type Segment } from "../MonoLine";
import type { FormValue } from "./form";
import "../surface.css";

export interface GraphModel {
  readonly nodes: number;
  readonly edges: number;
  /** The node the most work hangs off, and how much. */
  readonly heaviest?: { readonly name: string; readonly dependents: number };
  readonly cycles: number;
  /** The reference `/graph` would open on. */
  readonly shape?: string;
}

export function graphLines(model: GraphModel): Segment[][] {
  const heaviest: Segment[] = model.heaviest
    ? [
        { text: "heaviest ", role: "mono-dim" },
        { text: model.heaviest.name, role: "mono-ref" },
        { text: ` (${model.heaviest.dependents} dependent${model.heaviest.dependents === 1 ? "" : "s"})`, role: "mono-dim" },
        { text: "  ", role: "mono-faint" },
        { text: "cycles ", role: "mono-dim" },
        model.cycles === 0
          ? { text: "none", role: "mono-ok" }
          : { text: String(model.cycles), role: "mono-warn" },
      ]
    : [{ text: `${model.nodes} nodes · ${model.edges} edges`, role: "mono-dim" }];
  return [heaviest];
}

export function GraphPreview({ model }: { readonly model: GraphModel }) {
  return (
    <div className="form-lines">
      {graphLines(model).map((line, at) => (
        <MonoLine key={at} segments={line} />
      ))}
    </div>
  );
}

export function readGraph(value: FormValue): GraphModel {
  const record = (typeof value.data === "object" && value.data !== null ? value.data : {}) as Record<string, unknown>;
  const nodes = Array.isArray(record.nodes) ? record.nodes.length : 0;
  const edges = Array.isArray(record.edges) ? record.edges.length : 0;
  const cycles = Array.isArray(record.cycles) ? record.cycles.length : 0;
  return { nodes, edges, cycles };
}
