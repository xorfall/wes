/**
 * Where an opened result lives when it is not a screen over the workspace.
 *
 * Opening a result in a separate window or browser tab requires an address
 * to be opened at, and this is it: the same client, told to draw one result rather than the
 * session. A hash route rather than a path, because the engine serves one document and the client
 * already reaches its galleries this way — asking the server to route would make the address a
 * second thing that has to agree with this one.
 */
import { OPEN_TABS, type OpenTab } from "./screens/Open";
import { workspaceName } from "../workspace-binding";
import { viewNamed } from "./result-views";
import { PEEK_WHATS, type PeekWhat } from "./peek";

interface NodeOpenRoute {
  readonly node: string;
  readonly cell?: never;
  readonly tab: OpenTab;
  readonly workspace?: string;
  /** One piece of the result in a plain window (`#peek/…`), instead of the tabbed result. */
  readonly peek?: PeekWhat;
}

export type OpenRoute = NodeOpenRoute | {
  readonly cell: string;
  readonly node?: never;
  readonly tab: "result";
  readonly peek: "source";
  readonly workspace?: string;
};

const query = (workspace?: string) => (workspace === undefined ? "" : `?${new URLSearchParams({ workspace })}`);

export function openRoute(node: string, tab: OpenTab = "result", workspace?: string): string {
  return `#open/${encodeURIComponent(node)}/${tab}${query(workspace)}`;
}

/** The address of one piece of a result — its type, its value or its source — in a plain window. */
export function peekRoute(node: string, what: PeekWhat, workspace?: string): string {
  return `#peek/${encodeURIComponent(node)}/${what}${query(workspace)}`;
}

/** Source belongs to a command cell, including commands that produce no result. */
export function sourceRoute(cell: string, workspace?: string): string {
  return `#source/${encodeURIComponent(cell)}${query(workspace)}`;
}

/** A screen summoned into a window of its own: the graph, or a settings section. */
export interface ScreenRoute {
  readonly screen: "graph" | "stale" | "settings" | "spec";
  readonly section?: string;
  readonly workspace?: string;
}

const SCREENS: readonly ScreenRoute["screen"][] = ["graph", "stale", "settings", "spec"];

/** The address of a screen in a window of its own, of the same shape as a result's. */
export function screenRoute(screen: ScreenRoute["screen"], workspace?: string, section?: string): string {
  return `#${screen}${screen === "settings" && section ? `/${encodeURIComponent(section)}` : ""}${query(workspace)}`;
}

/** The screen a hash names, or nothing when it names something else — a result, a gallery, the session. */
export function readScreenRoute(hash: string): ScreenRoute | undefined {
  const [path, query] = hash.split("?");
  const binding = new URLSearchParams(query).get("workspace");
  if (binding !== null && !workspaceName(binding)) return undefined;
  const parts = path!.replace(/^#\/?/, "").split("/");
  const screen = SCREENS.find((it) => it === parts[0]);
  if (!screen) return undefined;
  const section = screen === "settings" && parts[1] ? decodeURIComponent(parts[1]) : undefined;
  return { screen, ...(section ? { section } : {}), ...(binding === null ? {} : { workspace: binding }) };
}

/** The route a hash names, or nothing when it names something else — a gallery, or the session. */
export function readOpenRoute(hash: string): OpenRoute | undefined {
  const [path, query] = hash.split("?");
  const binding = new URLSearchParams(query).get("workspace");
  if (binding !== null && !workspaceName(binding)) return undefined;
  const parts = path!.replace(/^#\/?/, "").split("/");
  if (parts[0] === "source") {
    const cell = decodeURIComponent(parts[1] ?? "");
    return cell ? { cell, tab: "result", peek: "source", ...(binding === null ? {} : { workspace: binding }) } : undefined;
  }
  if (parts[0] !== "open" && parts[0] !== "peek") return undefined;
  const node = decodeURIComponent(parts[1] ?? "");
  if (node === "") return undefined;
  if (parts[0] === "peek") {
    /* A piece nobody can peek at falls back to the whole result, which is always there. */
    const what = PEEK_WHATS.find((it) => it === parts[2]);
    return { node, tab: "result", ...(what ? { peek: what } : {}), ...(binding === null ? {} : { workspace: binding }) };
  }
  /* A view is addressable by name the way `json` is; anything else is the result. */
  const named = OPEN_TABS.find(it => it === parts[2]) ?? viewNamed(parts[2])?.name;
  return { node, tab: named ?? "result", ...(binding === null ? {} : { workspace: binding }) };
}
