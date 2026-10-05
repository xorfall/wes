/**
 * What a `/` word does on the surface.
 *
 * `/settings` says at the bottom that every row on it is a command you can type, and that sentence
 * is only true if the commands exist. This is where they exist: the screens summoned by name, and
 * the surface's own settings as `/theme`, which is the same grammar the screen prints beside each
 * row. The two files change together — a row may only show a command this one answers.
 *
 * A `/` word is the client's, never the engine's, so one that is not here is said to be
 * not here rather than sent on to be refused by something that was never asked.
 */
import { terminalArguments, terminalCommand, terminalTabCommand, type TerminalIntent } from "./terminal-command";
import type { SplitDirection } from "./split-model";
import type { Direction, Settings, SurfacePalette } from "../settings";
import { faceChosen, sectionNamed, type SectionName } from "./settings-model";
import { workspaceName } from "../workspace-binding";

/** The screens a command can summon over the session. */
export type Summoned = "dashboard" | "graph" | "stale" | "env" | "settings" | "open" | "edit" | "peek" | "spec";

/** How the workspace is divided. `right` and `down` make two; `3` and `4` make that many. */
export type Division = "right" | "down" | 3 | 4;

export type PaneContent =
  | ({ readonly terminal: true } & TerminalIntent)
  | { readonly workspace: string }
  | { readonly value: string; readonly related?: true }
  | { readonly screen: Summoned; readonly node?: string; readonly section?: SectionName; readonly fileContext?: EditFileContext; readonly fileName?: string };

/** `/edit env|types [name]` — a saved buffer, not a bound `:calc` program. */
export type EditFileContext = "env" | "types";

export type Typed =
  | { readonly kind: "goto"; readonly node: string }
  | { readonly kind: "value-tab"; readonly node: string; readonly pane?: string; readonly activate: boolean; readonly related?: true }
  | { readonly kind: "workspace-delete" }
  /** `/terminal-tab` leaves `activate` and `pane` unset; `/tab xterm` and `/tabx xterm` set `activate`. */
  | ({ readonly kind: "terminal-tab"; readonly activate?: boolean; readonly pane?: string } & TerminalIntent)
  | { readonly kind: "workspace-tab"; readonly workspace: string; readonly pane?: string; readonly activate: boolean }
  | { readonly kind: "clear" | "debug" }
  | { readonly kind: "aliases"; readonly command: string }
  | { readonly kind: "directional-split"; readonly direction: SplitDirection; readonly takeFocus: boolean; readonly content?: PaneContent }
  | {
      readonly kind: "screen";
      readonly screen: Summoned;
      readonly section?: SectionName;
      /** `/open $orders` and `/edit $revenue` name one; on their own they take what is focused. */
      readonly node?: string;
      /** `/edit env|types`, and the name it was given, if it was. */
      readonly fileContext?: EditFileContext;
      readonly fileName?: string;
      /**
       * `… split` — open it in a pane beside the session rather than over the workspace.
       *
       * Every screen command takes it, and where it lands is remembered per command,
       * so the second `/graph split` goes back to the pane the first one went to.
       */
      readonly inPane?: boolean;
    }
  /** `/split right`, `/split 3` — divide the workspace, the session keeping its pane. */
  | { readonly kind: "split"; readonly how: Division }
  /** `/close` — close the focused pane. The last pane never closes. */
  | { readonly kind: "close" }
  /** Change the settings and keep the prompt where it is. */
  | { readonly kind: "settings"; readonly change: (was: Settings) => Settings }
  | { readonly kind: "trouble"; readonly said: string }
  /** Not a client command at all: it belongs to the engine. */
  | { readonly kind: "engine" };

/**
 * The words of a command line, with a quoted run counting as one.
 *
 * Enough for `/theme font "PT Mono" dense` and no more: the engine parses the language, and this
 * parses four client commands.
 */
export function words(line: string): string[] {
  const found = line.match(/"(?:\\.|[^"\\])*"|\S+/g) ?? [];
  return found.map(word => {
    if (!word.startsWith('"') || !word.endsWith('"')) return word;
    try { return JSON.parse(word) as string; } catch { return word.slice(1, -1); }
  });
}

/** The exact source after the command name, so quoted terminal names keep their spaces and escapes. */
function afterName(text: string): string {
  return text.trim().replace(/^\S+/, "");
}

/** `/graph`, `/stale`, `/env`, `/settings`, `/open` — and nothing else is a screen. */
export function summonedBy(text: string): Summoned | undefined {
  const typed = read(text);
  return typed.kind === "screen" ? typed.screen : undefined;
}

export function read(text: string): Typed {
  if (text.trimStart().startsWith("//")) return { kind: "engine" };
  const said = words(text.trim());
  const name = said[0] ?? "";
  if (!name.startsWith("/")) return { kind: "engine" };
  // Preserve the exact source: quoted strings and a trailing `split` are alias data.
  if (name === "/alias" || name === "/unalias") return { kind: "aliases", command: text.trim() };
  if (name === "/clear" || name === "/debug") {
    return said.length === 1 ? { kind: name === "/clear" ? "clear" : "debug" }
      : { kind: "trouble", said: `${name} takes no arguments` };
  }
  if (name === "/workspace" && said[1] === "delete") return said.length === 2 ? {kind:"workspace-delete"} : {kind:"trouble",said:"Use /workspace delete in the workspace you want to remove."};
  if (name === "/terminal-tab") {
    try { return { kind: "terminal-tab", ...terminalArguments(text.trim().replace(/^\/terminal-tab/, "")) }; }
    catch (error) { return { kind: "trouble", said: (error as Error).message }; }
  }
  if (name === "/goto") {
    const node = said[1]?.replace(/^\$/, "");
    return said.length === 2 && node && !/[\s$\u0000-\u001f]/.test(node)
      ? { kind: "goto", node } : { kind: "trouble", said: "Use /goto NAME or /goto $NAME to find its definition in this workspace." };
  }
  if (name === "/tab" || name === "/tabx") {
    // `xterm` is reserved for terminals, quoted or not; it never names a workspace here.
    if (said[1] === "xterm") {
      try { return { kind: "terminal-tab", activate: name === "/tabx", ...terminalTabCommand(afterName(text)) }; }
      catch (error) { return { kind: "trouble", said: (error as Error).message }; }
    }
    const related = said[1] === "related" && !/^\/tabx?\s+"related"(?:\s|$)/.test(text.trim());
    if (related && !said[2]?.startsWith("$")) return { kind: "trouble", said: "Use /tab related $NAME or /tabx related $NAME to open related commands." };
    const args = related ? [said[0]!, ...said.slice(2)] : said;
    if (args[1]?.startsWith("$")) {
      if (args.length > 3 || !/^\$[^\s$\u0000-\u001f]+$/.test(args[1]) || (args[2] !== undefined && !/^pane:p[1-9][0-9]*$/.test(args[2])))
        return { kind: "trouble", said: `Use /tab ${related ? "related " : ""}$NAME [pane:p1] or /tabx ${related ? "related " : ""}$NAME [pane:p1] to open ${related ? "related commands" : "an existing value"}.` };
      return { kind: "value-tab", node: args[1].slice(1), activate: name === "/tabx", ...(args[2] ? { pane: args[2].slice(5) } : {}), ...(related ? { related: true } : {}) };
    }
    if (said.length < 2 || said.length > 3 || !workspaceName(said[1]) || (said[2] !== undefined && !/^pane:p[1-9][0-9]*$/.test(said[2])))
      return { kind: "trouble", said: 'Use /tab NAME [pane:p1] or /tabx NAME [pane:p1] to open a workspace tab, or /tab xterm or /tabx xterm [pane:pN] [env:NAME] [target:NAME] to add a terminal tab.' };
    return { kind: "workspace-tab", workspace: said[1], activate: name === "/tabx", ...(said[2] ? { pane: said[2].slice(5) } : {}) };
  }
  const directional = /^\/([lrtb])split(x)?$/.exec(name);
  if (directional) {
    const direction = ({ l: "left", r: "right", t: "up", b: "down" } as const)[directional[1] as "l" | "r" | "t" | "b"];
    const base = { kind: "directional-split" as const, direction, takeFocus: !!directional[2] };
    const args = said.slice(1);
    if (!args.length) return base;
    const related = args[0] === "related" && !/^\/\S+\s+"related"(?:\s|$)/.test(text.trim());
    if (args.length === 2 && related && /^\$[^\s$\u0000-\u001f]+$/.test(args[1]!)) return { ...base, content: { value: args[1]!.slice(1), related: true } };
    if (related) return { kind: "trouble", said: `Use ${name} related $NAME to open related commands.` };
    if (args[0] === "xterm") {
      try { return { ...base, content: { terminal: true, ...terminalCommand(afterName(text)) } }; }
      catch (error) { return { kind: "trouble", said: (error as Error).message }; }
    }
    if (args.length === 1 && /^\$[^\s$\u0000-\u001f]+$/.test(args[0]!)) return { ...base, content: { value: args[0]!.slice(1) } };
    if (args.length === 1 && !args[0]!.startsWith("/") && !args[0]!.startsWith("$")) {
      return workspaceName(args[0]) ? { ...base, content: { workspace: args[0] } }
        : { kind: "trouble", said: "Invalid workspace name: use 1–96 bytes without path separators, '..' or control characters." };
    }
    const target = args[0]!.replace(/^\//, "");
    const allowed: Record<string, number> = { dashboard:1,spec: 0, graph: 0, stale: 0, env: 0, settings: 1, open: 1, edit: 2 };
    if (!args[0]!.startsWith("/") || !Object.hasOwn(allowed, target) || args.length - 1 > allowed[target]! || args.at(-1) === "split")
      return { kind: "trouble", said: `${name} takes a workspace name, $node, xterm or a slash-prefixed screen command (/graph, /stale, /env, /settings, /open, /edit)` };
    if (target === "settings" && args[1] !== undefined && sectionNamed(args[1]) !== args[1])
      return { kind: "trouble", said: `Unknown settings section '${args[1]}'` };
    const content = read(`/${target} ${args.slice(1).join(" ")}`);
    return content.kind === "screen" ? { ...base, content: { screen: content.screen,
      ...(content.node === undefined ? {} : { node: content.node }), ...(content.section === undefined ? {} : { section: content.section }),
      ...(content.fileContext === undefined ? {} : { fileContext: content.fileContext }), ...(content.fileName === undefined ? {} : { fileName: content.fileName }) } }
      : { kind: "trouble", said: "This content cannot open in a pane" };
  }
  /*
   * `split` on the end of any screen command is the suffix, not an argument.
   *
   * Taken off before the command reads its own words, so `/settings connections split` is still
   * `/settings connections`, and `/open $x split` still names `$x`. A command that is not a screen
   * never sees it, and `/split` itself is a command rather than a suffix.
   */
  const suffixed = name !== "/split" && said.length > 1 && said[said.length - 1] === "split";
  const rest = said.slice(1, suffixed ? -1 : undefined);
  const inPane: { readonly inPane?: true } = suffixed ? { inPane: true } : {};
  switch (name) {
    case '/dashboard': {
      if(rest.length>1||rest[0]!==undefined&&!/^\$[^\s$\u0000-\u001f]+$/.test(rest[0]))return {kind:'trouble',said:'Use /dashboard to create or choose a dashboard, or /dashboard $NAME to edit it.'};
      return {kind:'screen',screen:'dashboard',...(rest[0]?{node:rest[0].slice(1)}:{}),...inPane};
    }
    case "/graph": return { kind: "screen", screen: "graph", ...inPane };
    case "/stale": return { kind: "screen", screen: "stale", ...inPane };
    case "/spec": return rest.length ? {kind:"trouble",said:"/spec takes no arguments"} : {kind:"screen",screen:"spec",...inPane};
    case "/env": return { kind: "screen", screen: "env", ...inPane };
    case "/edit": {
      const first = rest[0];
      if (first === "env" || first === "types") {
        const fileName = rest[1];
        return { kind: "screen", screen: "edit", fileContext: first, ...(fileName === undefined ? {} : { fileName }), ...inPane };
      }
      const named = first;
      return named === undefined
        ? { kind: "screen", screen: "edit", ...inPane }
        : { kind: "screen", screen: "edit", node: named.startsWith("$") ? named.slice(1) : named, ...inPane };
    }
    case "/open": {
      const named = rest[0];
      return named === undefined
        ? { kind: "screen", screen: "open", ...inPane }
        : { kind: "screen", screen: "open", node: named.startsWith("$") ? named.slice(1) : named, ...inPane };
    }
    case "/settings": return settings(rest, inPane);
    case "/split": {
      if (rest[0] === "xterm") return read(`/rsplit${afterName(text)}`);
      if (rest[0] === "related") return rest.length === 2 && rest[1]?.startsWith("$") ? read(`/rsplit ${rest.join(" ")}`) : { kind: "trouble", said: "Use /split related $NAME to open related commands." };
      if (rest.length === 1 && rest[0]!.startsWith("$")) return read(`/rsplit ${rest[0]}`);
      if (rest.length > 1) {
        if (rest[0] !== "right" && rest[0] !== "down") return { kind: "trouble", said: "/split takes right or down before its content" };
        return read(`/${rest[0] === "right" ? "r" : "b"}split ${text.trim().replace(/^\/split\s+(?:right|down)\s+/, "")}`);
      }
      return divide(rest[0]);
    }
    case "/close": return { kind: "close" };
    case "/theme": return theme(rest);
    case "/layout": return layout(rest[0]);
    case "/follow": return follow(rest[0]);
    default:
      return { kind: "trouble", said: `'${name}' is not one of the surface's commands` };
  }
}

/** The supported palette names. */
const PALETTES: Readonly<Record<string, SurfacePalette>> = {
  paper: "paper", ink: "ink", white: "white", system: "system",
};

const CHROMES = ["controls", "keys"] as const;

function theme(rest: readonly string[]): Typed {
  const first = rest[0];
  // `/theme` on its own switches, between paper and ink.
  if (first === undefined) {
    return {
      kind: "settings",
      change: (was) => ({ ...was, surfacePalette: was.surfacePalette === "ink" ? "paper" : "ink" }),
    };
  }
  if (first === "cell") {
    const chrome = CHROMES.find((it) => it === rest[1]);
    if (chrome === undefined) return { kind: "trouble", said: `'/theme cell' takes ${CHROMES.join(", ")}` };
    return { kind: "settings", change: (was) => ({ ...was, surfaceChrome: chrome }) };
  }
  if (first === "keys") {
    const shown = rest[1] === "shown" ? "shown" : rest[1] === "hidden" ? "hidden" : undefined;
    if (shown === undefined) return { kind: "trouble", said: "'/theme keys' takes shown or hidden" };
    return { kind: "settings", change: (was) => ({ ...was, surfaceTailKeys: shown }) };
  }
  if (first === "focus") {
    const focus = rest[1] === "outline" ? "outline" : rest[1] === "wash" ? "wash" : undefined;
    if (focus === undefined) return { kind: "trouble", said: "'/theme focus' takes outline or wash" };
    return { kind: "settings", change: (was) => ({ ...was, surfaceFocus: focus }) };
  }
  if (first === "font" || first === "ui-font") {
    const { face, density } = faceChosen(rest.slice(1).join(" "));
    if (face === undefined) return { kind: "trouble", said: `'/theme ${first}' takes a font family name${first === "font" ? ", and 'dense' or 'normal'" : ""}` };
    return first === "font"
      ? { kind: "settings", change: (was) => ({ ...was, surfaceFace: face, ...(density ? { surfaceDensity: density } : {}) }) }
      : { kind: "settings", change: (was) => ({ ...was, surfaceSansFace: face }) };
  }
  if (first === "density") {
    const density = rest[1] === "dense" ? "dense" : rest[1] === "normal" ? "normal" : undefined;
    if (density === undefined) return { kind: "trouble", said: "'/theme density' takes normal or dense" };
    return { kind: "settings", change: (was) => ({ ...was, surfaceDensity: density }) };
  }
  const palette = PALETTES[first];
  if (palette === undefined) {
    return { kind: "trouble", said: `'/theme' takes paper, ink, white, system, 'cell …', 'keys …', 'focus …', 'density …', 'font …' or 'ui-font …'` };
  }
  return { kind: "settings", change: (was) => ({ ...was, surfacePalette: palette }) };
}

/**
 * `/settings`, `/settings results`, `/settings results open window`.
 *
 * The screen's own sentence is that every row on it is a command you can type, so a row that offers
 * a choice has to be settable from the line as well as from the card. Only the rows that offer a
 * choice answer here; a section with nothing to choose just opens.
 */
function settings(rest: readonly string[], inPane: { readonly inPane?: true }): Typed {
  const section = sectionNamed(rest[0]);
  const [, row, option] = rest;
  if (row === undefined) return { kind: "screen", screen: "settings", section, ...inPane };
  if (section === "results" && row === "open") {
    if (option !== "window" && option !== "screen") {
      return { kind: "trouble", said: "'/settings results open' takes window or screen" };
    }
    return { kind: "settings", change: (was) => ({ ...was, openIn: option }) };
  }
  return { kind: "trouble", said: `'/settings ${rest[0]}' has no '${row}' to set` };
}

/**
 * `/split right`, `/split down`, `/split 3`, `/split 4`.
 *
 * With no argument it is `right`, because that is the one somebody means when they say "split": a
 * second surface beside this one. The session keeps the pane it is in either way — dividing the
 * workspace is not leaving it.
 */
function divide(word: string | undefined): Typed {
  if (word === undefined || word === "right") return { kind: "split", how: "right" };
  if (word === "down") return { kind: "split", how: "down" };
  if (word === "3" || word === "4") return { kind: "split", how: Number(word) as 3 | 4 };
  return { kind: "trouble", said: "'/split' takes right, down, 3 or 4" };
}

/** Whether the scrollback stays with the newest line. No argument switches. */
function follow(word: string | undefined): Typed {
  if (word === undefined) {
    return { kind: "settings", change: (was) => ({ ...was, stayAtNewest: !was.stayAtNewest }) };
  }
  if (word !== "on" && word !== "off") return { kind: "trouble", said: "'/follow' takes on or off" };
  return { kind: "settings", change: (was) => ({ ...was, stayAtNewest: word === "on" }) };
}

function layout(word: string | undefined): Typed {
  const direction: Direction | undefined =
    word === undefined ? undefined : word.toLowerCase() === "lr" ? "LR" : word.toLowerCase() === "tb" ? "TB" : undefined;
  if (direction === undefined) {
    // No argument turns the layout.
    if (word === undefined) {
      return { kind: "settings", change: (was) => ({ ...was, direction: was.direction === "LR" ? "TB" : "LR" }) };
    }
    return { kind: "trouble", said: "'/layout' takes lr or tb" };
  }
  return { kind: "settings", change: (was) => ({ ...was, direction }) };
}
