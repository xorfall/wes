/**
 * The `types`/`env` editing mode, read from `yaml-highlight.ts` the way `calc-mode.ts` reads
 * `calc-highlight.ts` — one tokeniser, asked for the shapes an editor needs rather than tokenised a
 * second time. Pure and free of the editor library, so what is decided here is testable without
 * mounting anything.
 */
import { highlightYaml, type Mistake, type TypeVocabulary } from "./yaml-highlight";
import { CLOSES, gutterMark, lineAt, OPENS, pairAt as pairAtSpans, type Pair, type Span } from "./source-mode";
import type { Segment } from "./MonoLine";
import { yamlLocation, type YamlContext, type YamlSchema } from "./yaml-schema";

export { CLOSES, gutterMark, lineAt, OPENS };
export type { Pair, TypeVocabulary };

/** How far one level of nesting indents. YAML's own convention, and `calc-mode.ts`'s. */
export const INDENT = 2;

export interface YamlDiagnostic {
  readonly from: number;
  readonly to: number;
  /** Counted from zero, the way the gutter counts before it adds one. */
  readonly line: number;
  readonly column: number;
  /** The offending token itself, which is how wide the caret row under it is. */
  readonly text: string;
  /** What is wrong, said in one line. */
  readonly message: string;
  /** What the workspace has instead, in the roles the mistake row draws it in. */
  readonly hint: readonly Segment[];
}

export interface YamlMode {
  readonly spans: readonly Span[];
  readonly diagnostics: readonly YamlDiagnostic[];
  /** Which lines carry a mistake, so the gutter can mark them. Counted from zero. */
  readonly marked: readonly number[];
}

/** Everything the editor needs to draw one source, read once. */
export function readMode(source: string, vocabulary: TypeVocabulary, context: YamlContext = "types", schema?: YamlSchema): YamlMode {
  const { mistakes, spans } = highlightYaml(source, vocabulary, context, schema);
  const diagnostics = mistakes.map(asDiagnostic);
  return { spans, diagnostics, marked: [...new Set(diagnostics.map((it) => it.line))].sort((a, b) => a - b) };
}

function asDiagnostic(mistake: Mistake): YamlDiagnostic {
  return {
    from: mistake.from,
    to: mistake.to,
    line: mistake.line,
    column: mistake.column,
    text: mistake.text,
    message: mistake.said,
    hint: mistake.hint,
  };
}

/** The bracket the caret is beside, and its partner — flow mappings and sequences nest with these too. */
export function pairAt(source: string, caret: number, mode: YamlMode): Pair | undefined {
  return pairAtSpans(source, caret, mode.spans);
}

/**
 * How far a line should be indented, counted in spaces.
 *
 * YAML nests by indentation rather than by bracket, so what decides it is the line above: one that
 * ends a block mapping key with nothing after the colon opens a level or a sequence item's own dash
 * asks for its content one level in; anything else keeps the same indent as what is above it. A flow
 * bracket still open when the line begins adds one level on top of that, the same as `calc-mode.ts`.
 */
export function indentAt(source: string, at: number, mode: YamlMode, context?: YamlContext, schema?: YamlSchema): number {
  const lineStart = source.lastIndexOf("\n", Math.max(0, at - 1)) + 1;
  if (context !== undefined) {
    const current = source.slice(lineStart, source.indexOf("\n", lineStart) < 0 ? source.length : source.indexOf("\n", lineStart));
    const leading = /^[ \t]*/.exec(current)![0];
    if (/^[}\]]/.test(current.slice(leading.length))) {
      const pair = pairAt(source, lineStart + leading.length + 1, mode);
      if (pair) {
        const openingLine = source.lastIndexOf("\n", pair.open) + 1;
        return /^[ \t]*/.exec(source.slice(openingLine))![0].length;
      }
    }
    if (lineStart === 0) return 0;
    const previous = source.slice(0, lineStart - 1);
    const edit = newlineAt(previous, previous.length, context, schema);
    return /^[ \t]*/.exec(edit.insert.slice(1))![0].length;
  }
  // Walk back over blank lines to the nearest one that has content.
  let cursor = lineStart - 1;
  let previousLine = "";
  while (cursor >= 0) {
    const previousStart = source.lastIndexOf("\n", cursor - 1) + 1;
    const candidate = source.slice(previousStart, cursor);
    if (candidate.trim() !== "") { previousLine = candidate; break; }
    cursor = previousStart - 1;
  }
  const previousIndent = /^[ \t]*/.exec(previousLine)![0].length;
  const trimmed = previousLine.trim();
  let depth = 0;
  for (const span of mode.spans) {
    if (span.from >= lineStart) break;
    if (span.to - span.from !== 1) continue;
    const character = source[span.from]!;
    if (character === "{" || character === "[") depth += 1;
    else if (character === "}" || character === "]") depth = Math.max(0, depth - 1);
  }
  if (depth > 0) return previousIndent + depth * INDENT;
  if (trimmed === "") return 0;
  if (trimmed.endsWith(":")) return previousIndent + INDENT; // a block mapping key with children below
  if (/^-(\s|$)/.test(trimmed)) return previousIndent + 2; // aligns under the dash's own content
  return previousIndent;
}

/**
 * The word being typed at the caret, which is what completion is about.
 *
 * A type expression's own punctuation (`<`, `,`, `{`, `:`) ends it, so typing `List<Mo` completes
 * `Mo` and not `List<Mo` — the prompt and the calc editor ask the analogous question the same way.
 */
export function wordAt(source: string, caret: number): { readonly text: string; readonly from: number } {
  const before = source.slice(0, caret);
  const match = /([\p{L}\p{N}_]*)$/u.exec(before);
  const text = match?.[1] ?? "";
  return { text, from: caret - text.length };
}

/** Text inserted by Enter. Only an end-of-line structural position may add a
 * sequence marker; splitting a line leaves its suffix and indentation intact. */
export function newlineAt(source: string, caret: number, context: YamlContext, schema?: YamlSchema): { from: number; insert: string } {
  const start = source.lastIndexOf("\n", Math.max(0, caret - 1)) + 1;
  const end = source.indexOf("\n", caret);
  const line = source.slice(start, caret);
  const prefix = /^[ \t]*/.exec(line)![0];
  const content = line.slice(prefix.length);
  const suffix = source.slice(caret, end < 0 ? source.length : end);
  const structuralEnd = /\s+#/.exec(line)?.index;
  const place = yamlLocation(source, structuralEnd === undefined ? caret : start + structuralEnd, context, schema);
  const ordinary = { from: caret, insert: `\n${prefix}` };
  if (suffix.trim() !== "") return ordinary;
  // Literal/folded strings and quoted text never become mapping/list syntax.
  if (place.blocked && !place.closed) return ordinary;
  if (/[:>-]\s*[|>](?:[+-]?[1-9]?|[1-9][+-]?)\s*(?:#.*)?$/.test(content) || /^[|>](?:[+-]?[1-9]?|[1-9][+-]?)\s*$/.test(content)) {
    return { from: caret, insert: `\n${prefix}  ` };
  }
  if (place.flow) {
    // Continuations align one level after the latest flow opener. A completed
    // collection is blocked by the context reader and follows ordinary indent.
    return { from: caret, insert: `\n${prefix}${/[{[]\s*$/.test(content) ? "  " : ""}` };
  }
  const dash = /^-(?:[ \t]+|$)/.exec(content);
  if (dash && content.trim() === "-") return { from: start + prefix.length, insert: "\n" + prefix.slice(0, Math.max(0, prefix.length - 2)) };
  const emptyKey = /:\s*(?:#.*)?$/.test(content);
  if (emptyKey) {
    const nestedPrefix = prefix + (dash ? "  " : "") + "  ";
    if (place.shape.item) return { from: caret, insert: `\n${nestedPrefix}- ` };
    if (place.shape.fields || place.shape.entry) return { from: caret, insert: `\n${nestedPrefix}` };
    return { from: caret, insert: `\n${prefix}${dash ? "  " : ""}` };
  }
  if (dash) {
    if (/^-[ \t]+[^:]+:/.test(content) && !/^-[ \t]+[{[]/.test(content)) return { from: caret, insert: `\n${prefix}  ` };
    return { from: caret, insert: `\n${prefix}- ` };
  }
  return ordinary;
}
