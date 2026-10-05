/**
 * One line of the terminal surface.
 *
 * Everything the surface says about a command — the command itself, its verdict, a diagnostic, a
 * footer — is one line of monospace text made of runs, each run carrying the role that colours it.
 * Generic status lines clip overflowing text. Command and prompt callers opt into source blocks:
 * explicit newlines and whitespace are retained. Command bands may opt into visual punctuation
 * breaks; they never insert source characters or rewrite the supplied segments.
 */
import "./surface.css";
import { Fragment, type ReactNode } from "react";

/** The roles a run on a mono line may take. Bold is reserved for the four `-strong` roles. */
export type MonoRole =
  | "mono-ink"
  | "mono-dim"
  | "mono-faint"
  | "mono-meta"
  | "mono-provider"
  | "mono-param"
  | "mono-literal"
  | "mono-ref"
  | "mono-ok"
  | "mono-warn"
  | "mono-bad"
  | "mono-ink-strong"
  | "mono-ref-strong"
  | "mono-meta-strong"
  | "mono-bad-strong";

export interface Segment {
  readonly text: string;
  /** A run without a role is ordinary text. */
  readonly role?: MonoRole;
  readonly variableName?: string;
}

export interface MonoLineProps {
  readonly segments: readonly Segment[];
  /** Placed beside `mono-line`, for whatever the line belongs to. */
  readonly className?: string;
  readonly description?: string;
  /** For a line something else scrolls, such as the drawn half of the prompt under its field. */
  readonly innerRef?: import("react").Ref<HTMLPreElement>;
  /** Visual break opportunities only; the source text and selection/copy remain unchanged. */
  readonly sourceWrap?: boolean;
}

export function MonoLine({ segments, className, description, innerRef, sourceWrap = false }: MonoLineProps) {
  const breaks = sourceWrap ? sourceBreaks(lineText(segments)) : new Set<number>();
  let offset = 0;
  return (
    <pre className={className ? `mono-line ${className}` : "mono-line"} aria-description={description} ref={innerRef}>
      {segments.map((segment, at) => {
        const children: ReactNode[] = [];
        let start = 0;
        for (let i = 0; i < segment.text.length; i++) if (breaks.has(offset + i)) {
          children.push(segment.text.slice(start, i), <wbr key={i} />); start = i;
        }
        children.push(segment.text.slice(start)); offset += segment.text.length;
        return <Fragment key={at}><span className={`${segment.role ?? "mono-ink"}${sourceWrap && segment.role === "mono-literal" && (segment.text.startsWith('"') || segment.text.startsWith("'")) ? " source-string" : ""}`} data-variable-name={segment.variableName}>{children}</span></Fragment>;
      })}
    </pre>
  );
}

/** Scan across highlighting boundaries, including escaped quotes, without touching string data. */
export function sourceBreaks(text: string): Set<number> {
  const breaks = new Set<number>();
  let quote: string | undefined, escaped = false;
  for (let at = 0; at < text.length; at++) {
    const char = text[at];
    if (quote) {
      if (escaped) escaped = false;
      else if (char === "\\") escaped = true;
      else if (char === quote) quote = undefined;
    } else if (char === '"' || char === "'") quote = char;
    else if (char === "{" || char === "[") breaks.add(at);
    else if (char === "," || char === ";") breaks.add(at + 1);
  }
  return breaks;
}

/** Everything the line reads as, roles forgotten. */
export function lineText(segments: readonly Segment[]): string {
  return segments.map((segment) => segment.text).join("");
}

/**
 * The column a run starts in, counted in characters.
 *
 * A diagnostic points at characters, not at pixels, so the row that carries the carets is built by
 * counting to the offending run and padding to it. Both rows are the same face, so equal character
 * offsets are equal positions on screen.
 */
export function columnOf(segments: readonly Segment[], index: number): number {
  return lineText(segments.slice(0, index)).length;
}

/** A row of carets under one run of another line, in the bad role, ready to render as its own line. */
export function caretsUnder(segments: readonly Segment[], index: number, note?: string): Segment[] {
  const run = segments[index];
  if (!run) return [];
  const carets: Segment[] = [
    { text: " ".repeat(columnOf(segments, index)) },
    { text: "^".repeat(run.text.length), role: "mono-bad" },
  ];
  return note ? [...carets, { text: note, role: "mono-bad" }] : carets;
}
