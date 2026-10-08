/**
 * `/graph` — what depends on what, when that is the question.
 *
 * The scrollback reads in the order the work was done, which is the order a person did it in. This
 * is the other order, and it is worth a screen of its own rather than a default: a canvas laid out
 * by dependency answers "why is this stale and what breaks", and answers nothing about what just
 * happened.
 *
 * The selection panel is the screen's point. A node on a canvas is a dot; the panel is where the
 * dot says it is stale because `$orders` ran again, that one dependent breaks if it runs again, and
 * which cell made it.
 */
import { MonoLine, type Segment } from "../MonoLine";
import { leaving, Screen } from "../Screen";
import { padded } from "../forms/form";

export type GraphNodeState = "ok" | "running" | "stale" | "failed" | "selected";

export interface GraphNode {
  readonly id: string;
  /** `$orders` — what a command would call it. */
  readonly name: string;
  /** What it holds, in one short line: `table 248×6`, `stale · 09:31`, `running…`. */
  readonly detail?: string;
  readonly state: GraphNodeState;
}
export interface GraphEdge {
  readonly from: string;
  readonly to: string;
  /**
   * `creation`: the edge ordered the consumer's one construction. Once it is `constructed`, refreshes
   * no longer travel along it; the edge stays drawn because ownership and deletion still follow it.
   */
  readonly lifetime?: "creation" | "captured";
  readonly constructed?: boolean;
  /**
   * `captured`: the consumer took this input when its run was admitted. Producer updates no longer
   * make it out of date (an explicit refresh of it captures them); the edge stays drawn and still
   * counts for ownership, deletion and cycles.
   */
  readonly captured?: boolean;
}

/** Why the selected node is what it is, and what it would take with it. */
export interface Selected {
  readonly name: string;
  readonly state: string;
  /** What made it that: `$orders ran again`. */
  readonly because?: string;
  /** The cell it came from: `cell 09:16`. */
  readonly madeBy?: string;
  readonly shape?: string;
  /** How much would be invalidated by running it again. */
  readonly breaks?: number;
  /** For a creation-lifetime node: what its input edges mean, in one line. */
  readonly input?: string;
}

export interface GraphProps {
  readonly top: readonly Segment[];
  readonly nodes: readonly GraphNode[];
  readonly edges: readonly GraphEdge[];
  /** Each cycle as the names in it, in order. */
  readonly cycles: readonly (readonly string[])[];
  readonly selected?: Selected;
  /** Only the stale nodes and what they reach. `/stale` opens the screen with this on. */
  readonly staleOnly?: boolean;
  /** Only the nodes an edge touches; the others are counted in `hidden`. */
  readonly connectedOnly?: boolean;
  /** Nodes the filters left out. */
  readonly hidden?: number;
  readonly direction?: "LR" | "TB";
  readonly onStaleOnly?: (on: boolean) => void;
  readonly onConnectedOnly?: (on: boolean) => void;
  readonly onDirection?: (direction: "LR" | "TB") => void;
  readonly onFit?: () => void;
  readonly onFind?: () => void;
  /** Take the session to the cell that made the selected node. */
  readonly onJump?: () => void;
  /** Run the selected node's cell again, dependents and all. */
  readonly onRepeat?: () => void;
  /** Open the selected node's result in `/open`. */
  readonly onOpenResult?: () => void;
  readonly onClose?: () => void;
  /** The canvas itself. Drawn by the client; a test tree has no canvas and needs none. */
  readonly canvas?: React.ReactNode;
  /** `pane` when the screen is inside a split rather than over the workspace. */
  readonly chrome?: "full" | "pane";
}

/** `$totals   9 nodes  ·  11 edges  ·  3 cycles` */
export function graphSubject(
  selected: Selected | undefined,
  nodes: readonly GraphNode[],
  edges: readonly GraphEdge[],
  cycles: readonly (readonly string[])[],
): Segment[] {
  const dot: Segment = { text: "  ·  ", role: "mono-faint" };
  const line: Segment[] = [];
  if (selected) line.push({ text: selected.name, role: "mono-ref", variableName:selected.name }, { text: "   ", role: "mono-faint" });
  line.push({ text: `${nodes.length} nodes`, role: "mono-ink" }, dot, { text: `${edges.length} edges`, role: "mono-ink" });
  if (cycles.length > 0) line.push(dot, { text: `${cycles.length} cycles`, role: "mono-warn" });
  return line;
}

/** The column each fact's name is padded to, so the values start together. */
const FACT_WIDTH = 13;

export function selectionFacts(selected: Selected): Segment[][] {
  const facts: [string, string][] = [];
  facts.push(["state", selected.state]);
  if (selected.because) facts.push(["because", selected.because]);
  if (selected.madeBy) facts.push(["made by", selected.madeBy]);
  if (selected.shape) facts.push(["shape", selected.shape]);
  if (selected.input) facts.push(["input", selected.input]);
  if (selected.breaks !== undefined) {
    facts.push(["breaks", `${selected.breaks} dependent${selected.breaks === 1 ? "" : "s"}`]);
  }
  return facts.map(([name, value]) => [
    { text: padded(name, FACT_WIDTH), role: "mono-param" as const },
    { text: value, role: "mono-literal" as const },
  ]);
}

export function cycleLine(cycle: readonly string[]): Segment[] {
  return [{ text: [...cycle, cycle[0]].join(" → "), role: "mono-warn" }];
}

/**
 * One offer from the selection panel, drawn as the key that makes it.
 *
 * Actionable key hints are buttons as well: clicking a hint performs the same action
 * as its keyboard shortcut.
 */
function PanelKey({ mark, what, onDo }: { readonly mark: string; readonly what: string; readonly onDo?: () => void }) {
  const segments: Segment[] = [
    { text: mark, role: "mono-ref" },
    { text: ` ${what}`, role: "mono-dim" },
  ];
  if (!onDo) return <MonoLine className="graph-panel-keys" segments={segments} />;
  return (
    <button type="button" className="graph-panel-key cell-action" onClick={onDo}>
      <MonoLine className="graph-panel-keys" segments={segments} />
    </button>
  );
}

export function GraphScreen({
  top, nodes, edges, cycles, selected, staleOnly = false, connectedOnly = false, hidden = 0, direction = "LR",
  onStaleOnly, onConnectedOnly, onDirection, onFit, onFind, onJump, onRepeat, onOpenResult, onClose, canvas, chrome = "full",
}: GraphProps) {
  return (
    <Screen
      name="/graph"
      top={top}
      chrome={chrome}
      subject={graphSubject(selected, nodes, edges, cycles)}
      onClose={onClose}
      footer={leaving(
        { text: "⇥", role: "mono-ref" }, { text: " next node", role: "mono-dim" },
        { text: "   ", role: "mono-faint" },
        { text: "/", role: "mono-ref" }, { text: " another screen", role: "mono-dim" },
      )}
      tools={
        <>
          <button
            type="button"
            className={`screen-chip ${staleOnly ? "chip-chosen" : "cell-action"}`}
            aria-pressed={staleOnly}
            onClick={() => onStaleOnly?.(!staleOnly)}
          >
            stale only
          </button>
          <button
            type="button"
            className={`screen-chip ${connectedOnly ? "chip-chosen" : "cell-action"}`}
            aria-pressed={connectedOnly}
            aria-description="leave out the nodes no dependency touches"
            onClick={() => onConnectedOnly?.(!connectedOnly)}
          >
            hide unconnected
          </button>
          <button type="button" className="screen-chip cell-action" onClick={() => onDirection?.(direction === "LR" ? "TB" : "LR")}>
            {`layout ↔ ${direction}`}
          </button>
          <button type="button" className="screen-chip cell-action" onClick={() => onFit?.()}>fit</button>
          <button type="button" className="screen-chip cell-action" onClick={() => onFind?.()}>⌕ find node</button>
        </>
      }
      keys={[
        { text: "esc", role: "mono-ref" }, { text: " close", role: "mono-dim" },
        { text: "   ", role: "mono-faint" },
        { text: "⏎", role: "mono-ref" }, { text: " jump to cell", role: "mono-dim" },
      ]}
    >
      <div className="graph-layout">
        {/* Nothing has been run yet, which is a fact about the session and not an empty canvas. */}
        {nodes.length === 0 ? (
          <MonoLine segments={[{ text: "no nodes yet", role: "mono-faint" }]} className="graph-empty" />
        ) : (
          <div className="graph-canvas" role="img" aria-label={`${nodes.length} nodes, ${edges.length} edges`}>
            {canvas}
          </div>
        )}
        <aside className="graph-panel surface-sunk">
          {(staleOnly || connectedOnly) && (
            <>
              <span className="screen-label">Filter</span>
              <MonoLine className="graph-fact" segments={[
                { text: [staleOnly ? "stale only" : "", connectedOnly ? "connected only" : ""].filter(Boolean).join(" · "), role: "mono-dim" },
                ...(hidden > 0 ? [{ text: ` · ${hidden} hidden`, role: "mono-faint" as const }] : []),
              ]} />
            </>
          )}
          {selected && (
            <>
              <span className="screen-label">Selected</span>
              {selectionFacts(selected).map((line, at) => (
                <MonoLine key={at} segments={line} className="graph-fact graph-selection-fact" />
              ))}
              <div className="graph-panel-offers">
                <PanelKey mark="⏎" what="jump to cell" onDo={onJump} />
                <PanelKey mark="↻" what="repeat + 1" onDo={onRepeat} />
                <PanelKey mark="⊙" what="open result" onDo={onOpenResult} />
              </div>
            </>
          )}
          {cycles.length > 0 && (
            <>
              <span className="screen-label graph-cycles-title">Cycles</span>
              {cycles.map((cycle) => (
                <MonoLine key={cycle.join(">")} segments={cycleLine(cycle)} className="graph-cycle" />
              ))}
            </>
          )}
        </aside>
      </div>
    </Screen>
  );
}
