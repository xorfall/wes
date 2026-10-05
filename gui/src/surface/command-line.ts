/**
 * A command line, coloured by what each word is.
 *
 * One colour vocabulary runs through the whole surface, so a provider call is the same green in the
 * scrollback, in the prompt and in the editor. This is the line-level half of it: the shape of a
 * command — a meta word, a provider and its capability, named parameters, literals, references, a
 * redirect. The `:calc` program inside a command is the editor's half, and it is generated from the
 * language package the engine serves rather than written here.
 *
 * Display only. Nothing here decides whether a command is valid — the engine does that, and saying
 * so in colour before it has would be a guess dressed as a fact.
 */
import { highlightCalc } from "./calc-highlight";
import { bundledPackage, readLanguage, type Language } from "./language";
import type { MonoRole, Segment } from "./MonoLine";

const defaultLanguage = readLanguage(bundledPackage, "bundled");

/** Words that name something the client does rather than something a provider does. */
const META = /^(:calc|:import|:iter|:type|:use|:set|:keep|:trace)$/;

/**
 * Splits on whitespace but keeps quoted runs whole, so `status:"open with space"` stays one word.
 * Returns each word with the whitespace that preceded it, so the line reads back exactly.
 */
function words(text: string): { gap: string; word: string }[] {
  const out: { gap: string; word: string }[] = [];
  let gap = "";
  let word = "";
  let quote: string | undefined;
  const flush = () => {
    // Two spaces in a row are one gap; clearing it on an empty word would forget the first.
    if (word === "") return;
    out.push({ gap, word });
    gap = "";
    word = "";
  };
  for (const character of text) {
    if (quote) {
      word += character;
      if (character === quote) quote = undefined;
      continue;
    }
    if (character === '"' || character === "'") {
      quote = character;
      word += character;
      continue;
    }
    if (/\s/u.test(character)) {
      flush();
      gap += character;
      continue;
    }
    word += character;
  }
  flush();
  if (gap !== "") out.push({ gap, word: "" });
  return out;
}

function literal(text: string): boolean {
  return /^["']/.test(text) || /^-?\d/.test(text) || text === "true" || text === "false" || text === "none";
}

/** A parameter is `name:` and then its value, and the colon belongs to the name. */
function parameter(word: string): Segment[] | undefined {
  const at = word.indexOf(":");
  if (at <= 0 || word.startsWith(":")) return undefined;
  const name = word.slice(0, at + 1);
  const value = word.slice(at + 1);
  if (!/^[A-Za-z_][\w.-]*:$/.test(name)) return undefined;
  if (value === "") return [{ text: name, role: "mono-param" }];
  return [
    { text: name, role: "mono-param" },
    { text: value, role: literal(value) ? "mono-literal" : "mono-ink" },
  ];
}

/**
 * The command as roled runs.
 *
 * The first word decides the line: a meta word makes it the client's, anything else makes it a
 * provider call, whose next word is the capability and is coloured with it.
 */
export function commandSegments(text: string, language: Language = defaultLanguage): Segment[] {
  if (/^\s*(?:@\S+\s+)*:calc(?=\s|\{|$)/u.test(text)) {
    return highlightCalc(text, language).spans.map(span => ({
      text: text.slice(span.from, span.to),
      ...(span.role ? { role: span.role } : {}),
    }));
  }
  const out: Segment[] = [];
  const parts = words(text);
  let position = 0;
  let comment = false;
  for (const { gap, word } of parts) {
    if (gap.includes("\n")) comment = false;
    if (word.startsWith("//")) comment = true;
    if (gap !== "") out.push({ text: gap });
    if (word === "") continue;
    out.push(...(comment ? [{ text: word, role: "mono-faint" as const }] : runsFor(word, position, parts)));
    position += 1;
  }
  return out;
}

function runsFor(word: string, position: number, parts: readonly { word: string }[]): Segment[] {
  if (word.startsWith("$")) return [{ text: word, role: "mono-ref" }];
  if (word === ">") return [{ text: word, role: "mono-dim" }];
  if (META.test(word)) return [{ text: word, role: "mono-meta" }];
  if (word.startsWith(":")) return [{ text: word, role: "mono-meta" }];
  if (word.startsWith("@")) return [{ text: word, role: "mono-meta" }];
  const named = parameter(word);
  if (named) return named;
  // A provider and its capability are one call and take one colour; a redirect target is a name.
  const after = parts.filter((part) => part.word !== "");
  const previous = after[position - 1]?.word;
  if (previous === ">") return [{ text: word, role: "mono-ref" }];
  if (position <= 1 && !META.test(after[0]?.word ?? "")) return [{ text: word, role: "mono-provider" }];
  if (literal(word)) return [{ text: word, role: "mono-literal" }];
  return [{ text: word, role: "mono-ink" }];
}

/** The scrollback's command line: when it ran, the caret, and the command. */
export function commandLine(text: string, at?: Date, language?: Language): Segment[] {
  const time = at ? `${pad(at.getHours())}:${pad(at.getMinutes())}  ` : "";
  const lead: Segment[] = time === "" ? [] : [{ text: time, role: "mono-faint" as MonoRole }];
  return [...lead, { text: "❯ ", role: "mono-ref-strong" }, ...commandSegments(text, language)];
}

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

/** Separate display metadata from source without inserting indentation into either one. */
export function splitCommandLine(segments: readonly Segment[]): { prefix: readonly Segment[]; source: readonly Segment[] } {
  const caret = segments.findIndex((segment, index) => index < 2
    && segment.text === "❯ " && segment.role === "mono-ref-strong");
  if (caret < 0) return { prefix: [], source: segments };
  return { prefix: segments.slice(0, caret + 1), source: segments.slice(caret + 1) };
}
