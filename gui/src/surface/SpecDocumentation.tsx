import type { ReactNode } from "react";

/** A deliberately small Markdown reader. Source HTML is always React text, never markup.
 * No images, embedded resources or relative URLs are fetched from documentation. */
function inline(text: string): ReactNode[] {
  const pattern = /(`[^`\n]+`|\*\*[^*\n]+\*\*|\[[^\]\n]+\]\([^\s)]+\))/g;
  const parts: ReactNode[] = [];
  let start = 0;
  for (const match of text.matchAll(pattern)) {
    parts.push(text.slice(start, match.index));
    const token = match[0];
    const key = match.index;
    if (token.startsWith("`")) parts.push(<code key={key}>{token.slice(1, -1)}</code>);
    else if (token.startsWith("**")) parts.push(<strong key={key}>{token.slice(2, -2)}</strong>);
    else {
      const [, label, href] = /^\[([^\]]+)\]\(([^)]+)\)$/.exec(token)!;
      let safe = false;
      try { safe = ["https:", "http:"].includes(new URL(href!).protocol); } catch { /* plain text */ }
      parts.push(safe ? <a key={key} href={href} target="_blank" rel="noopener noreferrer">{label}</a> : token);
    }
    start = match.index + token.length;
  }
  parts.push(text.slice(start));
  return parts;
}

export function documentationSummary(summary?: string, description?: string): string {
  return (summary?.trim() || description?.trim().split(/\n\s*\n/)[0] || "").replace(/\s+/g, " ");
}

export function SpecDocumentation({ text }: { text?: string }) {
  if (!text?.trim()) return null;
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const blocks: ReactNode[] = [];
  for (let i = 0; i < lines.length;) {
    const line = lines[i]!;
    if (!line.trim()) { i++; continue; }
    const key = i;
    if (/^\s*```/.test(line)) {
      const code: string[] = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i]!)) code.push(lines[i++]!);
      if (i < lines.length) i++;
      blocks.push(<pre key={key}><code>{code.join("\n")}</code></pre>);
    } else if (/^\s*(?:[-*]|\d+\.)\s/.test(line)) {
      const ordered = /^\s*\d+\./.test(line);
      const rule = ordered ? /^\s*\d+\.\s+/ : /^\s*[-*]\s+/;
      const items: ReactNode[] = [];
      while (i < lines.length && rule.test(lines[i]!)) {
        items.push(<li key={i}>{inline(lines[i++]!.replace(rule, ""))}</li>);
      }
      blocks.push(ordered ? <ol key={key}>{items}</ol> : <ul key={key}>{items}</ul>);
    } else if (/^#{1,6}\s/.test(line)) {
      blocks.push(<p className="spec-doc-heading" key={key}>{inline(line.replace(/^#{1,6}\s+/, ""))}</p>); i++;
    } else {
      const paragraph: string[] = [line]; i++;
      while (i < lines.length && lines[i]!.trim() && !/^\s*(?:```|[-*]\s|\d+\.\s|#{1,6}\s)/.test(lines[i]!)) paragraph.push(lines[i++]!);
      blocks.push(<p key={key}>{inline(paragraph.join("\n"))}</p>);
    }
  }
  return <div className="spec-documentation">{blocks}</div>;
}
