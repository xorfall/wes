import { planned, retireCells, type Cell } from "../cells";
import type { Event } from "../protocol";
import type { WorkspaceNode } from "../workspace";

/** A weakly connected component: follow both inputs and consumers at every step. */
export function connectedNodes(nodes: readonly Pick<WorkspaceNode, "id" | "dependsOn">[], root: string): ReadonlySet<string> {
  const neighbors = new Map(nodes.map(node => [node.id, new Set<string>()]));
  if (!neighbors.has(root)) return new Set();
  for (const node of nodes) for (const input of node.dependsOn) if (neighbors.has(input)) {
    neighbors.get(node.id)!.add(input);
    neighbors.get(input)!.add(node.id);
  }
  const found = new Set([root]), pending = [root];
  for (let index = 0; index < pending.length; index++) for (const neighbor of neighbors.get(pending[index]!)!) {
    if (!found.has(neighbor)) { found.add(neighbor); pending.push(neighbor); }
  }
  return found;
}

/** Preserve original cell order and owning command groups; never manufacture a second execution. */
export function connectedCells(cells: readonly Cell[], nodes: readonly Pick<WorkspaceNode, "id" | "dependsOn">[], root: string): readonly Cell[] {
  const connected = connectedNodes(nodes, root);
  return cells.filter(cell => cell.nodes.some(node => connected.has(node)));
}

/** The shared ledger listens to every pane's acknowledged work; it never owns a submission. */
export function sharedCellEvent(cells: readonly Cell[], event: Event): readonly Cell[] {
  if (event.event === "session") return [];
  if (event.event === "work-retired") return retireCells(cells, event.cells);
  if (event.event === "planned") return planned(cells, event);
  if (event.event === "reported" && event.cell) return cells.map(cell => cell.lastRun === event.cell ? { ...cell, diagnostics: event.diagnostics } : cell);
  if (event.event === "created" && event.command) return cells.map(cell => !cell.text && cell.nodes.includes(event.node) ? { ...cell, text: event.command } : cell);
  return cells;
}
