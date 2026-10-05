/**
 * What the plain-text prompt is allowed to suggest.
 *
 * The grown editor completes a `:calc` program from the engine's language package; this completes a
 * command, which is a different language with different sources — the engine's meta commands, the
 * providers and capabilities the vocabulary announced, the parameters each one takes, the `$names`
 * this workspace holds and the client's own `/` commands. All of them come from `complete.ts`,
 * which is where the client keeps the shared vocabulary reading.
 *
 * The drawing matches the editor's list, because it is the same list in a different place.
 */
import { complete, wantsSuggestions, type Completion, type Kind, type Suggestion } from "../complete";
import type { Aliases } from "../aliases";
import type { Catalogue } from "../vocabulary";
import type { Segment } from "./MonoLine";

export interface PromptCompletionInput {
  readonly line: string;
  readonly caret: number;
  readonly catalogue: Catalogue;
  /** Every way a result can be referred to: the names given and the ids they always had. */
  readonly names: readonly string[];
  readonly variables?: readonly string[];
  readonly dashboards?:readonly string[];
  readonly aliases: Aliases;
  readonly workspaces?: readonly string[];
}

/** Visible rows only; matching candidates remain available for navigation. */
export const VISIBLE_ROWS = 8;
/** The destination word that opens a terminal after /tab, /tabx and every split command. */
const TERMINAL = "xterm";

/** Whether there is a question yet. The rule is `complete.ts`'s, asked of what somebody typed. */
export function asks(line: string, caret: number): boolean {
  return /^\/(?:dashboard|goto|tabx?|[lrtb]splitx?|split)(?:[ \t]+(?:right|down))?[ \t]+[^\n]*$/.test(line.slice(0, caret)) || wantsSuggestions(line, caret);
}

export function promptCompletion(input: PromptCompletionInput): Completion {
  const head = input.line.slice(0, input.caret);
  const goto = /^\/goto[ \t]+([^\s]*)$/.exec(head);
  const references = (written: string): Suggestion[] => [...new Set((input.variables ?? input.names).map(name => name.replace(/^\$/, "")))]
    .filter(name => name.startsWith(written.replace(/^\$/, ""))).sort()
    .map(name => ({ text: `$${name}`, kind: "reference", detail: "existing value in this workspace", separate: true }));
  if (goto) return { from: head.length - goto[1]!.length, items: references(goto[1]!) };
  const boards=(written:string):Suggestion[]=>[...new Set(input.dashboards??[])].filter(name=>name.startsWith(written.replace(/^\$/,''))).sort().map(name=>({text:`$${name}`,kind:'reference',detail:'saved dashboard layout',separate:true}));
  const dashboard=/^\/dashboard[ \t]+([^\s]*)$/.exec(head);
  if(dashboard)return {from:head.length-dashboard[1]!.length,items:boards(dashboard[1]!)};
  const destination = /^\/(tabx?|[lrtb]splitx?|split)(?:[ \t]+(right|down))?[ \t]+(?:(related)[ \t]+)?("(?:\\.|[^"\\])*"?|[^\s]*)$/.exec(head);
  if (destination) {
    const written = destination[4]!;
    if (destination[3]) return { from: head.length - written.length, items: written === "" || written.startsWith("$") ? references(written) : [] };
    if (written.startsWith("$")) return { from: head.length - written.length, items: [...references(written),...boards(written)] };
    let prefix = written.replace(/^"/, "").replace(/"$/, "");
    if (written.startsWith('"')) { try { prefix = JSON.parse(written.endsWith('"') ? written : `${written}"`) as string; } catch { /* incomplete escape */ } }
    const quote = (name: string) => name === "related" || /\s|"/.test(name) ? JSON.stringify(name) : name;
    // `xterm` is reserved for a terminal, so a saved workspace of that exact name is never offered.
    const names = [...new Set(input.workspaces ?? [])].filter(name => name !== TERMINAL && name.startsWith(prefix)).sort();
    const directions = destination[1] === "split" && !destination[2] ? ["right", "down"].filter(name => name.startsWith(prefix)) : [];
    return { from: head.length - written.length, items: [
      ...directions.map(text => ({ text, kind: "client" as const, detail: "split direction", separate: true })),
      ...("related".startsWith(prefix) && !written.startsWith('"') ? [{ text: "related", kind: "client" as const, detail: "inputs and consumers of $variable", separate: true }] : []),
      ...(TERMINAL.startsWith(prefix) ? [{ text: TERMINAL, kind: "client" as const, detail: "terminal", separate: true }] : []),
      ...names.map(name => ({ text: `${destination[1] === "split" && !destination[2] ? "right " : ""}${quote(name)}`, label: name, kind: "client" as const, detail: "workspace", separate: true })),
      ...(written === "" ? [...references(""),...boards('')] : []),
    ] };
  }
  if (/^\/[^\s]*$/.test(head)) {
    const commands = ["dashboard","goto", "spec", "graph", "stale", "env", "settings", "open", "edit", "alias", "unalias", "theme", "layout", "follow", "split", "close", "clear", "debug",
      "workspace", "tab", "tabx", "lsplit", "rsplit", "tsplit", "bsplit", "lsplitx", "rsplitx", "tsplitx", "bsplitx"];
    return { from: 0, items: commands.filter(name => `/${name}`.startsWith(head))
      .map(name => ({ text: `/${name}`, kind: "client", separate: true })) };
  }
  const deletion = /^\/workspace[ \t]+([^\s]*)$/.exec(head);
  if (deletion) return { from: head.length - deletion[1]!.length, items: "delete".startsWith(deletion[1]!) ? [{text:"delete",kind:"client",detail:"open deletion preview",separate:true}] : [] };
  const removal = /^\/unalias[ \t]+([^\s]*)$/.exec(head);
  if (removal) {
    return { from: head.length - removal[1]!.length, items: Object.keys(input.aliases)
      .filter(name => name.startsWith(removal[1]!))
      .map(name => ({ text: name, kind: "alias", detail: input.aliases[name], separate: true })) };
  }
  const found = complete(input.line, input.caret, input.catalogue, input.names, false, input.aliases);
  return { hint: found.hint, from: found.from, items: found.items };
}

/**
 * The colour a suggestion is offered in: the one the command line would draw it in once accepted.
 *
 * A kind this client does not know — a completer registered for another language names its own —
 * takes the plain ink role rather than being left uncoloured.
 */
export function roleOf(kind: Kind): Segment["role"] {
  switch (kind) {
    case "client":
    case "meta":
    case "annotation": return "mono-meta";
    case "reserved": return "mono-warn";
    case "provider": return "mono-provider";
    case "parameter": return "mono-param";
    case "value": return "mono-literal";
    case "reference": return "mono-ref";
    default: return "mono-ink";
  }
}

/** One row: the chosen one leads with a caret, the rest with two spaces. The editor's own shape. */
export function suggestionLine(suggestion: Suggestion, chosen: boolean, width = 22): Segment[] {
  // Always at least one space before the detail, so a long name does not run into it.
  const visible = suggestion.label ?? suggestion.text;
  const label = visible + " ".repeat(Math.max(1, width - visible.length));
  const line: Segment[] = [
    { text: chosen ? "› " : "  ", role: "mono-ref" },
    { text: label, role: chosen ? roleOf(suggestion.kind) : "mono-dim" },
  ];
  if (suggestion.detail) {
    line.push({ text: suggestion.kind === "alias" ? "· personal alias · " : "· ", role: "mono-faint" }, { text: suggestion.detail, role: "mono-faint" });
  }
  return line;
}

/** Where the list says its suggestions came from, so nobody expects the engine to know more. */
export function suggestionSource(kinds: readonly Kind[] = []): Segment[] {
  const personal = kinds.includes("alias");
  const client = kinds.includes("client");
  const engine = kinds.some(kind => kind !== "alias" && kind !== "client");
  const text = personal && engine ? "from this workspace and your personal aliases"
    : personal ? "from your personal aliases"
    : client && engine ? "from this workspace and the client's commands"
    : client && !engine ? "from the client's commands"
    : "from what the engine said this workspace can do";
  return [{ text, role: "mono-faint" }];
}

/** What the line holds once a suggestion is taken, and where the caret goes. Kept out of the view. */
export type { Suggestion };
