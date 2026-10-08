import type { Catalogue } from "./vocabulary";
import type { Completion, Suggestion } from "./complete";

/** Lexical completion only. Blank strings/comments preserve UTF-16 offsets and block boundaries. */
export function calculationAt(text: string): { start: number; mask: string; quoted: boolean } | undefined {
  if (text.length > 16384) return undefined;
  let mask = "", quote = "", comment = "", depth = 0, start = -1;
  for (let i = 0; i < text.length; i++) {
    const c = text[i]!, next = text[i + 1];
    if (comment) {
      if (comment === "//" && c === "\n") comment = "";
      else if (comment === "/*" && c === "*" && next === "/") { mask += "  "; i++; comment = ""; continue; }
      mask += " "; continue;
    }
    if (quote) {
      mask += " ";
      if (c === "\\") { mask += " "; i++; }
      else if (c === quote) quote = "";
      continue;
    }
    if (c === '"' || c === "'") { quote = c; mask += " "; continue; }
    if (c === "/" && (next === "/" || next === "*")) { comment = c + next; mask += "  "; i++; continue; }
    mask += c;
    if (start < 0 && /(?:^|\n)\s*(?::def\s+[^\n{}]+\s+as\s+)?:calc\s*(?:pure\s*)?\{$/.test(mask)) { start = i + 1; depth = 1; }
    else if (start >= 0 && c === "{") depth++;
    else if (start >= 0 && c === "}" && --depth === 0) start = -1;
  }
  return start < 0 ? undefined : { start, mask, quoted: !!quote || !!comment };
}
export function calculationCompletion(before: string, catalogue: Catalogue, names: readonly string[], piped = false): Completion | undefined {
  const context = calculationAt(before);
  if (!context) return undefined;
  const body = before.slice(context.start);
  const offer = (prefix: string, items: Suggestion[]): Completion => ({ from: before.length - prefix.length, items: [...new Map(items.filter(i => i.text.startsWith(prefix)).map(i => [i.text, i])).values()].slice(0, 40) });
  const mask = context.mask.slice(context.start);
  // Editing discovery is approximate; execution scope is resolved by the engine.
  const locals: Suggestion[] = [];
  const signature = /:def\s+[^()\n]+\(([^)]*)\)[^{}]*$/.exec(context.mask.slice(0, context.start - 1));
  if (signature) for (const parameter of signature[1]!.matchAll(/(?:^|,)\s*([\p{L}_][\p{L}\p{N}_-]*)\s*:/gu)) locals.push({ text: parameter[1]!, kind: "local" });
  for (const match of mask.matchAll(/(?:const|let|function)\s+([\p{L}_][\p{L}\p{N}_]*)/gu)) locals.push({ text: match[1]!, kind: "local" });
  for (const match of mask.matchAll(/\bfunction(?:\s+[\p{L}_][\p{L}\p{N}_]*)?\s*\(([^)]*)\)|\(([^)]*)\)\s*=>|([\p{L}_][\p{L}\p{N}_]*)\s*=>/gu)) {
    for (const param of (match[1] ?? match[2] ?? match[3] ?? "").split(",")) if (/^[\p{L}_][\p{L}\p{N}_]*$/u.test(param.trim())) locals.push({ text: param.trim(), kind: "local" });
  }
  if (piped) locals.push({ text: "input", kind: "local" });
  const localCall = locals.some(item => item.text === "call");
  // Static metadata positions in the documented call form; no provider query or execution.
  const provider = /\bcall\(\s*['"]([^'"]*)$/.exec(body);
  if (provider && !localCall) return offer(provider[1]!, catalogue.providers.map(p => ({ text: p.name, kind: "provider" })));
  const path = /\bcall\(\s*['"]([^'"]+)['"]\s*,\s*\[([^\]]*)['"]([^'"]*)$/.exec(body);
  if (path && !localCall) {
    const previous = [...path[2]!.matchAll(/['"]([^'"]+)['"]\s*,/g)].map(m => m[1]);
    const capabilities = catalogue.providers.find(p => p.name === path[1])?.capabilities ?? [];
    return offer(path[3]!, capabilities.filter(c => previous.every((p,i) => c.path[i] === p)).flatMap(c => c.path[previous.length] ? [{ text: c.path[previous.length]!, kind: "capability" }] : []));
  }
  if (context.quoted) return { from: before.length, items: [] };
  const word = /[$\p{L}_][\p{L}\p{N}_:$-]*$/u.exec(before)?.[0] ?? "";
  if (word.startsWith("$")) return offer(word, names.map(name => ({ text: `$${name}`, kind: "reference" })));
  // A call or index result is a receiver too: `stripAnsi(raw).te` reads a field, not a free operation.
  const field = /([\p{L}_][\p{L}\p{N}_]*|[)\]])\.([\p{L}\p{N}_]*)$/u.exec(mask);
  if (field?.[1] === "iter") {
    return offer(field[2]!, (catalogue.calculation?.operations ?? []).filter(name => name.startsWith("iter.")).map(name => ({text:name.slice(5),kind:"operation"})));
  }
  if (field) {
    // Only names the vocabulary announces as methods; an operation it does not is function syntax only.
    const items: Suggestion[] = (catalogue.calculation?.methods ?? []).map(text => ({ text, kind: "operation" }));
    // Approximate literal fields; the engine still validates scope, types and callable fields.
    for (const record of mask.matchAll(/(?:const|let)\s+([\p{L}_][\p{L}\p{N}_]*)\s*=\s*\{([^{}]*)/gu)) {
      if (record[1] === field[1]) for (const key of record[2]!.matchAll(/([\p{L}_][\p{L}\p{N}_]*)\s*:/gu)) items.push({ text: key[1]!, kind: "field" });
    }
    return offer(field[2]!, items);
  }
  const items: Suggestion[] = (catalogue.calculation?.keywords ?? []).map(text => ({ text, kind: "keyword" }));
  items.push(...(catalogue.calculation?.operations ?? []).map(text => ({ text, kind: "operation" })));
  items.push(...["true", "false", "none"].map(text => ({ text, kind: "value" })));
  items.push(...locals);
  return offer(word, items);
}
