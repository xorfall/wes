/**
 * The command, formatted for reading.
 *
 * Only `:calc { … }` bodies are rewritten: one statement per line, an object's fields one per
 * line, two spaces of indent per brace, argument lists kept on one line. Strings and comments are
 * copied as written. Everything outside a calc block — other commands, `> name`, pipes — is left
 * exactly as the person wrote it, because nothing here knows those languages.
 */

const INDENT = "  ";
const CALC_OPEN = /:calc\s*(?:pure\s*)?\{/g;

export function formatCommand(source: string): string {
  let out = "";
  let at = 0;
  CALC_OPEN.lastIndex = 0;
  for (let match = CALC_OPEN.exec(source); match; match = CALC_OPEN.exec(source)) {
    const open = match.index + match[0].length - 1;
    const close = closingBrace(source, open);
    if (close === undefined) break;
    out += `${source.slice(at, match.index)}${match[0].slice(0, -1).trimEnd()} ${formatBlock(source.slice(open, close + 1))}`;
    at = close + 1;
    CALC_OPEN.lastIndex = at;
  }
  return out + source.slice(at);
}

/** One token of a calc body: a string, a comment, a run of whitespace, or one character. */
function tokenAt(text: string, at: number): string {
  const char = text[at]!;
  if (char === '"' || char === "'" || char === "`") {
    let end = at + 1;
    while (end < text.length && text[end] !== char) end += text[end] === "\\" ? 2 : 1;
    return text.slice(at, Math.min(text.length, end + 1));
  }
  if (char === "/" && text[at + 1] === "/") {
    const end = text.indexOf("\n", at);
    return text.slice(at, end < 0 ? text.length : end);
  }
  if (char === "/" && text[at + 1] === "*") {
    const end = text.indexOf("*/", at + 2);
    return text.slice(at, end < 0 ? text.length : end + 2);
  }
  if (/\s/.test(char)) {
    let end = at + 1;
    while (end < text.length && /\s/.test(text[end]!)) end += 1;
    return text.slice(at, end);
  }
  return char;
}

/** The `}` that closes the `{` at `open`, or nothing when the block never closes. */
function closingBrace(text: string, open: number): number | undefined {
  let depth = 0;
  for (let at = open; at < text.length;) {
    const token = tokenAt(text, at);
    if (token === "{") depth += 1;
    if (token === "}") { depth -= 1; if (depth === 0) return at; }
    at += token.length;
  }
  return undefined;
}

function formatBlock(block: string): string {
  const out: string[] = [];
  const stack: string[] = [];
  let depth = 0;
  let lineStart = true;
  let pendingSpace = false;
  const newline = () => { if (!lineStart) { out.push("\n"); lineStart = true; pendingSpace = false; } };
  const emit = (text: string) => {
    if (lineStart) { out.push(INDENT.repeat(depth)); lineStart = false; }
    else if (pendingSpace && !/^[)\],;]/.test(text) && !/[([]$/.test(out[out.length - 1] ?? "")) out.push(" ");
    pendingSpace = false;
    out.push(text);
  };
  // A `// comment` on the same line as the statement it follows stays on that line.
  const commentFollows = (from: number): boolean => {
    const gap = from < block.length ? tokenAt(block, from) : "";
    const rest = /^\s/.test(gap) ? from + gap.length : from;
    return !gap.includes("\n") && block.startsWith("//", rest);
  };
  const breakAfter = (at: number) => { if (commentFollows(at)) pendingSpace = true; else newline(); };
  const nextSignificant = (from: number): string | undefined => {
    for (let at = from; at < block.length;) {
      const token = tokenAt(block, at);
      if (!/^\s/.test(token)) return token;
      at += token.length;
    }
    return undefined;
  };
  for (let at = 0; at < block.length;) {
    const token = tokenAt(block, at);
    at += token.length;
    if (/^\s/.test(token)) { if (!lineStart) pendingSpace = true; continue; }
    if (token.startsWith("//")) { emit(token); newline(); continue; }
    if (token === "{") {
      if (nextSignificant(at) === "}") { emit("{}"); at = block.indexOf("}", at) + 1; continue; }
      emit("{"); stack.push("{"); depth += 1; newline(); continue;
    }
    if (token === "}") { if (stack.pop() === "{") depth = Math.max(0, depth - 1); newline(); emit("}"); continue; }
    if (token === "(" || token === "[") { emit(token); stack.push(token); continue; }
    if (token === ")" || token === "]") { stack.pop(); emit(token); continue; }
    if (token === ";") { emit(";"); if (stack[stack.length - 1] === "{") breakAfter(at); else pendingSpace = true; continue; }
    if (token === ",") { emit(","); if (stack[stack.length - 1] === "{") breakAfter(at); else pendingSpace = true; continue; }
    emit(token);
  }
  return out.join("");
}
