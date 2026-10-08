/**
 * The `:calc` highlighter, generated from the language package the engine serves.
 *
 * The UI does not need a separate operation list when the language gains an operation. Every keyword, every
 * operator and every operation name here comes from the package — so a word is coloured as a
 * keyword because the engine's grammar says it is one, and never because JavaScript has a word that
 * looks like it.
 *
 * That cuts both ways, and the cutting is the point. `switch`, `class`, `new`, `typeof`, `async`,
 * `null`, `undefined` and `NaN` are ordinary identifiers in this language; colouring them as
 * keywords would be a lie about what the engine accepts, and a lie that only shows up when the
 * command is run. `===`, `!==`, `%`, `**`, `??`, `?.`, `++`, `+=` and the ternary are not operators
 * here; template literals and regex literals are not literals here. Each is marked as a mistake
 * where it stands, with a hint that names what the language does have.
 */
import type { Language } from "./language";
import type { MonoRole, Segment } from "./MonoLine";
import type { Mistake, Span } from "./source-mode";

export type { Mistake, Span };
export { mistakeRows } from "./source-mode";

export interface Highlighted {
  /** One array of runs per line of the source, whitespace and all. */
  readonly lines: readonly (readonly Segment[])[];
  readonly mistakes: readonly Mistake[];
  /**
   * The same runs again, as offsets into the source rather than as lines.
   *
   * An editor decorates ranges of a document; a drawn block draws rows. Both are this one reading
   * of the source, said twice — never tokenised twice, because two tokenisers eventually disagree
   * and the disagreement is a word that is one colour in the prompt and another in the editor.
   */
  readonly spans: readonly Span[];
}

/** What the language has instead of each thing JavaScript would offer. */
const INSTEAD: Record<string, { readonly said: string; readonly hint: readonly Segment[] }> = {
  "===": { said: "no === in this language", hint: said("equality is ", { text: "==", role: "mono-literal" }) },
  "!==": {
    said: "no !== in this language",
    hint: [
      { text: "inequality is ", role: "mono-dim" },
      { text: "!=", role: "mono-literal" },
      { text: ", and absence is ", role: "mono-dim" },
      { text: "none", role: "mono-meta" },
      { text: " — read it with ", role: "mono-dim" },
      { text: "isSome()", role: "mono-provider" },
    ],
  },
  "%": { said: "no % in this language", hint: said("the remainder is ", { text: "rem(a, b)", role: "mono-provider" }) },
  "**": { said: "no ** in this language", hint: said("there is no exponent operator", { text: "", role: "mono-dim" }) },
  "??": { said: "no ?? in this language", hint: said("absence is read with ", { text: "unwrapOr()", role: "mono-provider" }) },
  "?.": { said: "no ?. in this language", hint: said("absence is read with ", { text: "isSome()", role: "mono-provider" }) },
  "++": { said: "no ++ in this language", hint: said("write ", { text: "x = x + 1", role: "mono-literal" }) },
  "+=": { said: "no += in this language", hint: said("write ", { text: "x = x + 1", role: "mono-literal" }) },
  "-=": { said: "no -= in this language", hint: said("write ", { text: "x = x - 1", role: "mono-literal" }) },
  "--": { said: "no -- in this language", hint: said("write ", { text: "x = x - 1", role: "mono-literal" }) },
  "&": { said: "no bitwise operators in this language", hint: said("there are no bitwise operators", { text: "", role: "mono-dim" }) },
  "|": { said: "no bitwise operators in this language", hint: said("there are no bitwise operators", { text: "", role: "mono-dim" }) },
  "^": { said: "no bitwise operators in this language", hint: said("there are no bitwise operators", { text: "", role: "mono-dim" }) },
  "~": { said: "no bitwise operators in this language", hint: said("there are no bitwise operators", { text: "", role: "mono-dim" }) },
  ">>": { said: "no bitwise operators in this language", hint: said("there are no bitwise operators", { text: "", role: "mono-dim" }) },
  "<<": { said: "no bitwise operators in this language", hint: said("there are no bitwise operators", { text: "", role: "mono-dim" }) },
  "?": { said: "no ternary in this language", hint: said("write an ", { text: "if", role: "mono-meta" }) },
};

function said(lead: string, what: Segment): Segment[] {
  return what.text === "" ? [{ text: lead, role: "mono-dim" }] : [{ text: lead, role: "mono-dim" }, what];
}

/** Characters an operator can be made of, so a run of them can be taken whole and then judged. */
const OPERATOR_CHARACTERS = "+-*/!=<>|&%^~?";
/** Punctuation that is structure rather than meaning. */
const PUNCTUATION = "(){}[],;.:";

interface Token {
  readonly text: string;
  readonly from: number;
  role: MonoRole | undefined;
  mistake?: { readonly said: string; readonly hint: readonly Segment[] };
}

/**
 * The highlighter.
 *
 * Reads the source once into runs, decides each run's role from the package, then walks the runs
 * again for the two things a single run cannot know: which identifiers are a lambda's parameters,
 * and whether an operation was given the number of arguments its signature allows.
 */
export function highlightCalc(source: string, language: Language): Highlighted {
  const tokens = read(source, language);
  markParameters(tokens);
  const mistakes = [
    ...tokens.flatMap((token) => (token.mistake ? [asMistake(token, source, token.mistake)] : [])),
    ...arityMistakes(tokens, source, language),
  ].sort((a, b) => a.from - b.from);
  return { lines: intoLines(tokens, source), mistakes, spans: spansOf(tokens) };
}

/** The tokens as ranges. Whitespace keeps its place so a span always abuts the next one. */
function spansOf(tokens: readonly Token[]): Span[] {
  return tokens.map((token) => ({
    from: token.from,
    to: token.from + token.text.length,
    ...(token.role === undefined ? {} : { role: token.role }),
  }));
}

function read(source: string, language: Language): Token[] {
  const tokens: Token[] = [];
  const keywords = new Set(language.keywords());
  const operations = new Set(language.operations());
  const operators = new Set(language.operators());
  let at = 0;
  /** Brace depth, so a `>` outside the program is a redirect and inside it is greater-than. */
  let depth = 0;
  let redirecting = false;
  const push = (text: string, role: MonoRole | undefined, mistake?: Token["mistake"]) => {
    tokens.push({ text, from: at, role, mistake });
    at += text.length;
  };

  while (at < source.length) {
    const rest = source.slice(at);
    const character = rest[0]!;

    const space = /^[ \t\r\n]+/.exec(rest);
    if (space) {
      push(space[0], undefined);
      continue;
    }
    if (rest.startsWith("//")) {
      const line = /^\/\/[^\n]*/.exec(rest)!;
      push(line[0], "mono-faint");
      continue;
    }
    if (rest.startsWith("/*")) {
      const closed = rest.indexOf("*/", 2);
      if (closed < 0) {
        push(rest, "mono-bad", {
          said: "this block comment is never closed",
          hint: said("close it with ", { text: "*/", role: "mono-literal" }),
        });
        continue;
      }
      push(rest.slice(0, closed + 2), "mono-faint");
      continue;
    }
    if (character === "`") {
      const end = rest.indexOf("`", 1);
      push(end < 0 ? rest : rest.slice(0, end + 1), "mono-bad", {
        said: "no template literals in this language",
        hint: said("strings are quoted, and only quoted: ", { text: '"like this"', role: "mono-literal" }),
      });
      continue;
    }
    if (character === '"' || character === "'") {
      const string = new RegExp(`^${character}(?:[^${character}\\\\\\n]|\\\\.)*${character}?`).exec(rest)!;
      const closed = string[0].length > 1 && string[0].endsWith(character);
      push(string[0], closed ? "mono-literal" : "mono-bad", closed ? undefined : {
        said: "this string is never closed",
        hint: said("close it with ", { text: character, role: "mono-literal" }),
      });
      continue;
    }
    const number = /^\d+(?:\.\d+)?/.exec(rest);
    if (number) {
      push(number[0], "mono-literal");
      continue;
    }
    if (rest.startsWith("?.")) {
      push("?.", "mono-bad", INSTEAD["?."]);
      continue;
    }
    // A command's own words wrap the program: `:calc { … } > name`. They are not the language's.
    const meta = /^:[\p{L}_][\p{L}\p{N}_.-]*/u.exec(rest);
    if (meta) {
      push(meta[0], "mono-meta");
      continue;
    }
    if (character === "$") {
      const reference = /^\$[\p{L}_][\p{L}\p{N}_]*/u.exec(rest);
      push(reference ? reference[0] : "$", "mono-ref");
      continue;
    }
    const identifier = /^[\p{L}_][\p{L}\p{N}_]*/u.exec(rest);
    if (identifier) {
      const word = identifier[0];
      const previous = lastMeaningful(tokens);
      const property = previous?.text === ".";
      if (redirecting) {
        redirecting = false;
        push(word, "mono-ref");
      } else if (property) {
        // After a dot only a declared method is one; `stripAnsi(raw).text` reads a field, not `text`.
        push(word, language.method(word) || operations.has(namespaced(tokens, word)) ? "mono-provider" : "mono-param");
      }
      else if (word === "pure" && previous?.text === ":calc") push(word, "mono-meta");
      else if (keywords.has(word)) push(word, "mono-meta");
      else if (word === "none" || word === "true" || word === "false") push(word, "mono-meta");
      else if (operations.has(word)) push(word, "mono-provider");
      else push(word, "mono-ink");
      continue;
    }
    if (OPERATOR_CHARACTERS.includes(character)) {
      const run = /^[+\-*/!=<>|&%^~?]+/.exec(rest)![0];
      if (run === "->" && depth === 0) { push(run, "mono-dim"); continue; }
      // Outside the program a lone `>` sends the result somewhere; inside it, it compares.
      if (run === ">" && depth === 0) {
        redirecting = true;
        push(">", "mono-dim");
        continue;
      }
      pushOperator(run, operators, push);
      continue;
    }
    if (PUNCTUATION.includes(character)) {
      if (character === "{") depth += 1;
      if (character === "}") depth = Math.max(0, depth - 1);
      push(character, "mono-faint");
      continue;
    }
    push(character, "mono-ink");
  }
  return tokens;
}

/**
 * A run of operator characters, taken apart into the operators the package has.
 *
 * Longest match first, so `!=` is never read as `!` and `=`. What is left over is a mistake, and
 * the hint names what the language has in its place.
 */
function pushOperator(
  run: string,
  operators: ReadonlySet<string>,
  push: (text: string, role: MonoRole | undefined, mistake?: Token["mistake"]) => void,
): void {
  let rest = run;
  while (rest.length > 0) {
    const known = [3, 2, 1]
      .map((length) => rest.slice(0, length))
      .find((candidate) => operators.has(candidate) || candidate === "=>" || candidate === "=");
    const wrong = [3, 2].map((length) => rest.slice(0, length)).find((candidate) => candidate in INSTEAD);
    if (wrong && (!known || wrong.length >= known.length)) {
      push(wrong, "mono-bad", INSTEAD[wrong]);
      rest = rest.slice(wrong.length);
      continue;
    }
    if (known) {
      push(known, "mono-faint");
      rest = rest.slice(known.length);
      continue;
    }
    const one = rest[0]!;
    if (one in INSTEAD) push(one, "mono-bad", INSTEAD[one]);
    else push(one, "mono-faint");
    rest = rest.slice(1);
  }
}

/** `iter.lines` is one operation name with a dot in it, so the dot before it does not make a field. */
function namespaced(tokens: readonly Token[], word: string): string {
  const dot = lastMeaningful(tokens);
  const before = lastMeaningful(tokens, 1);
  return dot?.text === "." && before ? `${before.text}.${word}` : word;
}

function lastMeaningful(tokens: readonly Token[], skip = 0): Token | undefined {
  let seen = 0;
  for (let at = tokens.length - 1; at >= 0; at -= 1) {
    const token = tokens[at]!;
    if (token.role === undefined || token.role === "mono-faint" && token.text.trim() === "") continue;
    if (seen === skip) return token;
    seen += 1;
  }
  return undefined;
}

/**
 * A lambda's parameters are scaffolding, not names the program introduced, so they read as quietly
 * as the arrow and the brackets around them. `o => …` and `(t, o) => …` both count.
 */
function markParameters(tokens: Token[]): void {
  const meaningful = tokens.filter((token) => token.text.trim() !== "");
  const parameters = new Set<string>();
  for (let at = 0; at < meaningful.length; at += 1) {
    if (meaningful[at]!.text !== "=>") continue;
    const before = meaningful[at - 1];
    if (!before) continue;
    if (before.role === "mono-ink") {
      parameters.add(before.text);
      continue;
    }
    if (before.text !== ")") continue;
    for (let back = at - 2; back >= 0; back -= 1) {
      const token = meaningful[back]!;
      if (token.text === "(") break;
      if (token.role === "mono-ink") parameters.add(token.text);
    }
  }
  // A parameter reads the same wherever it appears: naming it and using it are the same scaffolding.
  for (const token of tokens) {
    if (token.role === "mono-ink" && parameters.has(token.text)) token.role = "mono-faint";
  }
}

/**
 * Whether each operation was given the number of arguments the package allows.
 *
 * Method style counts the receiver: `$rows.filter(fn)` is two arguments to `filter`, which is why
 * the completion list says "exactly 2 args" for a call that looks like it takes one.
 */
function arityMistakes(tokens: readonly Token[], source: string, language: Language): Mistake[] {
  const meaningful = tokens.filter((token) => token.text.trim() !== "");
  const mistakes: Mistake[] = [];
  for (let at = 0; at < meaningful.length; at += 1) {
    const token = meaningful[at]!;
    if (token.role !== "mono-provider") continue;
    const open = meaningful[at + 1];
    if (open?.text !== "(") continue;
    const receiver = meaningful[at - 1]?.text === "." ? 1 : 0;
    const given = receiver + countArguments(meaningful, at + 1);
    const arity = language.arity(nameOf(meaningful, at));
    if (!arity || (given >= arity.min && given <= arity.max)) continue;
    mistakes.push(
      asMistake(token, source, {
        said: `${nameOf(meaningful, at)} takes ${describeArity(arity)}, not ${given}`,
        hint: said("the package says ", { text: describeArity(arity), role: "mono-literal" }),
      }),
    );
  }
  return mistakes;
}

function nameOf(meaningful: readonly Token[], at: number): string {
  const dot = meaningful[at - 1];
  const before = meaningful[at - 2];
  const word = meaningful[at]!.text;
  return dot?.text === "." && before && before.role === "mono-ink" && /^[\p{L}_]/u.test(before.text)
    ? word
    : word;
}

/** The arguments between one `(` and its `)`, counting only the commas at that depth. */
function countArguments(meaningful: readonly Token[], open: number): number {
  let depth = 0;
  let commas = 0;
  let empty = true;
  for (let at = open; at < meaningful.length; at += 1) {
    const text = meaningful[at]!.text;
    if (text === "(" || text === "[" || text === "{") depth += 1;
    else if (text === ")" || text === "]" || text === "}") {
      depth -= 1;
      if (depth === 0) break;
    } else if (depth === 1) {
      if (text === ",") commas += 1;
      empty = false;
    }
  }
  return empty ? 0 : commas + 1;
}

export function describeArity(arity: { readonly min: number; readonly max: number }): string {
  const word = (count: number) => `${count} arg${count === 1 ? "" : "s"}`;
  return arity.min === arity.max ? `exactly ${word(arity.min)}` : `${arity.min} to ${word(arity.max)}`;
}

function asMistake(token: Token, source: string, what: { said: string; hint: readonly Segment[] }): Mistake {
  const before = source.slice(0, token.from);
  const line = before.split("\n").length - 1;
  const column = token.from - (before.lastIndexOf("\n") + 1);
  return { from: token.from, to: token.from + token.text.length, line, column, text: token.text, said: what.said, hint: what.hint };
}

/** The runs, cut at every newline, so each line can be drawn as its own mono line. */
function intoLines(tokens: readonly Token[], source: string): Segment[][] {
  const lines: Segment[][] = [[]];
  for (const token of tokens) {
    const parts = token.text.split("\n");
    parts.forEach((part, at) => {
      if (at > 0) lines.push([]);
      if (part !== "") lines[lines.length - 1]!.push(token.role ? { text: part, role: token.role } : { text: part });
    });
  }
  // A source that ends in a newline has a last line, and it is empty.
  if (source.endsWith("\n") && lines[lines.length - 1]!.length === 0 && lines.length > 1) return lines;
  return lines;
}
