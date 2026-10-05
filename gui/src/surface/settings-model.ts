/**
 * What `/settings` shows, and what choosing a card does.
 *
 * The screen's own sentence is that every row is a command you can type, so a row may only show a
 * command that really works — `surface/commands.ts` is the other half of this file, and the two
 * change together. Appearance is the section with choices, because the surface's own settings are
 * the only ones the client decides; the other six say what is actually set or configured and say
 * `nothing to set yet` when the client knows nothing, rather than drawing an empty list.
 *
 * Pure, like the other models here: it reads settings and the workspace, and returns what to draw.
 */
import { primaryGlyph } from "../platform-keys";
import { describeSize } from "../protocol";
import { validFace, type OpenIn, type Settings, type SurfaceChrome, type SurfaceDensity, type SurfaceFace, type SurfacePalette } from "../settings";
import type { Workspace } from "../workspace";
import { padded } from "./forms/form";
import type { Segment } from "./MonoLine";
import type { Language } from "./language";
import type { Option, SettingRow } from "./screens/Settings";

export type SectionName = "appearance" | "editor" | "results" | "keys" | "connections" | "aliases" | "data" | "limits";

/** Settings sections, in their navigation order. */
export const SECTIONS: readonly SectionName[] = [
  "appearance", "editor", "results", "keys", "connections", "aliases", "data", "limits",
];

export function sectionNamed(word: string | undefined): SectionName {
  return SECTIONS.find((section) => section === word) ?? "appearance";
}

export interface SettingsInput {
  readonly settings: Settings;
  readonly workspace: Workspace;
  /** The engine's language package, once it has arrived. */
  readonly pack?: Language;
}

export interface SectionView {
  /** The rows with choices. Only `appearance` has any. */
  readonly rows: readonly SettingRow[];
  /** What is set or configured, when it is not something this screen chooses. */
  readonly facts: readonly (readonly Segment[])[];
  /** Said instead of an empty list, when the client knows nothing for this section. */
  readonly empty?: string;
}

/** The column every fact's name is padded to, as the graph panel pads its own. */
const FACT_WIDTH = 16;

/** A name longer than the column keeps two spaces before its value rather than running into it. */
export function fact(name: string, value: string): Segment[] {
  return [
    { text: padded(name, Math.max(FACT_WIDTH, name.length + 2)), role: "mono-param" },
    { text: value, role: "mono-literal" },
  ];
}

/** Which setting a row writes. The card carries the option's name; this says where it goes. */
export type RowKey = "palette" | "chrome" | "tail" | "focus" | "density" | "face" | "sansFace" | "open";

/**
 * A typed face, as `/theme font` takes it: a family name, quoted or not, and optionally a density
 * after it — `"PT Mono" dense`, `Menlo`, `"SF Mono" normal`. A name that could not name a font gives
 * no face; the density word is read either way.
 */
export function faceChosen(typed: string): { face?: SurfaceFace; density?: SurfaceDensity } {
  const match = /^\s*(?:"([^"]*)"|(.*?))(?:\s+(dense|normal))?\s*$/.exec(typed);
  const named = (match?.[1] ?? match?.[2] ?? "").replace(/\s+13$/, "").trim();
  const density = match?.[3] as SurfaceDensity | undefined;
  return { ...(validFace(named) ? { face: named } : {}), ...(density ? { density } : {}) };
}

/**
 * Whether the machine has a face, asked of the browser rather than guessed at.
 *
 * `document.fonts.check` answers for the family as the machine would resolve it, which is exactly
 * the question. Where there is no font API — a test runner — the answer is "yes", because a picker
 * that cries "not installed" in a place with no fonts at all would be noise.
 */
export function installedFace(face: SurfaceFace): boolean {
  if (typeof document === "undefined" || !document.fonts || typeof document.fonts.check !== "function") return true;
  try {
    return document.fonts.check(`13px "${face}"`);
  } catch {
    return true;
  }
}

export function appearanceRows(settings: Settings): SettingRow[] {
  return [
    {
      key: "palette",
      label: "Palette",
      command: "/theme paper",
      chosen: settings.surfacePalette,
      options: [
        { name: "paper", what: "light · warm" },
        { name: "ink", what: "dark" },
        { name: "white", what: "plain light, no warmth" },
        { name: "system", what: "follows macOS light or dark" },
      ],
    },
    {
      key: "chrome",
      label: "Cell actions",
      command: "/theme cell controls",
      chosen: settings.surfaceChrome,
      options: [
        { name: "keys", what: "key hints in cell actions" },
        { name: "controls", what: "buttons in cell actions" },
      ],
    },
    {
      key: "tail",
      label: "Action footers",
      command: "/theme keys hidden",
      chosen: settings.surfaceTailKeys,
      options: [
        { name: "shown", what: "always visible below every cell" },
        { name: "hidden", what: `hidden; ${primaryGlyph()}M opens cell actions` },
      ],
    },
    {
      key: "focus",
      label: "Focused cell",
      command: "/theme focus wash",
      chosen: settings.surfaceFocus,
      options: [
        { name: "outline", what: "violet border only" },
        { name: "wash", what: "violet border over the selection wash" },
      ],
    },
    {
      key: "density",
      label: "Density",
      command: "/theme density dense",
      chosen: settings.surfaceDensity,
      options: [
        { name: "normal", what: "line height 1.62, for any face" },
        { name: "dense", what: "line height 1.4, for any face" },
      ],
    },
    {
      key: "face",
      kind: "font",
      family: "mono",
      label: "Data font",
      note: "commands, results, tables · monospaced only",
      command: `/theme font "${settings.surfaceFace}"`,
      chosen: settings.surfaceFace,
      options: [],
    },
    {
      key: "sansFace",
      kind: "font",
      family: "sans",
      label: "Interface font",
      note: "screen titles, labels, notes · proportional only",
      command: `/theme ui-font "${settings.surfaceSansFace}"`,
      chosen: settings.surfaceSansFace,
      options: [],
    },
  ];
}

/** What choosing a card means. Unknown rows leave the settings alone rather than guessing. */
export function chose(settings: Settings, row: SettingRow, option: Option): Settings {
  switch (row.key as RowKey | undefined) {
    case "palette":
      return { ...settings, surfacePalette: option.name as SurfacePalette };
    case "chrome":
      return { ...settings, surfaceChrome: option.name as SurfaceChrome };
    case "tail":
      return { ...settings, surfaceTailKeys: option.name === "hidden" ? "hidden" : "shown" };
    case "focus":
      return { ...settings, surfaceFocus: option.name === "wash" ? "wash" : "outline" };
    case "density":
      return { ...settings, surfaceDensity: option.name === "dense" ? "dense" : "normal" };
    case "face":
      return validFace(option.name) ? { ...settings, surfaceFace: option.name } : settings;
    case "sansFace":
      return validFace(option.name) ? { ...settings, surfaceSansFace: option.name } : settings;
    case "open":
      return { ...settings, openIn: option.name as OpenIn };
    default:
      return settings;
  }
}

/** The preview line, drawn in whatever face and palette is currently chosen. */
export function previewOf(settings: Settings): Segment[] {
  return [
    { text: `${settings.surfaceFace} 13 / ${settings.surfaceDensity === "dense" ? "1.4" : "1.62"} · ${settings.surfaceSansFace}`, role: "mono-dim" },
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
}

function editorFacts(pack: Language | undefined): SectionView {
  if (pack === undefined) return { rows: [], facts: [], empty: "waiting for the engine's language package" };
  return {
    rows: [],
    facts: [
      fact("language", `${pack.package.language} · version ${pack.package.version}`),
      fact("read from", pack.origin === "engine" ? "the engine" : "the copy bundled with the client"),
      fact("statements", String(pack.keywords().length)),
      fact("operators", String(pack.operators().length)),
      fact("operations", String(pack.operations().length)),
    ],
  };
}

function resultFacts(settings: Settings): SectionView {
  return {
    rows: [
      {
        key: "open",
        label: "/open",
        command: "/settings results open window",
        chosen: settings.openIn,
        options: [
          { name: "window", what: "its own window" },
          { name: "screen", what: "over the workspace" },
        ],
      },
    ],
    facts: [
      fact("preview", `${settings.previewChars} characters of a value`),
      fact("follow", settings.stayAtNewest ? "stay with the newest cell" : "stay where it was left"),
      fact("graph grows", settings.direction === "LR" ? "left to right" : "top to bottom"),
    ],
  };
}

/**
 * The keys the surface answers to, said once here so nobody has to find them by pressing things.
 *
 * Each says whose key it is: the prompt's while typing there, or the focused cell's once the cell
 * itself — not a field or button in it — holds focus. `⌘⇧F` is the Meta key on every platform.
 */
function keyFacts(): SectionView {
  const mod = primaryGlyph();
  return {
    rows: [],
    facts: [
      fact("↑ ↓", "prompt: earlier and later commands"),
      fact("⇥", "prompt: complete what is being typed"),
      fact("⌃space", "prompt: ask for the completion list"),
      fact(`${mod}⏎`, "prompt: run, without accepting a completion"),
      fact("⇧⏎", "prompt: another line, without running it"),
      fact(`${mod}⇧⏎`, "prompt: open the draft in the editor"),
      fact("⌃⇧C", "prompt: toggle action label style"),
      fact("esc", "leave a screen, keeping the half-typed line"),
      fact("⌘⇧F", "expand the focused pane with its tabs; esc restores panes"),
      fact("j  k", "focused cell: the next, the previous cell"),
      fact("r  p  o", "focused cell: repeat, pin the cell in the scrollback, inspect"),
      fact("v  d", "focused cell: its JSON tab, its run details"),
      fact("x", "focused cell: cancel what is running"),
      fact(`${mod}M`, "focused cell: its actions menu"),
      fact("/graph /stale /env /settings /open", "the screens"),
    ],
  };
}

function connectionFacts(workspace: Workspace): SectionView {
  const providers = workspace.catalogue.providers;
  if (providers.length === 0) return { rows: [], facts: [], empty: "no providers are configured yet" };
  return {
    rows: [],
    facts: providers.map((provider) => {
      const wanted = provider.credentials.length;
      const present = provider.credentials.filter((credential) => credential.supplied).length;
      const said =
        wanted === 0
          ? `${provider.capabilities.length} capabilities · no credentials wanted`
          : `${provider.capabilities.length} capabilities · ${present} of ${wanted} credentials`;
      return [
        { text: padded(provider.name, FACT_WIDTH), role: "mono-provider" as const },
        { text: said, role: provider.ready ? ("mono-literal" as const) : ("mono-warn" as const) },
      ];
    }),
  };
}

function aliasFacts(settings: Settings): SectionView {
  const names = Object.keys(settings.aliases);
  if (names.length === 0) return { rows: [], facts: [], empty: "nothing to set yet" };
  return { rows: [], facts: names.map((name) => fact(name, settings.aliases[name] ?? "")) };
}

/** What this session is keeping, as the engine last said it. */
function dataFacts(workspace: Workspace): SectionView {
  const kept = workspace.nodes.filter((node) => node.kept).length;
  const facts: Segment[][] = [
    fact("keeping", workspace.keeping.automatic ? "results under a size, automatically" : "only what was kept by hand"),
    fact("under", describeSize(workspace.keeping.under)),
    fact("kept now", `${kept} of ${workspace.nodes.length} results`),
  ];
  // Where the engine keeps things is only sent when a client asks for it, and the surface does not.
  for (const kind of workspace.storage?.retention?.classes ?? []) {
    facts.push(fact(kind.kind, `${kind.count} results · ${describeSize(kind.bytes)}`));
  }
  return { rows: [], facts };
}

export function readSection(section: SectionName, input: SettingsInput): SectionView {
  switch (section) {
    case "appearance": return { rows: appearanceRows(input.settings), facts: [] };
    case "editor": return editorFacts(input.pack);
    case "results": return resultFacts(input.settings);
    case "keys": return keyFacts();
    case "connections": return connectionFacts(input.workspace);
    case "aliases": return aliasFacts(input.settings);
    case "data": return dataFacts(input.workspace);
    case "limits": return {rows: [], facts: []};
  }
}
