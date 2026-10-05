/**
 * The four screens of the surface, as the data the client would have.
 *
 * Synthetic throughout, and the same synthetic workspace the session uses, so the graph's nine
 * nodes really are the nine the footer counts.
 */
import type { Segment } from "../MonoLine";
import { emptyWorkspace } from "../../workspace";
import { factLines, openFacts } from "../open-model";
import type { Environment } from "./Env";
import type { GraphEdge, GraphNode, Selected } from "./Graph";
import type { SettingRow } from "./Settings";

const acme = (endpoint: string, supplied: readonly boolean[], writes = false) => ({
  name: "acme", kind: "spec", target: "local", endpoint, writes,
  credentials: supplied.map((yes, at) => ({ name: ["apiKey", "apiSecret"][at]!, supplied: yes })),
});
const builtIn = (name: string, writes: boolean) => ({ name, kind: "builtin", target: "local", writes, credentials: [] });

export const environments: readonly Environment[] = [
  { name: "DEV", providers: [acme("https://dev.acme.test", [true, true]), builtIn("http", false)], credentials: { present: 2, wanted: 2 }, revision: "sha256:14aa0000ffffffff", execution: true },
  { name: "STAGING", providers: [acme("https://stg.acme.test", [true, false])], credentials: { present: 1, wanted: 2 }, revision: "sha256:14bb0000ffffffff", execution: true },
  { name: "PROD", providers: [acme("https://api.acme.invalid", [true, true], true), builtIn("sh", true)], credentials: { present: 2, wanted: 2 }, revision: "sha256:14cc0000ffffffff", execution: true, dangerous: true },
  { name: "Local", providers: [] },
];

export const openSubject: Segment[] = [
  { text: "$orders", role: "mono-ref" },
  { text: "   ", role: "mono-faint" },
  { text: "acme orders.list", role: "mono-provider" },
  { text: "  ·  ", role: "mono-faint" },
  { text: "09:12", role: "mono-faint" },
  { text: "  ·  ", role: "mono-faint" },
  { text: "table 128×6", role: "mono-ink" },
  { text: "  ·  ", role: "mono-faint" },
  { text: "kept", role: "mono-dim" },
];

/** The whole result, not three rows of it: eight rows and the column the preview left out. */

/*
 * The details tab, drawn by the model the app uses rather than typed out here.
 *
 * The gallery's job is to show what the surface draws; a hand-written details tab would drift away
 * from the real one the moment a fact was added, which is exactly how it came to show a `ran at`
 * for a result nobody had run.
 */
export const openDetails: readonly Segment[][] = factLines(
  openFacts({
    node: {
      id: "id7", name: "orders", command: "acme orders.list since:2026-09-01",
      dependsOn: [], state: "ready", startedAt: "2026-09-20T09:12:00.000Z",
      run: "2f1c6b90-4d2e-4a11-9c33-7b0f6d2a8e41", type: "List<Order>", bytes: 24_576,
      kept: true, retention: "automatic", repeatable: true, traced: true,
      provenance: {}, cautions: [],
      environment: {
        event: "node-environment", node: "id7", environment: "DEV", revision: "rev 14",
        target: "https://dev.acme.test", endpoint: null, origin: "default",
      },
    },
    workspace: { ...emptyWorkspace, nodes: [] },
    context: { workspace: "sales-api", environment: "DEV", connection: "connected" },
    client: "6b1f0d24-6c3a-4f0b-9a7e-2d1c9f3b5a08",
  }),
);

export const settingsSections = [
  "appearance", "editor", "results", "keys", "connections", "aliases", "data", "limits",
] as const;

export const appearanceRows: readonly SettingRow[] = [
  {
    label: "Palette",
    command: "/theme paper",
    chosen: "paper",
    options: [
      { name: "paper", what: "light · warm" },
      { name: "ink", what: "dark" },
      { name: "system", what: "follows macOS" },
    ],
  },
  {
    label: "Cell chrome",
    command: "/theme cell bordered",
    chosen: "minimal",
    options: [
      { name: "minimal", what: "rail only" },
      { name: "bordered", what: "boxed" },
      { name: "controls", what: "buttons shown" },
      { name: "compact", what: "verdict only" },
    ],
  },
  {
    label: "Face and density",
    command: '/theme font "PT Mono" 13',
    chosen: "PT Mono 13",
    options: [
      { name: "PT Mono 13", what: "1.62 · normal" },
      { name: "PT Mono 13 dense", what: "1.4 · dense" },
      { name: "JetBrains Mono 13", what: "1.62 · normal" },
    ],
  },
];

export const settingsPreview: Segment[] = [
  { text: "PT Mono 13 / 1.62", role: "mono-dim" },
  { text: "  —  ", role: "mono-faint" },
  { text: "acme orders.list", role: "mono-provider" },
  { text: " " },
  { text: "since:", role: "mono-param" },
  { text: "2026-09-01", role: "mono-literal" },
  { text: " " },
  { text: ">", role: "mono-dim" },
  { text: " " },
  { text: "orders", role: "mono-ref" },
];

export const graphNodes: readonly GraphNode[] = [
  { id: "acme", name: "$acme", detail: "service · 41", state: "ok" },
  { id: "orders", name: "$orders", detail: "table 248×6", state: "ok" },
  { id: "totals", name: "$totals", detail: "stale · 09:31", state: "selected" },
  { id: "daily", name: "$daily", detail: "series 30", state: "ok" },
  { id: "invoices", name: "$invoices", detail: "running…", state: "running" },
  { id: "overdue", name: "$overdue", detail: "failed 404", state: "failed" },
  { id: "spread", name: "$spread", detail: "stale", state: "stale" },
  { id: "regions", name: "$regions", detail: "table 12×3", state: "ok" },
  { id: "ledger", name: "$ledger", detail: "table 9 120×4", state: "ok" },
];

export const graphEdges: readonly GraphEdge[] = [
  { from: "acme", to: "orders" }, { from: "acme", to: "regions" }, { from: "orders", to: "totals" },
  { from: "orders", to: "daily" }, { from: "orders", to: "invoices" }, { from: "totals", to: "spread" },
  { from: "daily", to: "spread" }, { from: "invoices", to: "overdue" }, { from: "regions", to: "totals" },
  { from: "ledger", to: "totals" }, { from: "acme", to: "ledger" },
];

export const graphCycles: readonly (readonly string[])[] = [
  ["$totals", "$spread"],
  ["$ledger", "$totals"],
  ["$regions", "$totals"],
];

export const graphSelected: Selected = {
  name: "$totals",
  state: "stale",
  because: "$orders ran again",
  madeBy: "cell 09:16",
  shape: "record · 4 fields",
  breaks: 1,
};
