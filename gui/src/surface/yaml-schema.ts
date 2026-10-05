/** Tolerant YAML location reader driven exclusively by the engine's schema graph. */
import type { YamlSchema } from "./yaml-schema-protocol";
export type { YamlSchema } from "./yaml-schema-protocol";
export type YamlContext = "env" | "types";
export interface Shape {
  readonly fields?: Readonly<Record<string, readonly string[]>>;
  readonly entry?: readonly string[];
  readonly item?: readonly string[];
  readonly values?: readonly string[];
  readonly type?: boolean;
  readonly scalars?: readonly { readonly type: string; readonly choices?: readonly string[] }[];
  readonly expected: string;
}
const scalar: Shape = { expected: "Value" };
interface Node { schema?: YamlSchema; refs: readonly string[]; seen: Record<string, string>; }
const node = (schema?: YamlSchema, refs: readonly string[] = []): Node => ({ schema, refs, seen: Object.create(null) });
const unique = <T,>(values: readonly T[]): T[] => [...new Set(values)];

function merge(shapes: readonly Shape[]): Shape {
  const keys = unique(shapes.flatMap(shape => Object.keys(shape.fields ?? {})));
  const fields = keys.length ? Object.fromEntries(keys.map(key => [key, unique(shapes.flatMap(shape => shape.fields?.[key] ?? []))])) : undefined;
  const entries = unique(shapes.flatMap(shape => shape.entry ?? []));
  const items = unique(shapes.flatMap(shape => shape.item ?? []));
  const values = unique(shapes.flatMap(shape => shape.values ?? []));
  return { fields, ...(entries.length ? { entry: entries } : {}), ...(items.length ? { item: items } : {}),
    ...(values.length ? { values } : {}), type: shapes.some(shape => shape.type), scalars: shapes.flatMap(shape => shape.scalars ?? []),
    expected: unique(shapes.map(shape => shape.expected)).join(" | ") || "Value" };
}
function describe(schema: YamlSchema, ref: string, seen: Readonly<Record<string, string>> = {}): Shape {
  const definition = Object.hasOwn(schema.definitions, ref) ? schema.definitions[ref] : undefined;
  if (!definition) return scalar;
  let shape: Shape;
  switch (definition.kind) {
    case "any": shape = scalar; break;
    case "scalar": shape = { expected: definition.type, values: definition.choices, type: definition.type === "TypeExpression", scalars: [definition] }; break;
    case "map": shape = { entry: [definition.values], expected: `Map<Text, ${describe(schema, definition.values).expected}>` }; break;
    case "list": shape = { item: [definition.items], expected: `List<${describe(schema, definition.items).expected}>` }; break;
    case "union": shape = merge(definition.variants.map(variant => describe(schema, variant, seen))); break;
    case "discriminated": {
      const value = seen[definition.field];
      const selected = value === undefined || !Object.hasOwn(definition.variants, value) ? undefined : definition.variants[value];
      shape = selected ? describe(schema, selected, seen) : merge(Object.values(definition.variants).map(variant => describe(schema, variant, seen)));
      break;
    }
    case "object": {
      let allowed = Object.keys(definition.fields);
      for (const condition of definition.conditions ?? []) {
        const value = seen[condition.field];
        if (value !== undefined && (condition.values?.includes(value) || (condition.prefix !== undefined && value.startsWith(condition.prefix)))) {
          allowed = allowed.filter(key => condition.allowed.includes(key));
        }
      }
      for (const group of definition.exclusive ?? []) {
        if (group.fields.filter(key => Object.hasOwn(seen, key)).length >= group.max) {
          allowed = allowed.filter(key => !group.fields.includes(key) || Object.hasOwn(seen, key));
        }
      }
      shape = { fields: Object.fromEntries(allowed.map(key => [key, [definition.fields[key]!.schema]])), expected: "Object" };
      break;
    }
  }
  return definition.hint ? { ...shape, expected: definition.hint } : shape;
}
function shapeOf(n: Node): Shape {
  return n.schema ? merge(n.refs.map(ref => describe(n.schema!, ref, n.seen))) : scalar;
}
function fields(n: Node): Readonly<Record<string, readonly string[]>> { return shapeOf(n).fields ?? {}; }
function child(n: Node, key: string): Node {
  const known = fields(n);
  return node(n.schema, Object.hasOwn(known, key) ? known[key] : shapeOf(n).entry);
}

export interface YamlLocation {
  readonly shape: Shape;
  readonly keys?: readonly string[];
  readonly keyTypes?: Readonly<Record<string, string>>;
  readonly from: number;
  readonly to: number;
  readonly prefix: string;
  readonly blocked?: boolean;
  readonly flow?: boolean;
  readonly closed?: boolean;
}
function location(source: string, caret: number, start: number, n: Node, key: boolean, flow = false): YamlLocation {
  const before = source.slice(start, caret);
  // A # is a comment only at a YAML separation boundary. Quotes are permitted
  // solely for schema type expressions, where 'List<T>' is a common spelling.
  const quoted = /^[ \t]*['"]/.test(before);
  const closedQuote = /^[ \t]*(['"])(?:.*)\1[ \t]*$/.test(before);
  const blocked = /(^|\s)#/.test(before) || (quoted && (!shapeOf(n).type || closedQuote));
  const word = /[\p{L}\p{N}_-]*$/u.exec(before)?.[0] ?? "";
  const rest = /^[\p{L}\p{N}_-]*/u.exec(source.slice(caret))?.[0] ?? "";
  return { shape: shapeOf(n), ...(key ? { keys: Object.keys(fields(n)).filter(k => !Object.hasOwn(n.seen, k)),
    keyTypes: Object.fromEntries(Object.keys(fields(n)).map(name => [name, shapeOf(child(n, name)).expected])) } : {}),
    from: caret - word.length, to: caret + rest.length, prefix: word, blocked, flow };
}

/** Scan flow collections without requiring a complete YAML document. */
function inline(source: string, start: number, caret: number, n: Node): YamlLocation {
  let at = start;
  const space = () => {
    while (at < caret) {
      if (/\s/.test(source[at]!)) { at++; continue; }
      if (source[at] === "#") {
        const end = source.indexOf("\n", at);
        if (end >= 0 && end < caret) { at = end + 1; continue; }
      }
      break;
    }
  };
  function value(current: Node): YamlLocation | undefined {
    space();
    const begin = at;
    const open = source[at];
    if (at < caret && (open === "{" || open === "[")) {
      at++;
      const map = open === "{";
      const body = map ? current : node(current.schema, shapeOf(current).item);
      while (at < caret) {
        space();
        if (source[at] === (map ? "}" : "]")) { at++; return undefined; }
        if (source[at] === ",") { at++; continue; }
        if (map) {
          const keyStart = at;
          const match = /^(?:"([^"\n]*)"|'([^'\n]*)'|([\p{L}\p{N}_-]+))\s*:/u.exec(source.slice(at, caret));
          if (!match) return location(source, caret, keyStart, body, true, true);
          const key = match[1] ?? match[2] ?? match[3]!;
          at += match[0].length;
          const valueStart = at;
          const result = value(child(body, key));
          if (result) return result;
          body.seen[key] = source.slice(valueStart, at).trim().replace(/^['"]|['"]$/g, "");
        } else {
          const result = value(body);
          if (result) return result;
        }
        space();
        if (at < caret && !",]}".includes(source[at]!)) at++;
      }
      return location(source, caret, at, body, map, true);
    }
    let quote = "";
    let angles = 0;
    while (at < caret) {
      const c = source[at]!;
      if (quote) {
        if (c === "\\" && quote === '"') { at += 2; continue; }
        if (c === quote) { if (source[at + 1] === quote && quote === "'") { at += 2; continue; } quote = ""; }
      } else {
        if (c === "'" || c === '"') quote = c;
        else if (c === "<") angles++;
        else if (c === ">") angles = Math.max(0, angles - 1);
        else if (angles === 0 && ",]}".includes(c)) return undefined;
      }
      at++;
    }
    return location(source, caret, begin, current, false, true);
  }
  return value(n) ?? { ...location(source, caret, at, node(), false), blocked: true, closed: true };
}

/** A block frame belongs to a mapping key's value or to a sequence item. */
interface Frame { indent: number; node: Node; }
function quoteCloses(text: string, quote: string, start: number): boolean {
  for (let i = start; i < text.length; i++) {
    if (quote === '"' && text[i] === "\\") { i++; continue; }
    if (text[i] === quote) {
      if (quote === "'" && text[i + 1] === "'") { i++; continue; }
      return true;
    }
  }
  return false;
}
export function yamlLocation(source: string, caret: number, context: YamlContext, schema?: YamlSchema): YamlLocation {
  const stack: Frame[] = [{ indent: -1, node: node(schema, schema ? [schema.roots[context]] : []) }];
  let offset = 0;
  let literalIndent: number | undefined;
  let scalarQuote: string | undefined;
  let flowStart: { at: number; node: Node } | undefined;
  for (const line of source.slice(0, caret).split("\n")) {
    const current = offset + line.length === caret;
    const whitespace = /^[ \t]*/.exec(line)![0];
    const indent = whitespace.length;
    const body = line.slice(indent);
    if (scalarQuote !== undefined) {
      if (quoteCloses(line, scalarQuote, 0)) scalarQuote = undefined;
      if (current) return { ...location(source, caret, offset, node(), false), blocked: true };
      offset += line.length + 1; continue;
    }
    if (literalIndent !== undefined) {
      if (body === "" || indent > literalIndent) {
        if (current) return { ...location(source, caret, offset, node(), false), blocked: true };
        offset += line.length + 1; continue;
      }
      literalIndent = undefined;
    }
    if (flowStart) {
      const loc = inline(source, flowStart.at, offset + line.length, flowStart.node);
      if (current) return loc;
      if (loc.closed) flowStart = undefined;
      offset += line.length + 1; continue;
    }
    if (!current && (body.trim() === "" || body.startsWith("#"))) { offset += line.length + 1; continue; }
    while (stack.length > 1 && stack[stack.length - 1]!.indent >= indent) stack.pop();
    let parent = stack[stack.length - 1]!.node;
    let start = offset + indent;
    const dash = /^-(?:[ \t]+|$)/.exec(body);
    if (dash) {
      parent = node(parent.schema, shapeOf(parent).item);
      stack.push({ indent, node: parent });
      start += dash[0].length;
    }
    const content = source.slice(start, offset + line.length);
    const key = /^(?:"([^"\n]*)"|'([^'\n]*)'|([\p{L}\p{N}_-]+))\s*:/u.exec(content);
    if (!key) {
      if (current) return (content.trim().startsWith("{") || content.trim().startsWith("["))
        ? inline(source, start, caret, parent)
        : location(source, caret, start, parent, !dash || !!shapeOf(parent).fields);
    } else {
      const name = key[1] ?? key[2] ?? key[3]!;
      const valueStart = start + key[0].length;
      const value = source.slice(valueStart, offset + line.length).trim();
      const valueNode = child(parent, name);
      if (current) {
        if (/^[ \t]*[\[{]/.test(source.slice(valueStart, caret))) return inline(source, valueStart, caret, valueNode);
        return location(source, caret, valueStart, valueNode, false);
      }
      parent.seen[name] = value.replace(/\s+#.*$/, "").replace(/^['"]|['"]$/g, "");
      if ((value[0] === "'" || value[0] === '"') && !quoteCloses(value, value[0], 1)) scalarQuote = value[0];
      else if (/^[|>](?:[+-]?[1-9]?|[1-9][+-]?)(?:\s|$)/.test(value)) literalIndent = start - offset;
      else if (value === "" || value.startsWith("#")) stack.push({ indent: start - offset, node: valueNode });
      else if (/^[\[{]/.test(value)) {
        const loc = inline(source, valueStart, offset + line.length, valueNode);
        if (!loc.closed) flowStart = { at: valueStart, node: valueNode };
      }
    }
    offset += line.length + 1;
  }
  return location(source, caret, caret, stack[0]!.node, true);
}

/** Shared schema traversal for the single-pass syntax highlighter. */
export { node as schemaNode, shapeOf as schemaShape, child as schemaChild };
export type { Node as SchemaCursor };
