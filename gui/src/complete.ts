import { calculationAt, calculationCompletion } from "./calculation-completion";
import { lastPipeEnd } from "./command-syntax";
import { collision, emptyAliases, type Aliases } from "./aliases";
import { clientCommandNames } from "./commands";
import {
  capabilityUnder,
  commandNamed,
  providerNamed,
  type Capability,
  type Catalogue,
  type MetaCommand,
  type Choices,
  type Parameter,
} from "./vocabulary";

/**
 * What to suggest for what is being typed.
 *
 * A heuristic, and named one. It reads the line the way the grammar does — annotations, then a path of
 * bare words, then `key:value` pairs — without being a parser. Duplicating the parser would mean a
 * second thing to keep in step with the language, and for a wrong guess the punishment is a wrong
 * suggestion. The engine remains the only thing that decides what a command means.
 *
 * The one rule it does honour exactly is which marker starts what, because that is what makes the
 * namespaces separate in the first place: `:` the engine's meta commands, `@` annotations, `/` the
 * client's own, `$` a result already made.
 */

/**
 * What sort of thing a suggestion is.
 *
 * <p>The named ones are wes's own and closed, because wes knows what it can suggest. The bare string
 * is for a suggestion that came from somewhere else: a completer registered for another language names
 * its own kinds — `program`, `directory`, `file` — and this client cannot have a list of them without
 * knowing every language anybody will ever register.
 */
export type Kind =
  | "client"
  | "meta"
  | "reserved"
  | "annotation"
  | "provider"
  | "capability"
  | "parameter"
  | "value"
  | "reference"
  | (string & {});

export interface Suggestion {
  readonly label?: string;
  /** What to put in place of the word being typed. */
  readonly text: string;
  readonly kind: Kind;
  /** A word or two about it, shown beside. */
  readonly detail?: string;
  /** This is a complete ordinary command word; accepting it can separate the next token. */
  readonly separate?: boolean;
}

export interface Completion {
  readonly hint?: string;
  /** Where the word being replaced starts. */
  readonly from: number;
  readonly items: readonly Suggestion[];
}

const NOTHING: Completion = { from: 0, items: [] };

/**
 * Whether a word is asking for suggestions on its own.
 *
 * A marker is a question. Typing `:` or `@` or `$` or a `key:` is someone saying which namespace they
 * are in and then pausing — there is nothing else those characters could mean, so answering at once is
 * answering what was asked.
 *
 * A bare word is not a question until there is something to go on, and three letters is where that
 * starts. Two was the first answer and it was one too few: `ls` and `cd` are whole commands somebody
 * meant to finish typing, so the list arrived exactly when nothing was being asked. A list that appears
 * before it can be useful is a list people learn to look past.
 *
 * <p>Asked of what the person typed, which is not always the whole command. Under `/focus` the console
 * holds a template, and a template ending in `cmd:"` contains a colon — so asking about the expanded
 * line opened the list before a single key was pressed. The rule is about what somebody wrote, so it is
 * measured on what they wrote.
 *
 * @param line  what the person typed
 * @param caret where the caret is in it
 * @return whether there is enough to answer
 */
const LETTERS = 3;

export function wantsSuggestions(line: string, caret: number): boolean {
  const before = line.slice(0, Math.max(0, Math.min(caret, line.length)));
  return asksForSuggestions(before.slice(wordStart(before)));
}

function asksForSuggestions(word: string): boolean {
  return (
    word.startsWith(":") ||
    word.startsWith("@") ||
    word.startsWith("$") ||
    word.startsWith("/") ||
    word.includes(":") ||
    word.length >= LETTERS
  );
}

/** The word the caret is in, which is what a suggestion would replace. */
export function wordAt(line: string, caret: number): string {
  const before = line.slice(0, caret);
  return before.slice(wordStart(before));
}

/**
 * What could go where the caret is.
 *
 * <p>Answers whenever it is called; <em>whether</em> to call it is {@link wantsSuggestions}, and the two
 * are apart because they ask about different strings — this one about the whole command, that one about
 * what somebody typed.
 */
export function complete(
  line: string,
  caret: number,
  catalogue: Catalogue,
  names: readonly string[],
  piped = false,
  aliases: Aliases = emptyAliases,
): Completion {
  const before = line.slice(0, caret);
  const pipe = lastPipeEnd(before);
  if (pipe > 0) {
    const result = complete(line.slice(pipe), caret - pipe, catalogue, names, true);
    return { ...result, from: result.from + pipe };
  }
  const calculation = calculationCompletion(before, catalogue, names, piped);
  if (calculation) return calculation;
  const from = wordStart(before);
  const word = before.slice(from);
  const words = wordsBefore(before, from);

  // Binding targets declare names; they are not references or command arguments.
  if (words.at(-1) === ">" || words.at(-1) === "*>") return NOTHING;

  if (word.startsWith("/")) {
    return offer(from, clientCommandNames().map((name) => ({ text: `/${name}`, kind: "client", separate: true })), word);
  }
  if (word.startsWith("@")) {
    return offer(
      from,
      catalogue.annotations.map((name) => ({ text: name === "trace" ? "@trace(http)" : `@${name}{`, kind: "annotation" as const })),
      word,
    );
  }
  if (word.startsWith("$")) {
    return references(from, "", word, names);
  }
  if (word.startsWith(":")) {
    return offer(from, metaCommands(catalogue, ":"), word);
  }

  const colon = word.indexOf(":");
  if (colon > 0) {
    if (piped && word.slice(colon + 1).startsWith("in") && "input".startsWith(word.slice(colon + 1))) {
      return { from, items: [{ text: `${word.slice(0, colon + 1)}input`, kind: "value", detail: "Previous pipeline stage" }] };
    }
    return values(from, word, colon, words, catalogue, names);
  }
  const ordinary = names_(from, word, words, catalogue);
  if (words.length || piped) return ordinary;
  return { ...ordinary, items: [...ordinary.items, ...Object.entries(aliases)
    .filter(([name]) => name.startsWith(word) && !collision(name, catalogue))
    .map(([name, template]) => ({ text: name, kind: "alias", detail: template, separate: true }))] };
}

/**
 * After `key:`.
 *
 * Two things can go there and they are asked for differently. A `$` means an earlier result, and any
 * argument can take one — so those are offered whatever the parameter is, and even when the command is
 * one this client has never heard of. Otherwise the only honest suggestions are the values a rule allows;
 * inventing others would be guessing at someone else's service.
 */
function values(
  from: number,
  word: string,
  colon: number,
  words: readonly string[],
  catalogue: Catalogue,
  names: readonly string[],
): Completion {
  const key = word.slice(0, colon);
  const written = word.slice(colon + 1);
  if (written.startsWith("$")) {
    return references(from, `${key}:`, written, names);
  }
  const parameter = parametersFor(words, catalogue).find((candidate) => candidate.name === key);
  if (parameter?.resourceHint) {
    const resources=parameter.resources;
    const prefix=written.replace(/^"/, "").toLocaleLowerCase();
    const age=resources ? Math.max(0, Math.floor((Date.now()-Number(BigInt(resources.observedAtNs)/1_000_000n))/1000)) : 0;
    return { from, hint: parameter.resourceHint, items: (resources?.items ?? [])
      .filter(item => item.value.toLocaleLowerCase().startsWith(prefix) || item.label.toLocaleLowerCase().includes(prefix))
      .map(item => ({ text: `${key}:${literalValue(item.value)}`, label: item.label, kind: "resource", detail: `${item.value.slice(0,12)} · ${item.detail} · observed ${age}s before this menu · ${resources!.node}` })) };
  }
  if (parameter?.choices) {
    return declared(from, key, written, parameter.type, parameter.choices);
  }
  if (parameter === undefined || parameter.allowed.length === 0) {
    return NOTHING;
  }
  const items = parameter.allowed.map((allowed) => ({
    text: `${key}:${literalValue(allowed)}`,
    kind: "value" as const,
    detail: parameter.type,
  }));
  return { from, items: items.filter((_, index) => parameter.allowed[index]?.startsWith(written.replace(/^"/, ""))) };
}

/**
 * The values a contract declares. Text is always written quoted, so a member spelled `true` or `123`
 * stays Text instead of becoming a Bool or an Int; numbers and booleans are written bare, exactly as
 * the engine spelled them. When the engine sent a preview, the hint says how much was left out even if
 * nothing in the preview matches what has been typed.
 */
function declared(from: number, key: string, written: string, type: string, choices: Choices): Completion {
  const prefix = written.replace(/^"/, "");
  const items = choices.members
    .filter((member) => member.startsWith(prefix))
    .map((member) => ({
      text: `${key}:${choices.kind === "text" ? quoted(member) : member}`,
      kind: "value" as const,
      detail: type,
    }));
  const hint = choices.total === 0
    ? "No value satisfies every rule for this parameter."
    : choices.complete ? undefined
      : `Only ${choices.members.length} of ${choices.total} declared values are listed; write any other in full.`;
  return hint === undefined ? { from, items } : { from, items, hint };
}

function quoted(value: string): string {
  return `"${value.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
}

function literalValue(value: string): string {
  return value === "" || /[\s>|{}"\\]/.test(value) || value.startsWith("$")
    ? `"${value.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"` : value;
}

function references(from: number, prefix: string, written: string, names: readonly string[]): Completion {
  const separator = written.indexOf("::");
  if (separator >= 0) {
    const source = written.slice(0, separator);
    const descriptions = { data: "Successful result", error: "Failure details", cancel: "Local cancellation or timeout" };
    return offer(from, Object.entries(descriptions).map(([port, detail]) => ({
      text: `${prefix}${source}::${port}`, kind: "reference" as const, detail,
    })), `${prefix}${written}`);
  }
  return offer(from, names.map((name) => ({ text: `${prefix}$${name}`, kind: "reference" as const })), `${prefix}${written}`);
}

/** A bare word: what it can be depends on how much of the command is already there. */
function names_(
  from: number,
  word: string,
  words: readonly string[],
  catalogue: Catalogue,
): Completion {
  const command = withoutAnnotations(words);
  if (command.length === 0) {
    return offer(
      from,
      [
        ...catalogue.providers.map((provider) => ({
          text: provider.name,
          kind: "provider" as const,
          separate: true,
          detail: `${provider.capabilities.length} capabilities`,
        })),
        ...(catalogue.templates ?? []).map(template => ({ text: template.name, kind: "template" as const, detail: template.body, separate: true })),
        ...metaCommands(catalogue, ""),
      ],
      word,
    );
  }

  const head = command[0] ?? "";
  if (head === ":help" || head === ":inspect") {
    const tail = command.slice(1);
    if (tail.length === 0) {
      return offer(from, [
        ...metaCommands(catalogue, ""),
        ...catalogue.providers.map(provider => ({text:provider.name,kind:"provider" as const,separate:true})),
        ...parameterItems(commandNamed(catalogue, head.slice(1))?.parameters ?? [], words),
      ], word);
    }
    const target = tail[0]!;
    const family = commandNamed(catalogue,target);
    const provider = providerNamed(catalogue,target);
    if (family && provider) {
      return {from, items:[], hint:"Ambiguous target. Use command:, provider: or capability:."};
    }
    if (family) return offer(from, tail.length === 1 ? family.takes.map(text => ({text,kind:"meta" as const,separate:true})) : [], word);
    if (provider) {
      const prefix = tail.slice(1);
      return offer(from, unique(provider.capabilities.filter(c => prefix.every((part,i) => c.path[i] === part) && c.path.length > prefix.length)
        .map(c => ({text:c.path[prefix.length]!,kind:"capability" as const,separate:true,detail:c.summary}))),word);
    }
    return NOTHING;
  }
  const template = catalogue.templates?.find(candidate => candidate.name === head);
  if (template !== undefined && providerNamed(catalogue, head) === undefined)
    return offer(from, parameterItems(template.parameters, words), word);
  const provider = providerNamed(catalogue, head);
  if (provider !== undefined) {
    const tail = command.slice(1);
    const capability = capabilityUnder(provider, tail);
    if (capability === undefined) {
      const segments = provider.capabilities
        .filter((candidate) => candidate.path.length > tail.length)
        .filter((candidate) => tail.every((segment, index) => candidate.path[index] === segment))
        .map((candidate) => ({
          text: candidate.path[tail.length] ?? "",
          kind: "capability" as const,
          separate: true,
          detail: candidate.summary,
        }));
      return offer(from, unique(segments), word);
    }
    return offer(from, parameterItems(capability.parameters, words), word);
  }

  const meta = commandNamed(catalogue, head.replace(/^:/, ""));
  if (meta !== undefined) {
    /*
     * The words it takes come first and only while none has been written. ':list' takes exactly one of
     * seven, and until the vocabulary carried them the client had nothing to offer after ':list ' but
     * 'provider:' — the argument, never the thing being listed.
     */
    const level = commandLevel(meta, command.slice(1));
    const offers = [
      ...(level.choosing ? level.takes.map((takes) => ({ text: takes, kind: "value" as const, separate: true })) : []),
      ...parameterItems(level.parameters, words),
    ];
    return offer(from, offers, word);
  }
  return NOTHING;
}

/** Arguments already written are not suggested again — a repeated key is an error, not a completion. */
function parameterItems(
  parameters: readonly Parameter[],
  words: readonly string[],
): Suggestion[] {
  const written = new Set(
    words.filter((word) => word.includes(":")).map((word) => word.slice(0, word.indexOf(":"))),
  );
  return parameters
    .filter((parameter) => !written.has(parameter.name))
    .map((parameter) => ({
      text: `${parameter.name}:`,
      kind: "parameter" as const,
      detail: `${parameter.type}${parameter.required ? " · required" : ""}`,
    }));
}

/**
 * Reserved names are offered, marked. Hiding them would let someone type a word that looks available
 * and resolves to nothing. Marking it reserved explains why it is unavailable.
 */
function metaCommands(catalogue: Catalogue, prefix: string): Suggestion[] {
  return catalogue.commands.map((command: MetaCommand) => ({
    text: `${prefix}${command.name}`,
    kind: command.implemented ? ("meta" as const) : ("reserved" as const),
    separate: true,
    detail: command.implemented ? undefined : "reserved, not implemented yet",
  }));
}

/**
 * Where the caret is, when it is inside a value written in a language of its own.
 *
 * <p>`sh run cmd:"cat /usr/lo"` is one argument at the type level and a shell line inside it. Nothing
 * here can complete that — the names are files and programs on the machine the engine runs on, and a
 * directory changes while you type in it — so this only says *that* it is foreign, and what the inner
 * text and caret are. Asking is somebody else's job.
 *
 * <p>Only a quoted value counts. An unquoted one ends at the first space, which is not a shell line.
 *
 * @param line      the whole command
 * @param caret     where the caret is in it
 * @param catalogue what the engine said its parameters are
 * @return what to ask about, or undefined when the caret is in wes's own language
 */
export function foreignAt(
  line: string,
  caret: number,
  catalogue: Catalogue,
): Foreign | undefined {
  const pipe = lastPipeEnd(line.slice(0, caret));
  if (pipe > 0) {
    const result = foreignAt(line.slice(pipe), caret - pipe, catalogue);
    return result && { ...result, from: result.from + pipe };
  }
  if (calculationAt(line.slice(0, caret))) return undefined;
  const before = line.slice(0, Math.max(0, Math.min(caret, line.length)));
  const from = wordStart(before);
  const word = before.slice(from);
  const quote = word.indexOf('"');
  if (quote < 0) {
    return undefined;
  }
  const colon = word.indexOf(":");
  if (colon < 0 || colon > quote) {
    return undefined;
  }
  const key = word.slice(0, colon);
  const parameter = parametersFor(wordsBefore(before, from), catalogue)
    .find((candidate) => candidate.name === key);
  if (parameter === undefined || parameter.content === "") {
    return undefined;
  }
  const opens = from + quote + 1;
  const raw = before.slice(opens);
  let written = "";
  const offsets = [0];
  for (let i = 0; i < raw.length; i++) {
    let ch = raw[i]!;
    if (ch === '"') return undefined;
    if (ch === "\\") {
      ch = raw[++i]!;
      // Match source-language escapes; unfinished/unknown escapes have no decoded offset.
      if (ch === "n") ch = "\n";
      else if (ch === "r") ch = "\r";
      else if (ch === "t") ch = "\t";
      else if (ch !== '"' && ch !== "\\") return undefined;
    }
    written += ch;
    offsets.push(i + 1);
  }
  return { language: parameter.content, written, caret: written.length, from: opens, offsets };
}

/** A value written in a language of its own, and where it sits in the command. */
export interface Foreign {
  /** What the parameter said it is written in. */
  readonly language: string;
  /** The value so far — what to ask about. */
  readonly written: string;
  /** Where the caret is within that value. */
  readonly caret: number;
  /** Where that value starts in the whole command, for putting an answer back. */
  readonly from: number;
  /** Decoded UTF-16 boundaries mapped back to source offsets inside the outer quotes. */
  readonly offsets: readonly number[];
}

/** Encode an inner-language answer back into the surrounding wes quoted value. */
export function foreignCompletion(foreign: Foreign, answer: Completion): Completion {
  const offset = foreign.offsets[answer.from];
  if (!Number.isInteger(answer.from) || offset === undefined) return NOTHING;
  const before = foreign.written.charCodeAt(answer.from - 1);
  const after = foreign.written.charCodeAt(answer.from);
  if (before >= 0xd800 && before <= 0xdbff && after >= 0xdc00 && after <= 0xdfff) return NOTHING;
  return {
    from: foreign.from + offset,
    items: answer.items.map(item => ({ ...item, text: item.text.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n").replace(/\r/g, "\\r").replace(/\t/g, "\\t") })),
  };
}

function parametersFor(words: readonly string[], catalogue: Catalogue): readonly Parameter[] {
  const command = withoutAnnotations(words);
  const head = command[0] ?? "";
  const provider = providerNamed(catalogue, head);
  if (provider !== undefined) {
    const capability: Capability | undefined = capabilityUnder(provider, command.slice(1));
    return capability?.parameters ?? [];
  }
  const template = catalogue.templates?.find(candidate => candidate.name === head);
  if (template !== undefined) return template.parameters;
  const meta = commandNamed(catalogue, head.replace(/^:/, ""));
  return meta ? commandLevel(meta, command.slice(1)).parameters : [];
}

/**
 * Where a meta command's path has reached. Subcommand words are read in order through nested
 * variants; written arguments (`key:value`, quoted values included) are passed over, and the first
 * other word that is not a listed subcommand ends the path. `choosing`: every word so far was a
 * subcommand, so the next one may still come.
 */
function commandLevel(meta: MetaCommand, tail: readonly string[]): { readonly takes: readonly string[]; readonly parameters: readonly Parameter[]; readonly choosing: boolean } {
  let takes = meta.takes, variants = meta.variants, parameters = meta.parameters;
  const bare = tail.filter(word => !ARGUMENT.test(word));
  let at = 0;
  for (; at < bare.length && takes.includes(bare[at]!); at++) {
    const variant = variants.find(candidate => candidate.word === bare[at]);
    // A listed word without its own signature (':list providers') keeps the command's parameters.
    if (variant === undefined) return { takes: [], parameters, choosing: false };
    ({ takes, variants, parameters } = variant);
  }
  return { takes, parameters, choosing: at === bare.length };
}
const ARGUMENT = /^[^\s:"$]+:/;

function withoutAnnotations(words: readonly string[]): readonly string[] {
  return words.filter((word) => !word.startsWith("@"));
}

function offer(from: number, items: readonly Suggestion[], word: string): Completion {
  const wanted = word.toLowerCase();
  const matching = items.filter((item) => item.text.toLowerCase().startsWith(wanted));
  return matching.length === 0 ? NOTHING : { from, items: matching };
}

function unique(items: readonly Suggestion[]): Suggestion[] {
  const seen = new Set<string>();
  return items.filter((item) => (seen.has(item.text) ? false : (seen.add(item.text), true)));
}

/** Words are separated by spaces; a quoted value is one word, so a space inside quotes does not split. */
function wordStart(before: string): number {
  let start = 0;
  let quoted = false;
  for (let index = 0; index < before.length; index++) {
    const character = before[index];
    if (character === "\\" && quoted) {
      index++;
    } else if (character === '"') {
      quoted = !quoted;
    } else if (/\s/.test(character ?? "") && !quoted) {
      start = index + 1;
    }
  }
  return start;
}

function wordsBefore(before: string, from: number): readonly string[] {
  const words: string[] = [];
  let start = 0;
  let quoted = false;
  for (let index = 0; index < from; index++) {
    const character = before[index];
    if (character === "\\" && quoted) index++;
    else if (character === '"') quoted = !quoted;
    else if (/\s/.test(character ?? "") && !quoted) {
      if (index > start) words.push(before.slice(start, index));
      start = index + 1;
    }
  }
  if (start < from) words.push(before.slice(start, from));
  return words;
}
