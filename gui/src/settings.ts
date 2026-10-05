/**
 * What belongs to the client and not to the engine.
 *
 * The line is which side would still care if the other were replaced. A theme is about looking at
 * results, not about producing them: a second client would want its own, and the engine would be
 * carrying state it can neither use nor check. Browsers store these locally; the desktop shell
 * persists its client's preferences through a separate HTTP endpoint, outside engine/workspace state.
 */
import { restoreAliases, emptyAliases, type Aliases } from "./aliases";
import { restoreSplit } from "./surface/split-storage";
import type { SplitState } from "./surface/split-model";
import { desktopSnapshot, saveDesktopPreferences } from "./desktop-preferences";
import { restoreTableViews, type TableViews } from "./presentation/table-views";
import {restoreDashboards,type SavedDashboard} from './dashboard/storage';

export type Direction = "LR" | "TB";

/**
 * The cell theme, one setting for every cell: `controls` draws the actions as chips
 * under the source; `keys` carries the same actions as keys in the tail.
 */
export type SurfaceChrome = "controls" | "keys";
export type SurfaceTailKeys = "shown" | "hidden";
/** How a focused cell is marked: its violet boundary alone, or the boundary over the selection wash. */
export type SurfaceFocus = "outline" | "wash";
/** `system` is not a third palette: it is paper or ink, whichever the machine is set to. */
/**
 * Paper is the base palette, ink the dark one, white a plain one.
 *
 * White is paper's own role colours on a pure white ground: the same green for a provider call and
 * the same amber for a literal, with the warmth taken out of the paper it is printed on. It is
 * chosen and never inferred — `system` still means the machine's light mode, which is paper.
 */
export type SurfacePalette = "paper" | "ink" | "white" | "system";
export type SurfaceDensity = "normal" | "dense";
/**
 * Which face leads the mono chain.
 *
 * Only PT Mono ships with the client. JetBrains Mono is the design system's first fallback, and SF
 * Mono is Apple's, installed on macOS and shipped by nobody — neither may be redistributed, so both
 * are asked for with `local()` and nothing else. Choosing one on a machine without it falls through
 * the chain to PT Mono, which is what a chain is for; the card says so rather than pretending.
 */
export type SurfaceFace = string;

/** The mono faces the surface knows by name before the machine's catalogue is read. */
export const SURFACE_FACES: readonly SurfaceFace[] = ["PT Mono", "JetBrains Mono", "SF Mono"];
/** The data face the client ships, and the interface face it ships. */
export const DEFAULT_FACE: SurfaceFace = "PT Mono";
export const DEFAULT_SANS_FACE: SurfaceFace = "IBM Plex Sans";

/**
 * A family name as a setting may hold it: the name of an installed font, never CSS. Quotes,
 * backslashes, control characters and CSS punctuation are refused, and names stay short, so a stored
 * or typed name can only ever select a font.
 */
export function validFace(value: unknown): value is SurfaceFace {
  return typeof value === "string" && value.trim() === value && value.length > 0 && value.length <= 100
    && !/["'\\;{}<>\u0000-\u001f\u007f]/.test(value);
}

export interface Settings {
  readonly dashboards?:readonly SavedDashboard[];
  readonly paneLayout?: SplitState;
  /** Terminal surface: one cell theme for every cell; there is no per-cell override. */
  readonly surfaceChrome: SurfaceChrome;
  /**
   * Keys theme: whether the tail lists the cell's actions as keys. Hidden, the tail keeps its
   * counts and every key still works; only the reminder goes.
   */
  readonly surfaceTailKeys: SurfaceTailKeys;
  /** Terminal surface: whether a focused cell also takes the selection wash behind its boundary. */
  readonly surfaceFocus: SurfaceFocus;
  /** Terminal surface: paper is the base palette, ink the other end of it. */
  readonly surfacePalette: SurfacePalette;
  /** Terminal surface: how tightly the mono lines are set. */
  readonly surfaceDensity: SurfaceDensity;
  /** Terminal surface: which face leads the mono chain (data: commands, results, tables). */
  readonly surfaceFace: SurfaceFace;
  /** Terminal surface: which face leads the sans chain (screen chrome: titles, labels, notes). */
  readonly surfaceSansFace: SurfaceFace;
  /** Which way the graph grows. Left to right reads like the commands that made it. */
  readonly direction: Direction;
  /** Whether the scrollback stays with the newest cell instead of where it was left. */
  readonly stayAtNewest: boolean;
  /**
   * Where opening a result puts it.
   *
   * `window` opens a result in a real second window in the desktop
   * app, a second tab of the same client in a browser, either way leaving the session exactly where
   * it was. `screen` is the summoned screen over the workspace. The default follows where the
   * client is running — a desktop app has windows to give, a browser tab mostly does not.
   */
  readonly openIn: OpenIn;
  readonly aliases: Aliases;
  /** How much of a value to print before cutting it. */
  readonly previewChars: number;
  /**
   * How long to wait for the engine before saying it did not answer. Source submissions wait at
   * least five minutes for preparation; longer configured waits are honored.
   *
   * A timeout is a decision, not a discovery: a slow engine and a dead one look the same from here. So
   * this only decides when to stop waiting — never whether to ask again, which is the person's call.
   */
  readonly requestTimeoutMs: number;
  /** How each row type's tables are arranged: hidden columns, widths, the key column's pin. */
  readonly tables: TableViews;
}

export const defaults: Settings = {
  aliases: emptyAliases,
  surfaceChrome: "keys",
  surfaceTailKeys: "shown",
  surfaceFocus: "outline",
  surfacePalette: "system",
  surfaceDensity: "normal",
  surfaceFace: DEFAULT_FACE,
  surfaceSansFace: DEFAULT_SANS_FACE,
  direction: "LR",
  stayAtNewest: true,
  openIn: defaultOpenIn(),
  previewChars: 4000,
  requestTimeoutMs: 15_000,
  tables: {},
};

/** Over the workspace, or in a window of its own. */
export type OpenIn = "window" | "screen";

/**
 * A desktop app can open a window; a browser tab is somebody else's window and mostly cannot.
 *
 * Read once at module load, which is where `defaults` is built. Nothing about this changes while
 * the client is open, and a saved choice overrides it either way.
 */
export function defaultOpenIn(): OpenIn {
  return typeof window !== "undefined" && window.__WES_DESKTOP__ === true ? "window" : "screen";
}

const KEY = "wes.settings";

/**
 * Settings that reset on reload are settings nobody changes. Storage can be unavailable — private
 * windows, a locked-down profile — and that is not worth failing over, so it degrades to the defaults.
 */
export function load(): Settings {
  const desktop = desktopSnapshot();
  if (desktop !== undefined) return restoreSettings(desktop);
  try {
    const stored = window.localStorage.getItem(KEY);
    return stored === null ? defaults : restoreSettings(JSON.parse(stored));
  } catch {
    return defaults;
  }
}

/** Restore supported personal preferences; unknown keys never affect the client. */
export function restoreSettings(value: unknown): Settings {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return defaults;
  const saved = value as Partial<Settings>;
  return { ...defaults,
    ...(saved.dashboards===undefined?{}:{dashboards:restoreDashboards(saved.dashboards)}),
    aliases: restoreAliases(saved.aliases),
    ...(saved.paneLayout === undefined ? {} : { paneLayout: restoreSplit(saved.paneLayout) }),
    surfaceChrome: saved.surfaceChrome === "controls" || saved.surfaceChrome === "keys"
      ? saved.surfaceChrome : defaults.surfaceChrome,
    surfaceTailKeys: saved.surfaceTailKeys === "hidden" ? "hidden" : "shown",
    surfaceFocus: saved.surfaceFocus === "wash" ? "wash" : "outline",
    surfacePalette: saved.surfacePalette === "ink" || saved.surfacePalette === "white" || saved.surfacePalette === "system"
      ? saved.surfacePalette : saved.surfacePalette === "paper" ? "paper" : defaults.surfacePalette,
    surfaceDensity: saved.surfaceDensity === "dense" ? "dense" : "normal",
    surfaceFace: validFace(saved.surfaceFace) ? saved.surfaceFace : DEFAULT_FACE,
    surfaceSansFace: validFace(saved.surfaceSansFace) ? saved.surfaceSansFace : DEFAULT_SANS_FACE,
    direction: saved.direction === "TB" ? "TB" : "LR",
    stayAtNewest: saved.stayAtNewest !== false,
    openIn: saved.openIn === "window" || saved.openIn === "screen" ? saved.openIn : defaultOpenIn(),
    previewChars: typeof saved.previewChars === "number" && Number.isFinite(saved.previewChars) && saved.previewChars > 0
      ? saved.previewChars : defaults.previewChars,
    requestTimeoutMs: typeof saved.requestTimeoutMs === "number" && Number.isFinite(saved.requestTimeoutMs) && saved.requestTimeoutMs > 0
      ? saved.requestTimeoutMs : defaults.requestTimeoutMs,
    tables: restoreTableViews(saved.tables),
  };
}

/**
 * Which palette `system` is right now.
 *
 * Resolved here rather than left to a media query, so one place decides and the surface's
 * `data-palette` carries an answer the rest of the CSS can rely on.
 */
/**
 * The palette actually drawn, which is the chosen one unless the choice was `system`.
 *
 * `system` is the machine's light or dark, and the machine has no third state — white is a palette
 * somebody picks, never one inferred from a preference nobody expressed. So light stays paper.
 */
export function resolveSurfacePalette(palette: SurfacePalette): "paper" | "ink" | "white" {
  if (palette !== "system") return palette;
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return "paper";
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "ink" : "paper";
}

/**
 * The mono chain, with the chosen face at its head and the design system's own order behind it.
 *
 * The tail is the same whichever face leads, so a machine missing the chosen one lands on the same
 * grid it would have had anyway rather than on whatever the browser calls monospace.
 */
export function monoFamily(face: SurfaceFace): string {
  const chosen = `"${face}"`;
  return [chosen, ...MONO_CHAIN.filter((it) => it !== chosen)].join(", ");
}

/**
 * The design system's own `--type-mono-family`, which is the chain every face falls through.
 *
 * Written here because a function cannot read a stylesheet, and asserted against `tokens.css` in
 * `tokens.test.ts` so the two can only ever say the same thing.
 */
const MONO_CHAIN = ['"PT Mono"', '"JetBrains Mono"', "ui-monospace", "Menlo", "monospace"];

/** The design system's sans chain behind the chosen interface face; asserted against `tokens.css`. */
const SANS_CHAIN = ['"IBM Plex Sans"', "ui-sans-serif", "system-ui", "sans-serif"];

/** The sans chain, with the chosen face at its head and the design system's own order behind it. */
export function sansFamily(face: SurfaceFace): string {
  const chosen = `"${face}"`;
  return [chosen, ...SANS_CHAIN.filter((it) => it !== chosen)].join(", ");
}

/** Every sans family token the design system defines: chrome follows the interface face everywhere. */
export const SANS_FAMILY_TOKENS = [
  "--type-sans-family", "--type-sans-small-family", "--type-display-family", "--type-headline-family",
  "--type-title-family", "--type-body-family", "--type-label-family", "--font-family",
] as const;
/** Every mono family token: bold data follows the data face as regular data does. */
export const MONO_FAMILY_TOKENS = ["--type-mono-family", "--type-mono-strong-family"] as const;

/**
 * The type variables a surface root carries for the chosen faces. Every family token follows its
 * choice, or part of the surface stays in the default face beside the rest in the chosen one.
 * `tokens.test.ts` holds both lists to every family token the design system defines.
 */
export function surfaceTypeStyle(faces: Pick<Settings, "surfaceFace" | "surfaceSansFace">): Record<string, string> {
  const mono = monoFamily(faces.surfaceFace);
  const sans = sansFamily(faces.surfaceSansFace);
  return Object.fromEntries([
    ...MONO_FAMILY_TOKENS.map((token) => [token, mono]),
    ...SANS_FAMILY_TOKENS.map((token) => [token, sans]),
  ]);
}

export function save(settings: Settings): void {
  void saveDesktopPreferences(settings);
  try {
    window.localStorage.setItem(KEY, JSON.stringify(settings));
  } catch {
    // nothing worth interrupting anyone over
  }
}
