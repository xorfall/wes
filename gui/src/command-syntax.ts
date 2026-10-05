import type { MetaCommand } from "./vocabulary";

type SyntaxCommands = readonly Pick<MetaCommand, "name" | "takes">[];
export type SyntaxRole = "meta" | "note" | "verb" | "ui" | "arg" | "str";
export interface SyntaxToken { readonly text: string; readonly role?: SyntaxRole }

/** Display-only lexical roles, never a validator or executor. Slices preserve the original source. */
export function commandTokens(source: string, commands: SyntaxCommands = []): SyntaxToken[] {
  const tokens: SyntaxToken[] = [];
  const name = /[\p{L}\p{N}_-][\p{L}\p{N}_.-]*/uy;
  const whitespace = /\s+/y;
  let at = 0, depth = 0;
  let path: "start" | "verb" | "tail" | "help" | "value" = "start";
  let meta = "";
  const emit = (end: number, role?: SyntaxRole) => {
    tokens.push({ text: source.slice(at, end), ...(role ? { role } : {}) }); at = end;
  };
  const nameEnd = (start: number) => { name.lastIndex = start; return name.exec(source) ? name.lastIndex : start; };
  const quotedEnd = (start: number) => {
    const quote = source[start]; let end = start + 1;
    while (end < source.length) {
      if (source[end] === "\\") { end = Math.min(source.length, end + 2); continue; }
      if (source[end++] === quote) break;
    }
    return end;
  };
  while (at < source.length) {
    whitespace.lastIndex = at;
    const space = whitespace.exec(source);
    if (space) {
      if (space[0].includes("\n") && depth === 0) path = "start";
      emit(whitespace.lastIndex); continue;
    }
    const ch = source[at];
    if (ch === "|" && depth === 0) { emit(at + 1); path = "start"; meta = ""; continue; }
    if (ch === '"' || ch === "'") { emit(quotedEnd(at), "str"); path = "value"; continue; }
    if (depth > 0 && source.startsWith("//", at)) {
      const end = source.indexOf("\n", at); emit(end < 0 ? source.length : end); continue;
    }
    if (depth > 0 && source.startsWith("/*", at)) {
      const end = source.indexOf("*/", at + 2); emit(end < 0 ? source.length : end + 2); continue;
    }
    if (ch === "@" && path === "start") {
      let end = nameEnd(at + 1);
      if (end > at + 1) {
        const closing: string[] = [];
        if (source[end] === "(" || source[end] === "{") {
          closing.push(source[end++] === "(" ? ")" : "}");
          while (end < source.length && closing.length) {
            const c = source[end];
            if (c === '"' || c === "'") {
              const quoted = quotedEnd(end);
              if (end > at) emit(end, "note");
              emit(quoted, "str"); end = quoted; continue;
            }
            if (c === "(" || c === "{") closing.push(c === "(" ? ")" : "}");
            else if (c === closing[closing.length - 1]) closing.pop();
            end++;
          }
        }
        if (end > at) emit(end, "note");
        continue;
      }
    }
    if (path === "start" && (ch === ":" || ch === "/")) {
      const end = nameEnd(at + 1);
      if (end > at + 1) {
        meta = source.slice(at + 1, end);
        emit(end, ch === ":" ? "meta" : "ui");
        path = ch === "/" ? "value" : meta === "help" ? "help" : "tail";
        continue;
      }
    }
    // Keys start at a token boundary. Once a bare value starts, its URL/colon content is opaque.
    const endName = nameEnd(at);
    if (endName > at && source[endName] === ":" && source[endName + 1] !== ":") {
      emit(endName + 1, "arg"); path = "value";
      // A bare value extends to its delimiter; do not reinterpret http: or nested colons as keys.
      let end = at;
      while (end < source.length && !/[\s{}\[\](),;>|"']/.test(source[end]!)) end++;
      if (end > at) emit(end);
      continue;
    }
    if (ch === "{" || ch === "[" || ch === "(") { depth++; path = "value"; emit(at + 1); continue; }
    if (ch === "}" || ch === "]" || ch === ")") { depth = Math.max(0, depth - 1); path = "value"; emit(at + 1); continue; }
    if (ch === ">" || source.startsWith("*>", at)) { path = "value"; emit(at + (ch === ">" ? 1 : 2)); continue; }
    if (ch === "," || ch === ";") { emit(at + 1); continue; }
    let end = at + 1;
    while (end < source.length && !/[\s{}\[\](),;>|"']/.test(source[end]!)) end++;
    const word = source.slice(at, end);
    if (path === "start") path = word.startsWith("$") ? "value" : "verb";
    if (word.startsWith("$")) path = "value";
    if (path === "help") {
      // Help accepts either a meta command name or a provider/capability path.
      if (commands.some(command => command.name === word.replace(/^:/, ""))) {
        meta = word; emit(end, "meta"); path = "tail";
      } else { emit(end, "verb"); path = "verb"; }
    } else if (path === "tail") {
      const keyword = commands.find(command => command.name === meta)?.takes.includes(word);
      emit(end, keyword ? "meta" : undefined); path = "value";
    } else emit(end, path === "verb" ? "verb" : undefined);
  }
  return tokens;
}

/** Completion boundary only: quoted shell pipes and calc operators are not wes stages. */
export function lastPipeEnd(source: string): number {
  let offset = 0, depth = 0, end = 0;
  for (const token of commandTokens(source)) {
    if (!token.role) {
      if (["{", "[", "("].includes(token.text)) depth++;
      else if (["}", "]", ")"].includes(token.text)) depth = Math.max(0, depth - 1);
      else if (token.text === "|" && depth === 0) end = offset + 1;
      else if (depth === 0 && token.text.includes("\n") && source.slice(end, offset).trim()
        && !source.slice(offset + token.text.length).trimStart().startsWith("|")) end = 0;
    }
    offset += token.text.length;
  }
  return end;
}
