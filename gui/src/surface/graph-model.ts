import { staleMessage } from "../workspace";
/**
 * The workspace as a dependency graph, for `/graph`.
 *
 * The scrollback reads in the order the work was done. This is the other order, and every fact in
 * it is the engine's: an edge exists because a node said it depends on another, a node is stale
 * because the engine said so, and a cycle is one the engine allowed. Nothing here invents a
 * relation the workspace did not carry.
 *
 * Kept pure and apart from the screen for the same reason `session-model.ts` is: the deciding is
 * the part that can be wrong, and the drawing is not.
 */
import { describeSize, describeType, type StoredValue } from "../protocol";
import { cellOf, constructed, inputsCaptured, type Workspace, type WorkspaceNode } from "../workspace";
import type { GraphEdge, GraphNode, GraphNodeState, Selected } from "./screens/Graph";
import { lifetimeActive } from "./record-progress";

export interface GraphOptions {
  /** The node whose panel is open, if one is. */
  readonly selected?: string;
  /** Only the stale nodes and what they reach. `/stale` opens the screen with this on. */
  readonly staleOnly?: boolean;
  /** Only the nodes that take part in a dependency: a node no edge touches is left out. */
  readonly connectedOnly?: boolean;
  /** Results fetched back from the engine, by handle, so `shape` can say what a value holds. */
  readonly held?: ReadonlyMap<string, StoredValue>;
}

export interface GraphView {
  readonly nodes: readonly GraphNode[];
  readonly edges: readonly GraphEdge[];
  readonly cycles: readonly (readonly string[])[];
  readonly selected?: Selected;
  /** Nodes the connected-only filter left out, so the screen can say so. */
  readonly hidden?: number;
}

/** `$orders`, or the id the engine gave it when nobody named it. */
export function nameOf(node: WorkspaceNode): string {
  return node.name === undefined ? node.id : `$${node.name}`;
}

/**
 * What a node says under its name: what it holds, or what is happening to it.
 *
 * A running node has no shape yet and a failed one has a reason instead, so the line says the more
 * particular thing. It is one line on a 150px node, so a failure is cut to its first clause.
 */
export function detailOf(node: WorkspaceNode): string | undefined {
  switch (node.state) {
    case "running": return node.updatePending ? "running… · newer input waiting" : "running…";
    case "pending": return "pending";
    case "cancelled": return "cancelled";
    case "skipped": return "skipped";
    case "failed": {
      // A committed partial result is beside the failure, never instead of it.
      const said = node.failure === undefined ? "failed" : firstClause(node.failure);
      return node.evidence?.kind === "incomplete" ? `incomplete · ${said}` : said;
    }
    case "stale": return node.type === undefined ? "stale" : `stale · ${node.type}`;
    case "ready": {
      const said: string[] = [];
      if (node.type !== undefined) said.push(node.type);
      if (node.bytes !== undefined) said.push(describeSize(node.bytes));
      return said.length === 0 ? undefined : said.join(" · ");
    }
  }
}

/** As much of a message as fits beside a name: the first sentence, and at most sixty characters. */
function firstClause(message: string): string {
  const first = message.split(/[.\n]/, 1)[0] ?? message;
  return first.length <= 60 ? first : `${first.slice(0, 59)}…`;
}

/** `selected` outranks everything, because the panel is about that node whatever else it is. */
export function stateOf(node: WorkspaceNode, selected: string | undefined): GraphNodeState {
  if (node.id === selected) return "selected";
  switch (node.state) {
    case "running": return "running";
    case "stale": return "stale";
    case "failed":
    case "cancelled": return "failed";
    // A usable prefix of a run the engine still calls open (recording or followed scan) has not finished.
    default: return lifetimeActive(node) ? "running" : "ok";
  }
}

/** One edge per declared dependency, and only between nodes the workspace still has. */
export function edgesOf(nodes: readonly WorkspaceNode[]): GraphEdge[] {
  const present = new Set(nodes.map((node) => node.id));
  return nodes.flatMap((node) =>
    node.dependsOn.filter((from) => present.has(from)).map((from) => node.dependencyLifetime === "creation"
      ? { from, to: node.id, lifetime: "creation" as const, constructed: constructed(node) }
      : node.dependencyLifetime === "captured"
        ? { from, to: node.id, lifetime: "captured" as const, captured: inputsCaptured(node) }
        : { from, to: node.id }),
  );
}

/** Whether a producer's update still makes this edge's consumer out of date, per the engine's lifetime. */
const carriesCurrency = (edge: GraphEdge) => !edge.constructed && !edge.captured;

/**
 * Everything an update reaches from these, following the edges the given way round. The engine
 * stops currency at a completed construction and at an analysis that has captured its inputs; the
 * structure itself (cycles, ownership, deletion) is read from all edges elsewhere.
 */
function reachable(edges: readonly GraphEdge[], from: Iterable<string>, downstream: boolean): Set<string> {
  const next = new Map<string, string[]>();
  for (const edge of edges) {
    if (!carriesCurrency(edge)) continue;
    const [key, value] = downstream ? [edge.from, edge.to] : [edge.to, edge.from];
    next.set(key, [...(next.get(key) ?? []), value]);
  }
  const found = new Set<string>();
  const pending = [...from];
  while (pending.length > 0) {
    const at = pending.pop()!;
    for (const to of next.get(at) ?? []) if (!found.has(to)) { found.add(to); pending.push(to); }
  }
  return found;
}

/**
 * What would be invalidated by running this again: everything that depends on it, however far,
 * short of completed constructions and analyses that already captured their inputs. Running such an
 * analysis again does reach its own downstream.
 */
export function dependentsOf(edges: readonly GraphEdge[], id: string): Set<string> {
  return reachable(edges, [id], true);
}

/**
 * The cycles in the graph, each as the names in it.
 *
 * Tarjan's, because enumerating every cycle is exponential and what a person needs to see is the
 * knot rather than every way round it: one strongly connected component is one knot. The recursion
 * is bounded by the workspace, which is tens of nodes.
 */
export function cyclesIn(nodes: readonly WorkspaceNode[], edges: readonly GraphEdge[]): string[][] {
  const next = new Map<string, string[]>();
  for (const edge of edges) next.set(edge.from, [...(next.get(edge.from) ?? []), edge.to]);
  const index = new Map<string, number>();
  const low = new Map<string, number>();
  const stack: string[] = [];
  const onStack = new Set<string>();
  const components: string[][] = [];
  let counter = 0;

  const visit = (at: string): void => {
    index.set(at, counter);
    low.set(at, counter);
    counter += 1;
    stack.push(at);
    onStack.add(at);
    for (const to of next.get(at) ?? []) {
      if (!index.has(to)) {
        visit(to);
        low.set(at, Math.min(low.get(at)!, low.get(to)!));
      } else if (onStack.has(to)) {
        low.set(at, Math.min(low.get(at)!, index.get(to)!));
      }
    }
    if (low.get(at) !== index.get(at)) return;
    const component: string[] = [];
    for (;;) {
      const member = stack.pop()!;
      onStack.delete(member);
      component.push(member);
      if (member === at) break;
    }
    // A component of one is a cycle only if it depends on itself.
    if (component.length > 1 || (next.get(at) ?? []).includes(at)) components.push(component);
  };

  for (const node of nodes) if (!index.has(node.id)) visit(node.id);

  const named = (id: string) => nodes.find((node) => node.id === id);
  return components.map((component) =>
    walk(component, next).flatMap((id) => { const node = named(id); return node ? [nameOf(node)] : []; }),
  );
}

/** A knot said in the order its edges run, so `a → b → a` reads the way the work would. */
function walk(component: readonly string[], next: ReadonlyMap<string, string[]>): string[] {
  const inside = new Set(component);
  const order: string[] = [];
  let at = component[component.length - 1]!;
  while (!order.includes(at)) {
    order.push(at);
    const on = (next.get(at) ?? []).find((to) => inside.has(to) && !order.includes(to));
    if (on === undefined) break;
    at = on;
  }
  for (const member of component) if (!order.includes(member)) order.push(member);
  return order;
}

/** The cause captured by the engine, independent of unrelated retry history. */
export function becauseOf(
  _workspace: Workspace,
  node: WorkspaceNode,
  _edges: readonly GraphEdge[],
): string | undefined {
  return staleMessage(node);
}

/** `cell 09:16` — the cell it came from, said as the time it ran, which is how a person finds it. */
export function madeByOf(workspace: Workspace, node: WorkspaceNode): string | undefined {
  if (cellOf(workspace, node.id) === undefined) return undefined;
  if (node.startedAt === undefined) return "a cell in this session";
  const at = new Date(node.startedAt);
  return Number.isNaN(at.getTime())
    ? "a cell in this session"
    : `cell ${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
}

/**
 * What the node holds: the type, then how much of it.
 *
 * The fetched value says the count the type cannot — a `List<Order>` is a type and `248 rows` is
 * the fact. When the value has not been fetched back, the node's own type and size still answer.
 */
export function shapeOf(node: WorkspaceNode, held: ReadonlyMap<string, StoredValue>): string | undefined {
  const stored = node.handle === undefined ? undefined : held.get(node.handle);
  const said: string[] = [];
  if (stored !== undefined) {
    said.push(describeType(stored.type));
    if (Array.isArray(stored.data)) said.push(`${stored.data.length} row${stored.data.length === 1 ? "" : "s"}`);
    // A record the engine described; a value whose type did not arrive has no field count to give.
    else if (stored.type?.kind === "record" && Array.isArray(stored.type.fields)) {
      said.push(`${stored.type.fields.length} field${stored.type.fields.length === 1 ? "" : "s"}`);
    }
  } else if (node.type !== undefined) {
    said.push(node.type);
  }
  if (node.bytes !== undefined) said.push(describeSize(node.bytes));
  return said.length === 0 ? undefined : said.join(" · ");
}

/** The five facts of the selection panel, each left out when the workspace cannot answer it. */
export function selectedOf(
  workspace: Workspace,
  node: WorkspaceNode,
  edges: readonly GraphEdge[],
  held: ReadonlyMap<string, StoredValue>,
): Selected {
  return {
    name: nameOf(node),
    state: node.state,
    because: becauseOf(workspace, node, edges),
    madeBy: madeByOf(workspace, node),
    shape: shapeOf(node, held),
    breaks: dependentsOf(edges, node.id).size,
    ...(node.dependencyLifetime === "creation" ? { input: creationInputOf(workspace, node) } : {}),
  };
}

/**
 * What a creation-lifetime node's input edges mean. The input ordered the one construction; after
 * it, later results reach a view through its Current input, and nothing re-creates it.
 */
export function creationInputOf(workspace: Workspace, node: WorkspaceNode): string {
  const inputs = node.dependsOn.map(id => workspace.nodes.find(other => other.id === id)).map(other => other ? nameOf(other) : undefined).filter(Boolean).join(", ") || "its input";
  return constructed(node)
    ? `created once from ${inputs}; refreshing it does not create this again`
    : `waits for ${inputs} before it is created`;
}

/**
 * The graph as the screen takes it.
 *
 * `stale only` keeps the stale nodes and everything downstream of them, because that is the
 * question it answers: what is out of date, and what else is out of date because of it.
 */
export function readGraph(workspace: Workspace, options: GraphOptions = {}): GraphView {
  const held = options.held ?? new Map<string, StoredValue>();
  const all = workspace.nodes;
  const allEdges = edgesOf(all);
  const cycles = cyclesIn(all, allEdges);

  let kept = all;
  if (options.staleOnly) {
    const stale = all.filter((node) => node.state === "stale").map((node) => node.id);
    const reached = reachable(allEdges, stale, true);
    const inside = new Set([...stale, ...reached, ...(options.selected === undefined ? [] : [options.selected])]);
    kept = all.filter((node) => inside.has(node.id));
  }
  let hidden = 0;
  if (options.connectedOnly) {
    // The selected node stays whatever the filter says: a selection that vanished would be a puzzle.
    const linked = new Set(allEdges.flatMap((edge) => [edge.from, edge.to]));
    const before = kept.length;
    kept = kept.filter((node) => linked.has(node.id) || node.id === options.selected);
    hidden = before - kept.length;
  }
  const present = new Set(kept.map((node) => node.id));

  const selectedNode = all.find((node) => node.id === options.selected);
  return {
    nodes: kept.map((node) => {
      const detail = detailOf(node);
      return {
        id: node.id,
        name: nameOf(node),
        ...(detail === undefined ? {} : { detail }),
        state: stateOf(node, options.selected),
      };
    }),
    edges: allEdges.filter((edge) => present.has(edge.from) && present.has(edge.to)),
    cycles,
    ...(selectedNode === undefined ? {} : { selected: selectedOf(workspace, selectedNode, allEdges, held) }),
    ...(hidden > 0 ? { hidden } : {}),
  };
}
