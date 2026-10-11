import { stringifyExactJson } from "../exact-json";
import { readableData } from "../presentation/readable";
/**
 * What `/open` shows about one result, read from what the engine actually said.
 *
 * Cells show command-specific facts; this details model adds command, run and session metadata.
 * Every displayed fact comes from an engine event. Missing facts are omitted rather than filled
 * with defaults, so an unknown retention policy cannot appear to be an explicit choice.
 *
 * The three groups describe what was asked, what happened when it ran, and what is true of this
 * session rather than of this result. Each fact is one `key value` row in the record form's two
 * columns, so the details tab reads like every other pair of columns on the surface.
 */
import { describeSize, type StoredValue } from "../protocol";
import type { Cell as ClientCell } from "../cells";
import type { Workspace, WorkspaceNode } from "../workspace";
import type { Catalogue } from "../vocabulary";
import { padded } from "./forms/form";
import { evidenceLabel } from "./record-progress";
import type { MonoRole, Segment } from "./MonoLine";
import type { SessionContext } from "./session-model";
import type { OpenTab } from "./screens/Open";

/** The column every fact's name is padded to, as `/settings` and the graph panel pad theirs. */
const NAME_WIDTH = 16;

/** What `/open`'s result tab draws before it says how much more there is. */
export const OPEN_ROWS = 200;

export interface Fact {
  readonly name: string;
  readonly value: string;
  /** `mono-literal` unless the fact is one somebody should look twice at. */
  readonly role?: MonoRole;
}

export interface FactGroup {
  readonly name: string;
  readonly facts: readonly Fact[];
}

export interface OpenInput {
  readonly node?: WorkspaceNode;
  /** The cell that ran it, for the two facts that belong to the attempt rather than to the node. */
  readonly cell?: ClientCell;
  readonly workspace: Workspace;
  readonly context: SessionContext;
  /** This client's own id, which the engine gave it. */
  readonly client?: string;
  /** The result itself, once it has been fetched back. */
  readonly stored?: StoredValue;
}

/** Omit facts the engine has not reported; never infer missing values. */
function fact(name: string, value: string | undefined, role?: MonoRole): Fact | undefined {
  if (value === undefined || value === "") return undefined;
  return role === undefined ? { name, value } : { name, value, role };
}

function group(name: string, facts: readonly (Fact | undefined)[]): FactGroup | undefined {
  const kept = facts.filter((it): it is Fact => it !== undefined);
  return kept.length === 0 ? undefined : { name, facts: kept };
}

/** `2026-09-22 09:12:05`, in the reader's own zone, because that is when it happened for them. */
export function when(at: string | undefined): string | undefined {
  if (at === undefined || at === "") return undefined;
  const date = new Date(at);
  if (Number.isNaN(date.getTime())) return undefined;
  const two = (number: number) => String(number).padStart(2, "0");
  return `${date.getFullYear()}-${two(date.getMonth() + 1)}-${two(date.getDate())} ` +
    `${two(date.getHours())}:${two(date.getMinutes())}:${two(date.getSeconds())}`;
}

/**
 * The provider a command names, and the capability under it, when the catalogue knows them.
 *
 * Read from the command's own words against what the engine announced, never from the shape of the
 * text: `acme orders.list` is a provider call because the engine said there is an `acme` with an
 * `orders list`, and `sh run` is one for the same reason. A word the catalogue does not know is not
 * a provider, and then there is no provider fact at all.
 */
export function capabilityOf(command: string, catalogue: Catalogue) {
  const words = command.trim().split(/\s+/);
  const first = words.findIndex((word) => !word.startsWith("@"));
  const name = words[first];
  if (name === undefined) return undefined;
  const provider = catalogue.providers.find((it) => it.name === name);
  if (!provider) return undefined;
  const path = (words[first + 1] ?? "").split(/[.\s]/).filter((part) => part !== "");
  const capability = provider.capabilities.find(
    (it) => it.path.length === path.length && it.path.every((part, at) => part === path[at]),
  );
  return { provider, capability };
}

/**
 * Facts grouped by command, run and session.
 *
 * Two facts have no field on the wire yet and are therefore always absent: a transfer
 * grant with its expiry, which only ever reaches the client as a context the surface is not sent,
 * and the time remaining on a retention class, which the engine reports as a class and not as a
 * deadline. Both are engine items; leaving the rows out is the rule this file is built on.
 */
export function openFacts(input: OpenInput): readonly FactGroup[] {
  const { node, cell, workspace, context } = input;
  const named = node ? capabilityOf(node.command || cell?.text || "", workspace.catalogue) : undefined;
  const environment = node?.environment;

  const command = group("about the command", [
    fact("provider", named?.provider.name, "mono-provider"),
    fact("capability", named?.capability?.path.join(" ")),
    named?.capability?.safe === false
      ? fact("unsafe", "this capability performs an external action", "mono-warn")
      : undefined,
    node?.traced ? fact("traced", "the call was recorded") : undefined,
    node?.interactive ? fact("interactive", "it can be typed at while it runs") : undefined,
    node?.repeatable === undefined
      ? undefined
      : fact(
          "repeatable",
          node.repeatable ? "yes" : "no — running it again acts again",
          node.repeatable ? undefined : "mono-warn",
        ),
    cell?.acknowledgeEffects ? fact("effects", "acknowledged for this attempt", "mono-warn") : undefined,
  ]);

  const run = group("about the run", [
    fact("ran at", when(node?.startedAt)),
    fact("run", node?.run),
    fact("environment", environment?.environment, "mono-meta"),
    fact("revision", environment?.revision),
    fact("target", environment?.target),
    fact("endpoint", environment?.endpoint ?? undefined),
    // `default` is the environment this session was already using; anything else is a redirection.
    environment && environment.origin !== "default" ? fact("origin", environment.origin, "mono-warn") : undefined,
    fact("grant", context.grantMinutes === undefined ? undefined : `${context.grantMinutes} min left`, "mono-warn"),
    fact("observation", evidenceLabel(node), "mono-warn"),
    fact("observed run", node?.evidence?.run),
    ...(node?.cautions ?? []).map((caution, at) => fact(at === 0 ? "caution" : "", caution, "mono-warn")),
    node?.doubt
      ? fact(
          "doubt",
          `a ${node.doubt.capability} call was in flight at ${when(node.doubt.when) ?? node.doubt.when}` +
            (node.doubt.safe ? "" : " — asking again may act twice"),
          "mono-bad",
        )
      : undefined,
    node?.private ? fact("private", "memory only; gone when the engine restarts", "mono-warn") : undefined,
    node?.confidential && !node.private ? fact("confidential", `encrypted local storage; ${node.residence === "temporary" ? "temporary only" : "explicit Keep permitted"}`, "mono-warn") : undefined,
    fact("retention", node?.retention),
    fact(node?.private ? "memory charge" : "storage bytes", node?.bytes === undefined ? undefined : `${node.bytes} bytes · ${node.private ? "bounded private memory, not an archive size" : "encoded value, type and provenance"}`),
    node === undefined ? undefined : fact("kept", node.kept ? "yes" : "no"),
    fact("result", node?.type === undefined ? undefined : describeShape(node)),
  ]);

  const session = group("about the session", [
    fact("client", input.client),
    fact(
      "keeping",
      workspace.keeping.automatic ? "finite results at or below the encoded storage size, automatically" : "only what was kept by hand",
    ),
    fact("under", workspace.keeping.automatic ? describeSize(workspace.keeping.under) : undefined),
    fact("workspace", context.workspace),
  ]);

  return [command, run, session].filter((it): it is FactGroup => it !== undefined);
}

function describeShape(node: WorkspaceNode): string {
  return node.bytes === undefined ? node.type ?? "" : `${node.type} · ${describeSize(node.bytes)}`;
}

/** A heading, then one `key value` row per fact. A blank line between groups, never before the first. */
export function factLines(groups: readonly FactGroup[]): Segment[][] {
  const lines: Segment[][] = [];
  for (const one of groups) {
    // A space rather than nothing: an empty line has no height, and the gap is the grouping.
    if (lines.length > 0) lines.push([{ text: " " }]);
    lines.push([{ text: one.name, role: "mono-dim" }]);
    for (const it of one.facts) {
      lines.push([
        { text: padded(it.name, NAME_WIDTH), role: "mono-param" },
        { text: it.value, role: it.role ?? "mono-literal" },
      ]);
    }
  }
  return lines;
}

/** `$orders   acme orders.list  ·  09:12  ·  table 128×6  ·  kept` — the head of the screen. */
export function openSubject(input: OpenInput): Segment[] {
  const { node } = input;
  if (!node) return [{ text: "no result is open", role: "mono-faint" }];
  const dot: Segment = { text: "  ·  ", role: "mono-faint" };
  const line: Segment[] = [
    { text: node.name ? `$${node.name}` : node.id, role: "mono-ref" },
    { text: "   ", role: "mono-faint" },
    { text: node.command, role: "mono-provider" },
  ];
  const at = when(node.startedAt);
  if (at) line.push(dot, { text: at.slice(11, 16), role: "mono-faint" });
  if (node.type) line.push(dot, { text: describeShape(node), role: "mono-ink" });
  line.push(dot, { text: node.kept ? "kept" : "not kept", role: "mono-dim" });
  return line;
}

/**
 * Which node a result screen command names, or why it names none.
 *
 * A reference that matches nothing is somebody's typo or a result that has since been let go, and
 * both deserve the same answer every other mistyped command gets: a sentence at the prompt, the
 * session exactly as it was. Opening an empty screen — or worse, a whole window with nothing in it
 * — answers a question nobody asked.
 */
export function resultNamed(
  workspace: Workspace,
  named: string | undefined,
  command: "/open" | "/edit" | "/split" | "/goto" | "/tab" = "/open",
): { readonly node?: string; readonly trouble?: string } {
  const found = workspace.nodes.find((it) => it.id === named || (it.name !== undefined && it.name === named));
  if (found) return { node: found.id };
  if (named === undefined) {
    return workspace.nodes.length === 0
      ? { trouble: `there is no result to ${command.slice(1)} yet` }
      : { trouble: `'${command}' takes a result: ${offered(workspace)}` };
  }
  return { trouble: `'${command}' knows no result called ${named}${offered(workspace) === "" ? "" : ` · names: ${offered(workspace)}`}` };
}

/** The first few results, named ones first, so the sentence can say what there is instead. */
function offered(workspace: Workspace, most = 5): string {
  const named = workspace.nodes.filter((it) => it.name !== undefined && it.name !== "");
  const rest = workspace.nodes.filter((it) => it.name === undefined || it.name === "");
  return [...named.map((it) => `$${it.name}`), ...rest.map((it) => `$${it.id}`)]
    .slice(0, most)
    .join(" ");
}

/** `could not draw details   <reason>` — the record form's two columns, like every fact beside it. */
export function couldNotDraw(tab: OpenTab | "subject", reason: string): Segment[] {
  return [
    { text: padded(`could not draw ${tab}`, NAME_WIDTH + 10), role: "mono-param" },
    { text: reason, role: "mono-bad" },
  ];
}

/** What went wrong, as a sentence rather than an object nobody can read. */
function because(thrown: unknown): string {
  const said = thrown instanceof Error ? thrown.message : String(thrown);
  return said === "" ? "the value could not be read" : said;
}

/**
 * The whole screen, read from one node, and never able to throw.
 *
 * The screen's own boundary catches a tab that fails to *render*; this catches a tab that fails to
 * be *worked out*, which happens earlier and outside any boundary — the projection runs in the
 * caller's render, so a shape nobody anticipated would unmount the client before a tab was ever
 * drawn. Each part is worked out on its own, so one impossible field costs one tab and not the
 * other two.
 */
export interface OpenDrawing {
  readonly subject: readonly Segment[];
  readonly json: string;
  readonly details: readonly (readonly Segment[])[];
}

export function readOpen(input: OpenInput): OpenDrawing {
  const tried = <T,>(read: () => T, otherwise: (said: string) => T): T => {
    try {
      return read();
    } catch (thrown) {
      return otherwise(because(thrown));
    }
  };
  return {
    subject: tried(() => openSubject(input), (said) => couldNotDraw("subject", said)),
    json: tried(() => openJson(input.stored), (said) => `could not draw json: ${said}`),
    details: tried(() => factLines(openFacts(input)), (said) => [couldNotDraw("details", said)]),
  };
}

/** The result as the engine would have encoded it, which is what the json tab is for. */
export function openJson(stored: StoredValue | undefined): string {
  if (!stored) return "";
  try {
    // `JSON.stringify` answers `undefined` — not a string — for undefined and for a function.
    return stringifyExactJson(readableData(stored.type, stored.data), 2) ?? "this value has nothing to write as JSON";
  } catch {
    return "this value cannot be written as JSON";
  }
}
