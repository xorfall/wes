/**
 * Width in display columns, the unit every budget of the presentation layer is measured in.
 *
 * A character count is not a width: `東` takes two columns of a monospace line, `é` written as `e`
 * plus a combining accent takes one, and a family emoji is one cluster of seven code points drawn two
 * columns wide. The cell's contract says widths are display columns of grapheme clusters with East
 * Asian wide and fullwidth characters counted as two, so ellipsis, table columns and the gutter all
 * ask this module rather than `String.length`.
 */

import { graphemeSegments, splitGraphemes } from "unicode-segmenter/grapheme";

/** Maximum UTF-16 input inspected solely to lay out a preview or count its omissions. */
export const TEXT_MEASURE_UNITS = 16_384;

/** A bounded prefix ending at a confirmed grapheme boundary, never inside an unfinished cluster. */
export function textPrefix(text: string, units = TEXT_MEASURE_UNITS): { text: string; whole: boolean } {
  if (text.length <= units) return { text, whole: true };
  const prefix = text.slice(0, units);
  let end = 0;
  // The final cluster may continue beyond the slice. Every preceding boundary is confirmed.
  for (const part of graphemeSegments(prefix)) end = part.index;
  return { text: prefix.slice(0, end), whole: false };
}

/** The ellipsis every shortened text ends or breaks with. One column. */
export const ELLIPSIS = "…";

/** Lazy extended grapheme clusters without copying the input into native ICU segmentation. */
export function* clusters(text: string): IterableIterator<string> {
  yield* splitGraphemes(text);
}

/** East Asian Wide and Fullwidth ranges, plus the emoji blocks drawn at double width. */
function wide(code: number): boolean {
  return (
    (code >= 0x1100 && code <= 0x115f) ||
    (code >= 0x2e80 && code <= 0x303e) ||
    (code >= 0x3041 && code <= 0x33ff) ||
    (code >= 0x3400 && code <= 0x4dbf) ||
    (code >= 0x4e00 && code <= 0x9fff) ||
    (code >= 0xa000 && code <= 0xa4cf) ||
    (code >= 0xac00 && code <= 0xd7a3) ||
    (code >= 0xf900 && code <= 0xfaff) ||
    (code >= 0xfe30 && code <= 0xfe4f) ||
    (code >= 0xff00 && code <= 0xff60) ||
    (code >= 0xffe0 && code <= 0xffe6) ||
    (code >= 0x1f300 && code <= 0x1f64f) ||
    (code >= 0x1f900 && code <= 0x1f9ff) ||
    (code >= 0x1f680 && code <= 0x1f6ff) ||
    (code >= 0x20000 && code <= 0x3fffd)
  );
}

/** Columns one grapheme cluster takes: 0 for a lone combining mark or control, 2 for wide, else 1. */
export function clusterWidth(cluster: string): number {
  const first = cluster.codePointAt(0) ?? 0;
  if (first < 0x20 || (first >= 0x7f && first < 0xa0)) return 0;
  if (/^\p{Mark}+$/u.test(cluster)) return 0;
  // An emoji presentation selector or a joined sequence is drawn as one wide picture.
  if (/\p{Extended_Pictographic}/u.test(cluster)
    && (cluster.includes("‍") || cluster.includes("️") || /\p{Emoji_Presentation}/u.test(cluster))) return 2;
  return wide(first) ? 2 : 1;
}

/** Display columns; with a finite limit, incomplete measurement conservatively reports overflow. */
export function width(text: string, limit = Infinity): number {
  const measured = Number.isFinite(limit) ? textPrefix(text) : { text, whole: true };
  let columns = 0;
  for (const cluster of clusters(measured.text)) {
    columns += clusterWidth(cluster);
    // Callers testing fit need only know that it exceeds the available columns.
    if (columns > limit) return columns;
  }
  return measured.whole ? columns : Math.max(columns, limit + 1);
}

/** The text shortened to `columns`, ending in `…` when anything was left out. */
export function ellipsizeEnd(text: string, columns: number): string {
  if (width(text, columns) <= columns) return text;
  if (columns <= 0) return "";
  let used = 0;
  let out = "";
  for (const cluster of clusters(textPrefix(text).text)) {
    const w = clusterWidth(cluster);
    if (used + w > columns - 1) break;
    out += cluster;
    used += w;
  }
  return out + ELLIPSIS;
}

/**
 * The text shortened to `columns` with the ellipsis in the middle, so both ends stay readable.
 *
 * Gutter names use this: `$very_long_result_name_number_one` and `…_number_two` differ only at the
 * end, which an end ellipsis would cut away.
 */
export function ellipsizeMiddle(text: string, columns: number): string {
  if (width(text, columns) <= columns) return text;
  if (columns <= 1) return columns === 1 ? ELLIPSIS : "";
  const room = columns - 1;
  const headRoom = Math.ceil(room / 2);
  const tailRoom = room - headRoom;
  let head = "";
  let used = 0;
  for (const cluster of clusters(text)) {
    const w = clusterWidth(cluster);
    if (used + w > headRoom) break;
    head += cluster;
    used += w;
  }
  // Keep only the suffix that fits, rather than retaining every cluster in the payload.
  const tail: { text: string; width: number }[] = [];
  used = 0;
  for (const cluster of clusters(text)) {
    const w = clusterWidth(cluster);
    if (w === 0) continue;
    tail.push({ text: cluster, width: w });
    used += w;
    while (used > tailRoom) used -= tail.shift()!.width;
  }
  return head + ELLIPSIS + tail.map(part => part.text).join("");
}

/** Pads with spaces to `columns` display columns. Never shortens. */
export function pad(text: string, columns: number): string {
  const missing = columns - width(text, columns);
  return missing > 0 ? text + " ".repeat(missing) : text;
}

/** Source text with tabs expanded to the next multiple of `size` columns (source strip, tab-size 4). */
export function expandTabs(text: string, size = 4): string {
  let out = "";
  let column = 0;
  for (const cluster of clusters(text)) {
    if (cluster === "\t") {
      const add = size - (column % size);
      out += " ".repeat(add);
      column += add;
    } else {
      out += cluster;
      column += clusterWidth(cluster);
    }
  }
  return out;
}
