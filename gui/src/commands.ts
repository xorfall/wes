import type { Diagnostic } from "./protocol";
import { defaults, type Direction, type Settings, type SurfacePalette } from "./settings";
import { defineAlias } from "./aliases";
import { emptyCatalogue } from "./vocabulary";
import { commandNamed, type Catalogue } from "./vocabulary";

/**
 * Commands the client answers itself.
 *
 * Written with a leading `/`, and the marker is not decoration. wes already has two namespaces that can
 * collide — meta commands and provider names — so an explicit marker separates them. A client command written as a bare word would be a
 * third thing to collide, so it gets its own mark: `:` is the engine's, `@` is an annotation's, `/` is
 * the client's.
 *
 * The engine never learns these exist. That is the test for whether something belongs here: if the
 * engine would have to know about it, it is not a client command.
 */

export interface Outcome {
  /** Whether to ask the engine where it keeps things before answering. */
  readonly askStorage?: boolean;
  /** What the client should look like afterwards, if this changed anything. */
  readonly settings?: Settings;
  /** What to say about it. */
  readonly said: readonly Diagnostic[];
  /** Whether to clear the visible screen. The scrollback is kept; earlier cells scroll back into view. */
  readonly clear?: boolean;
  /** Whether to open the settings panel. */
  readonly open?: boolean;
}

export function isClientCommand(text: string): boolean {
  return text.trimStart().startsWith("/");
}

/** The names, so the client can suggest its own commands the way it suggests the engine's. */
export function clientCommandNames(): readonly string[] {
  return OWN.map(([syntax]) => syntax.split(/\s/)[0]!.slice(1));
}

export function run(text: string, settings: Settings, catalogue?: Catalogue): Outcome {
  const match = /^\/(\S+)(?:[ \t]+([\s\S]*))?$/.exec(text.trim());
  const name = match?.[1];
  const argument = match?.[2] ?? "";
  switch (name) {
    case "help":
      return { said: [note(helpOn(argument, catalogue))] };
    case "theme":
      return theme(argument, settings);
    case "layout":
      return layout(argument, settings);
    case "follow":
      return follow(argument, settings);
    case "clear":
      return { said: [], clear: true };
    case "settings":
      return { said: [], open: true };
    case "alias":
      return alias(argument, settings, catalogue ?? emptyCatalogue);
    case "unalias": {
      if (!Object.hasOwn(settings.aliases, argument)) return { said: [problem(`No alias named '${argument}'.`, [])] };
      const aliases = { ...settings.aliases }; delete aliases[argument];
      return { settings: { ...settings, aliases }, said: [note(`Removed alias ${argument}.`)] };
    }
    case "debug":
      return { said: [], askStorage: true };
    case "reset":
      return { settings: defaults, said: [note("settings back to their defaults")] };
    default:
      return {
        said: [
          problem(`there is no client command called '/${name ?? ""}'`, [
            "'/help' lists them",
            "commands without '/' go to the engine",
          ]),
        ],
      };
  }
}

function theme(argument: string, settings: Settings): Outcome {
  if (argument === "") {
    const next: SurfacePalette = settings.surfacePalette === "ink" ? "paper" : "ink";
    return { settings: { ...settings, surfacePalette: next }, said: [note(`theme: ${next}`)] };
  }
  if (argument !== "paper" && argument !== "ink" && argument !== "white" && argument !== "system") {
    return { said: [problem(`'${argument}' is not a theme`, ["paper, ink, white or system"])] };
  }
  return { settings: { ...settings, surfacePalette: argument }, said: [note(`theme: ${argument}`)] };
}

function layout(argument: string, settings: Settings): Outcome {
  const wanted = argument.toUpperCase();
  if (wanted !== "LR" && wanted !== "TB") {
    return {
      said: [problem(`'${argument}' is not a direction`, ["lr — left to right", "tb — top to bottom"])],
    };
  }
  return {
    settings: { ...settings, direction: wanted as Direction },
    said: [note(`layout: ${wanted}`)],
  };
}

function alias(argument: string, settings: Settings, catalogue: Catalogue): Outcome {
  if (!argument) return { said: [note(Object.entries(settings.aliases).map(([name, template]) => `${name} = ${template}`).join("\n") || 'No aliases. Define one with /alias ls = sh run cmd:"ls _"')] };
  const match = /^([A-Za-z][A-Za-z0-9-]*)[ \t]*=[ \t]*([\s\S]+)$/.exec(argument);
  if (!match) return { said: [problem('Use /alias name = command; /unalias name removes it.', [])] };
  try {
    const aliases = defineAlias(settings.aliases, match[1]!, match[2]!, catalogue);
    return { settings: { ...settings, aliases }, said: [note(`${match[1]} = ${match[2]}`)] };
  } catch (error) { return { said: [problem((error as Error).message, [])] }; }
}

/** Whether the session scrollback stays with the newest cell; independent of log-view following. */
function follow(argument: string, settings: Settings): Outcome {
  if (argument !== "" && argument !== "on" && argument !== "off") {
    return { said: [problem(`'${argument}' is not on or off`, ["'/follow' on its own switches"])] };
  }
  const next = argument === "" ? !settings.stayAtNewest : argument === "on";
  return {
    settings: { ...settings, stayAtNewest: next },
    said: [note(next
        ? "follow: on — the scrollback stays with the newest cell"
        : "follow: off — the scrollback stays where you left it")],
  };
}

/**
 * Help, general or about one thing.
 *
 * <p>{@code /help} used to take an argument and ignore it, which is the worst of the three options: a
 * person who types {@code /help list} gets the general list and no sign that they asked something else.
 *
 * <p>What it can say about an engine command comes from the catalogue and nowhere else. A second copy of
 * those sentences kept here would describe an older engine the moment either changed, and could not
 * describe an imported service at all.
 */
function helpOn(argument: string, catalogue: Catalogue | undefined): string {
  if (argument === "") {
    return help();
  }
  const asked = argument.split(/\s+/);
  const name = (asked[0] ?? "").replace(/^[:/]/, "");

  // Matched on the first word: an entry reads '/follow [on|off]', which is the name and its shape.
  const mine = OWN.find(([written]) => written.split(/\s/)[0] === `/${name}`);
  if (mine !== undefined) {
    return `${mine[0]}  ${mine[1]}`;
  }

  const meta = catalogue === undefined ? undefined : commandNamed(catalogue, name);
  if (meta !== undefined) {
    return aboutMeta(meta, asked.slice(1));
  }

  return [
    `nothing here is called '${argument}'`,
    "",
    "'/help' on its own lists the client's own commands.",
    "':help' lists the engine's, and ':help <command>' describes one.",
    "':inspect <provider> <call>' describes one call of an imported service.",
  ].join("\n");
}

function aboutMeta(meta: NonNullable<ReturnType<typeof commandNamed>>, rest: readonly string[]): string {
  const lines = [`:${meta.name}  ${meta.summary === "" ? "no summary" : meta.summary}`];
  if (!meta.implemented) {
    return `${lines[0]}\n\nWriting it today is an error, not a parse failure — the name is taken so that`
      + " implementing it later breaks nothing.";
  }
  if (meta.takes.length > 0) {
    lines.push("", `takes one of: ${meta.takes.join(", ")}`);
  }
  meta.parameters.forEach((parameter) => {
    lines.push(`${parameter.name}:  ${parameter.type}${parameter.required ? " · required" : ""}`);
  });
  if (meta.open) {
    lines.push("other arguments are passed through to whatever handles the command");
  }
  if (rest.length > 0) {
    lines.push(
      "",
      `'${rest.join(" ")}' has no help of its own — run ':${meta.name} ${rest.join(" ")}' and read it.`,
    );
  }
  return lines.join("\n");
}

/**
 * The commands and what they do, in two columns.
 *
 * <p>Written as a table rather than a sentence because for a while it was rendered as one: the newlines
 * were there and the markup collapsed them, so every command ran into the next one's description and the
 * whole thing read as a paragraph of nonsense. The message is drawn with its whitespace kept now, and
 * the columns line up so the eye can find the second one.
 */
const OWN: readonly [string, string][] = [
  ["/help", "what you are reading — '/help <name>' says more about one thing"],
  ["/alias [name = command]", "list or define a personal shortcut; standalone '_' receives arguments"],
  ["/unalias <name>", "remove a personal shortcut"],
  ["/theme [paper|ink|white|system]", "with no argument, switches"],
  ["/layout [lr|tb]", "which way the graph grows"],
  ["/follow [on|off]", "stay with the newest cell as it arrives"],
  ["/settings", "open settings; /settings limits edits startup operating budgets"],
  ["/goto [$]NAME", "find the variable's definition in this workspace; runs nothing"],
  ["/dashboard [$NAME]", "create or edit a workspace UI dashboard from existing results; never runs source commands"],
  ["/tab $BOARD · /split $BOARD", "open a saved UI dashboard; its references stay with the original workspace session"],
  ["/split [right|down] $NAME", "open the existing value in a wide pane; runs nothing"],
  ["/split right|down [workspace]", "split this workspace, or open/create another; xterm is reserved and never names a workspace"],
  ["/split [right|down] xterm [env:NAME] [target:NAME]", "open a terminal pane (right by default); keep focus here"],
  ["/tab NAME|$NAME [pane:p1]", "open a workspace or existing value tab; keep focus here"],
  ["/tab xterm [pane:pN] [env:NAME] [target:NAME]", "add a terminal tab to this terminal, pane:pN, or the last-visited terminal of this workspace; with none, open a terminal pane to the right. Keep focus and the selected tab"],
  ["/tab related $NAME", "open its inputs and consumers as related command cells"],
  ["/split [right|down] related $NAME", "show related command cells in a new pane"],
  ["/workspace delete", "preview and explicitly delete the current workspace"],
  ["/tabx NAME|$NAME [pane:p1]", "open a workspace or existing value tab and focus it"],
  ["/tabx xterm [pane:pN] [env:NAME] [target:NAME]", "add a terminal tab where /tab xterm would, then select and focus it"],
  ["/lsplit [workspace|$node|xterm|/screen]", "split left; keep focus here"],
  ["/rsplit [workspace|$node|xterm|/screen]", "split right; keep focus here"],
  ["/tsplit [workspace|$node|xterm|/screen]", "split above; keep focus here"],
  ["/bsplit [workspace|$node|xterm|/screen]", "split below; keep focus here"],
  ["/lsplitx [workspace|$node|xterm|/screen]", "split left; focus the new pane"],
  ["/rsplitx [workspace|$node|xterm|/screen]", "split right; focus the new pane"],
  ["/tsplitx [workspace|$node|xterm|/screen]", "split above; focus the new pane"],
  ["/bsplitx [workspace|$node|xterm|/screen]", "split below; focus the new pane"],
  ["/debug", "what is being kept, and where"],
  ["/clear", "clear the screen; scroll up to see earlier cells again"],
  ["/reset", "settings back to their defaults"],
];

function help(): string {
  const widest = Math.max(...OWN.map(([name]) => name.length));
  return OWN.map(([name, what]) => `${name.padEnd(widest + 2)}${what}`).join("\n");
}

/** Client messages are drawn the same way the engine's are, so there is one thing to read, not two. */
function note(message: string): Diagnostic {
  return { code: "client", severity: "info", message, start: 0, end: 0, hints: [] };
}

function problem(message: string, hints: readonly string[]): Diagnostic {
  return { code: "client", severity: "error", message, start: 0, end: 0, hints: [...hints] };
}
