/**
 * The surface's session screen, as a workspace the client could actually have.
 *
 * Synthetic throughout — no live value and no real provider — and deliberately built as a
 * `Workspace` and a list of `Cell`s rather than as five ready-made surface cells, because the thing
 * worth testing is the projection: that these nodes and these attempts really do read as the
 * five synthetic cells, in that order, with those verdicts.
 */
import { emptyWorkspace, type Workspace, type WorkspaceNode } from "../workspace";
import type { Cell as ClientCell } from "../cells";
import type { SessionContext } from "./session-model";

/** Fixed fixture date keeps scrollback timestamps deterministic. */
const DAY = "2026-09-20T";
const at = (time: string) => `${DAY}${time}`;

function node(over: Partial<WorkspaceNode> & { id: string; command: string }): WorkspaceNode {
  return {
    dependsOn: [],
    state: "ready",
    provenance: {},
    cautions: [],
    kept: false,
    repeatable: true,
    ...over,
  };
}

/** The five gallery cells: a pinned table, bars, a live stream, a failure, and a graph. */
export const sessionNodes: WorkspaceNode[] = [
  node({
    id: "orders", name: "orders", command: 'acme orders.list since:2026-09-01 status:"open"',
    startedAt: at("09:12:00.000"), type: "table 128×6", kept: true, repeatable: true,
  }),
  node({
    id: "usage", name: "usage", command: "acme usage.by_region since:2026-09-01",
    startedAt: at("09:13:00.000"), type: "bars 4", kept: true, dependsOn: [],
  }),
  node({
    id: "events", name: "events", command: "acme events.tail",
    // Eighteen seconds before sessionNow, so the live verdict has a deterministic duration.
    startedAt: at("09:14:42.340"), state: "running",
  }),
  node({
    id: "lookup", command: "acme customer.get id:99999",
    startedAt: at("09:14:00.000"), state: "failed", failure: "not found",
  }),
  node({
    id: "deps", name: "deps", command: "acme deps.graph",
    startedAt: at("09:15:00.000"), type: "graph 42/87", kept: true,
  }),
  // Four more nodes the footer counts and the scrollback does not show: nine in the workspace.
  node({ id: "daily", name: "daily", command: "acme orders.daily", dependsOn: ["orders"], state: "stale", staleReason: { code: "dependency_refreshed", message: "An upstream result was requested again; this result is no longer current." } }),
  node({ id: "totals", name: "totals", command: ":calc", dependsOn: ["orders"], state: "stale", staleReason: { code: "dependency_refreshed", message: "An upstream result was requested again; this result is no longer current." } }),
  node({ id: "regions", name: "regions", command: "acme regions.list" }),
  node({ id: "ledger", name: "ledger", command: "acme ledger.list" }),
];

function cell(id: string, text: string, nodes: string[], over: Partial<ClientCell> = {}): ClientCell {
  return {
    id, text, lastRun: id, state: "answered", nodes, diagnostics: [],
    pinned: false, view: "preview", ...over,
  };
}

export const sessionCells: ClientCell[] = [
  cell("c1", 'acme orders.list since:2026-09-01 status:"open"', ["orders"], { pinned: true }),
  cell("c2", "acme usage.by_region since:2026-09-01", ["usage"]),
  cell("c3", "acme events.tail", ["events"], { state: "running" }),
  cell("c4", "acme customer.get id:99999", ["lookup"]),
  cell("c5", "acme deps.graph", ["deps"]),
];

export const sessionWorkspace: Workspace = {
  ...emptyWorkspace,
  nodes: sessionNodes,
  cells: Object.fromEntries(sessionCells.map((it) => [it.lastRun, it.nodes])),
  wentStale: ["daily", "totals"],
  repeatedRuns: { daily: "orders" },
};

export const sessionContext: SessionContext = {
  workspace: "sales-api",
  environment: "DEV",
  defaultEnvironment: "DEV",
  connection: "connected",
  revision: 14,
  grantMinutes: 12,
  target: "acme-eu",
};

/** Fixed fixture clock keeps durations stable across visual regression runs. */
export const sessionNow = new Date(at("09:15:00.340"));
