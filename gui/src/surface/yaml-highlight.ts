/** YAML syntax colors plus semantic expectations from the engine-published schema. */
import { schemaNode, schemaShape, schemaChild, type SchemaCursor, type YamlContext, type YamlSchema } from "./yaml-schema";
import type { MonoRole, Segment } from "./MonoLine";
import type { Mistake, Span } from "./source-mode";

export type { Mistake, Span };
export { mistakeRows } from "./source-mode";

export interface Highlighted {
  /** One array of runs per line of the source, whitespace and all. */
  readonly lines: readonly (readonly Segment[])[];
  readonly mistakes: readonly Mistake[];
  /** The same runs again, as offsets into the source — one tokeniser, asked twice. */
  readonly spans: readonly Span[];
}

/** What the editor checks a bare word against, wherever a type is expected. */
export interface TypeVocabulary {
  /** Every type name the workspace currently resolves: built-ins, `:package load`ed records, `List` &c. */
  readonly names: readonly string[];
}

interface Token {
  readonly text: string;
  readonly from: number;
  role: MonoRole | undefined;
  mistake?: { readonly said: string; readonly hint: readonly Segment[] };
}

function hint(lead: string, what: Segment): Segment[] {
  return what.text === "" ? [{ text: lead, role: "mono-dim" }] : [{ text: lead, role: "mono-dim" }, what];
}

/** Is `name` (or the whole of a `Ctor<...>` expression) something the vocabulary resolves? */
function knownType(expression: string, names: ReadonlySet<string>, constructors: YamlSchema["typeConstructors"]): boolean {
  const generic = /^([A-Za-z_][\w-]*)<(.*)>$/s.exec(expression);
  if (!generic) return names.has(expression);
  const [, ctor, inner] = generic;
  const constructor = constructors.find(item => item.name === ctor);
  const arguments_ = splitArguments(inner!);
  return !!constructor && arguments_.length === constructor.parameters.length &&
    arguments_.every(argument => knownType(argument.trim(), names, constructors));
}

/** Top-level commas only: `Map<Text, List<Int>>`'s inner comma belongs to `List`, not to `Map`. */
function splitArguments(text: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let at = 0; at < text.length; at += 1) {
    const character = text[at];
    if (character === "<") depth += 1;
    else if (character === ">") depth -= 1;
    else if (character === "," && depth === 0) {
      parts.push(text.slice(start, at));
      start = at + 1;
    }
  }
  parts.push(text.slice(start));
  return parts.filter((part) => part.trim() !== "");
}

export function highlightYaml(source: string, vocabulary: TypeVocabulary, context: YamlContext = "types", schema?: YamlSchema): Highlighted {
  const names = new Set(vocabulary.names);
  const constructors = schema?.typeConstructors ?? [];
  const tokens: Token[] = [];
  const stack = [{ indent: -1, node: schemaNode(schema, schema ? [schema.roots[context]] : []) }];
  let literalIndent: number | undefined;
  let at = 0;
  const push = (text: string, role: MonoRole | undefined, mistake?: Token["mistake"]) => {
    if (text === "") return;
    tokens.push({ text, from: at, role, mistake });
    at += text.length;
  };
  const unknownType = (word: string): Token["mistake"] => ({
    said: `unknown type: ${word}`,
    hint: hint("defined in this workspace: ", { text: [...names].slice(0, 3).join(", ") || "none yet", role: "mono-literal" }),
  });
  const unknownValue = (word: string, choices: readonly string[]): Token["mistake"] => ({
    said: `unexpected value: ${word}`,
    hint: hint("expected ", { text: choices.join(", "), role: "mono-literal" }),
  });
  function space(flow = false): void {
    while (at < source.length) {
      const whitespace = (flow ? /^\s+/ : /^[ \t]+/).exec(source.slice(at));
      if (whitespace) push(whitespace[0], undefined);
      if (!flow || source[at] !== "#") return;
      const end = source.indexOf("\n", at);
      push(source.slice(at, end < 0 ? source.length : end), "mono-faint");
    }
  }
  function scalar(current: SchemaCursor): void {
    const shape = schemaShape(current);
    const begin = at;
    let end = at;
    const quote = source[at] === "'" || source[at] === '"' ? source[at] : undefined;
    if (quote) {
      end++;
      let closed = false;
      while (end < source.length) {
        if (quote === '"' && source[end] === "\\") { end += 2; continue; }
        if (source[end] === quote) {
          if (quote === "'" && source[end + 1] === quote) { end += 2; continue; }
          end++; closed = true; break;
        }
        end++;
      }
      if (!closed) {
        push(source.slice(at), "mono-bad", { said: "this string is never closed", hint: hint("close it with ", { text: quote, role: "mono-literal" }) });
        return;
      }
    } else {
      let angles = 0;
      while (end < source.length) {
        const character = source[end]!;
        if (character === "\n" || (angles === 0 && ",{}[]".includes(character)) ||
          (character === "#" && (end === begin || /\s/.test(source[end - 1]!)))) break;
        if (shape.type && character === "<") angles++;
        if (shape.type && character === ">") angles = Math.max(0, angles - 1);
        end++;
      }
      while (end > begin && /[ \t]/.test(source[end - 1]!)) end--;
    }
    if (end === begin) { push(source[at] ?? "", "mono-faint"); return; }
    const text = source.slice(begin, end);
    const value = quote ? text.slice(1, -1) : text;
    if (shape.type && shape.scalars?.every(scalar => scalar.type === "TypeExpression")) {
      const known = knownType(value, names, constructors);
      push(text, known ? "mono-ref" : "mono-bad", known ? undefined : unknownType(value));
    } else if (shape.values?.length && shape.scalars?.every(scalar => scalar.choices !== undefined)) {
      const known = shape.scalars.some(scalar => scalar.choices!.some(choice => {
        if (scalar.type === "Bool") return value.toLowerCase() === choice.toLowerCase();
        if (scalar.type === "Int" && /^[+-]?\d+$/.test(value) && /^[+-]?\d+$/.test(choice)) return BigInt(value) === BigInt(choice);
        if (scalar.type === "Decimal" && value.trim() !== "" && choice.trim() !== "") return Number.isFinite(Number(value)) && Number(value) === Number(choice);
        return value === choice;
      }));
      push(text, known ? (/^(true|false)$/i.test(value) && !quote ? "mono-meta" : "mono-ref") : "mono-bad", known ? undefined : unknownValue(value, shape.values));
    } else if (/^(true|false)$/i.test(value) && !quote) {
      push(text, "mono-meta");
    } else push(text, quote || /^-?\d+(?:\.\d+)?$/.test(value) ? "mono-literal" : "mono-ink");
  }
  function value(current: SchemaCursor): void {
    space();
    if (source[at] === "{") {
      push("{", "mono-faint");
      while (at < source.length) {
        space(true);
        if (source[at] === "}") { push("}", "mono-faint"); return; }
        if (source[at] === ",") { push(",", "mono-faint"); continue; }
        if (!entry(current)) scalar(schemaNode());
      }
    } else if (source[at] === "[") {
      push("[", "mono-faint");
      while (at < source.length) {
        space(true);
        if (source[at] === "]") { push("]", "mono-faint"); return; }
        if (source[at] === ",") { push(",", "mono-faint"); continue; }
        value(schemaNode(current.schema, schemaShape(current).item));
      }
    } else scalar(current);
  }
  /** Consume one mapping entry; only block entries can open an indentation frame. */
  function entry(parent: SchemaCursor, blockIndent?: number): boolean {
    const key = /^(?:"([^"\n]*)"|'([^'\n]*)'|([\p{L}_][\p{L}\p{N}_-]*))([ \t]*):/u.exec(source.slice(at));
    if (!key) return false;
    const name = key[1] ?? key[2] ?? key[3]!;
    const keyText = key[0].slice(0, -(key[4]!.length + 1));
    const known = Object.hasOwn(schemaShape(parent).fields ?? {}, name);
    push(keyText, blockIndent === 0 && known ? "mono-meta" : blockIndent === undefined || known ? "mono-param" : "mono-ink");
    push(key[4]!, undefined); push(":", "mono-faint"); space();
    const child = schemaChild(parent, name);
    const begin = at;
    if (blockIndent !== undefined && (at === source.length || source[at] === "\n" || source[at] === "#")) {
      stack.push({ indent: blockIndent, node: child });
    } else if (blockIndent !== undefined && /^[|>](?:[+-]?[1-9]?|[1-9][+-]?)(?:\s|$)/.test(source.slice(at))) {
      const end = source.indexOf("\n", at);
      push(source.slice(at, end < 0 ? source.length : end), "mono-literal");
      literalIndent = blockIndent;
    } else value(child);
    parent.seen[name] = source.slice(begin, at).trim().replace(/^['"]|['"]$/g, "");
    return true;
  }
  while (at < source.length) {
    const end = source.indexOf("\n", at);
    const lineEnd = end < 0 ? source.length : end;
    const indent = /^[ \t]*/.exec(source.slice(at, lineEnd))![0];
    push(indent, undefined);
    const body = source.slice(at, lineEnd);
    if (literalIndent !== undefined && (body === "" || indent.length > literalIndent)) {
      push(body, "mono-literal");
    } else {
      literalIndent = undefined;
      while (stack.length > 1 && stack.at(-1)!.indent >= indent.length) stack.pop();
      let parent = stack.at(-1)!.node;
      if (body.startsWith("#")) push(body, "mono-faint");
      else if (body !== "") {
        const dash = /^-(?:[ \t]+|$)/.exec(body);
        if (dash) {
          push(dash[0], "mono-faint");
          parent = schemaNode(parent.schema, schemaShape(parent).item);
          stack.push({ indent: indent.length, node: parent });
        }
        if (!entry(parent, indent.length + (dash?.[0].length ?? 0))) {
          if (source[at] === "{" || source[at] === "[") value(parent);
          else scalar(schemaNode());
        }
      }
    }
    // Flow/quoted values may have consumed several lines; never rewind the tokenizer.
    const remainder = source.indexOf("\n", at);
    const restEnd = remainder < 0 ? source.length : remainder;
    if (at < restEnd) push(source.slice(at, restEnd), "mono-faint");
    if (source[at] === "\n") push("\n", undefined);
  }
  const mistakes = tokens
    .filter((token): token is Token & { mistake: NonNullable<Token["mistake"]> } => token.mistake !== undefined)
    .map((token) => asMistake(token, source));
  return { lines: intoLines(tokens, source), mistakes, spans: spansOf(tokens) };
}

function spansOf(tokens: readonly Token[]): Span[] {
  return tokens
    .filter((token) => token.text !== "")
    .map((token) => ({
      from: token.from,
      to: token.from + token.text.length,
      ...(token.role === undefined ? {} : { role: token.role }),
    }));
}

function asMistake(token: Token & { mistake: NonNullable<Token["mistake"]> }, source: string): Mistake {
  const before = source.slice(0, token.from);
  const line = before.split("\n").length - 1;
  const column = token.from - (before.lastIndexOf("\n") + 1);
  return {
    from: token.from,
    to: token.from + token.text.length,
    line,
    column,
    text: token.text,
    said: token.mistake.said,
    hint: token.mistake.hint,
  };
}

function intoLines(tokens: readonly Token[], source: string): Segment[][] {
  const lines: Segment[][] = [[]];
  for (const token of tokens) {
    const parts = token.text.split("\n");
    parts.forEach((part, index) => {
      if (index > 0) lines.push([]);
      if (part !== "") lines[lines.length - 1]!.push(token.role ? { text: part, role: token.role } : { text: part });
    });
  }
  if (source.endsWith("\n") && lines[lines.length - 1]!.length === 0 && lines.length > 1) return lines;
  return lines;
}
