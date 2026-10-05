import type { IBufferRange, ILink, ILinkHandler, Terminal } from "@xterm/xterm";

/**
 * Web addresses in terminal output, opened with Cmd-click (Ctrl-click elsewhere). A plain click
 * keeps selecting text. The address goes through `window.open`: a browser tab opens a tab, and the
 * desktop shell hands any other site to the person's browser.
 */
const WEB = /https?:\/\/[^\s"'`<>()[\]{}]+/g;
/** Punctuation that usually closes the sentence around an address rather than the address. */
const TRAILING = /[.,;:!?]+$/;
/** One logical line is read at most this far; a longer run of wrapped rows is not searched further. */
const MAX_LINE = 8192;

export interface FoundLink { readonly start: number; readonly end: number; readonly url: string }

/** Only an http or https address with a host and no credentials leaves the terminal. */
export function webAddress(text: string): string | undefined {
  try {
    const url = new URL(text);
    const web = url.protocol === "http:" || url.protocol === "https:";
    return web && url.hostname && !url.username && !url.password ? url.href : undefined;
  } catch {
    return undefined;
  }
}

/** The web addresses in `text`, as [start, end) offsets. */
export function findWebLinks(text: string): FoundLink[] {
  const found: FoundLink[] = [];
  for (const match of text.matchAll(WEB)) {
    const url = match[0].replace(TRAILING, "");
    if (webAddress(url) !== undefined) found.push({ start: match.index, end: match.index + url.length, url });
  }
  return found;
}

/** Cmd on macOS, Ctrl elsewhere. */
export function wantsLink(event: Pick<MouseEvent, "metaKey" | "ctrlKey">): boolean {
  return event.metaKey || event.ctrlKey;
}

type Open = (url: string, target: string, features: string) => unknown;

export function openWebAddress(text: string, open: Open = (url, target, features) => window.open(url, target, features)): void {
  const href = webAddress(text);
  if (href !== undefined) open(href, "_blank", "noopener,noreferrer");
}

/** OSC 8 hyperlinks: the same rule as detected addresses, without the default confirm dialog. */
export function hyperlinkHandler(open?: Open): ILinkHandler {
  return {
    activate: (event, text) => { if (wantsLink(event)) openWebAddress(text, open); },
    allowNonHttpProtocols: false,
  };
}

interface Cell { readonly x: number; readonly y: number }
type Buffer = Pick<Terminal["buffer"]["active"], "getLine">;

/** The text of the wrapped line through buffer row `y` (0-based), with the cell of each character. */
function logicalLine(buffer: Buffer, y: number): { text: string; cells: Cell[] } {
  let first = y;
  while (first > 0 && buffer.getLine(first)?.isWrapped) first--;
  let text = "";
  const cells: Cell[] = [];
  for (let row = first; text.length < MAX_LINE; row++) {
    const line = buffer.getLine(row);
    if (!line || (row > first && !line.isWrapped)) break;
    for (let x = 0; x < line.length; x++) {
      const cell = line.getCell(x);
      if (!cell) break;
      // The right half of a wide character carries nothing of its own.
      if (cell.getWidth() === 0) continue;
      const chars = cell.getChars() || " ";
      for (let i = 0; i < chars.length; i++) cells.push({ x, y: row });
      text += chars;
    }
  }
  return { text, cells };
}

/** Links that touch buffer row `row` (1-based, as xterm asks for them). */
export function linksOnRow(buffer: Buffer, row: number, open?: Open): ILink[] {
  const { text, cells } = logicalLine(buffer, row - 1);
  return findWebLinks(text).flatMap(found => {
    const first = cells[found.start], last = cells[found.end - 1];
    if (!first || !last) return [];
    const range: IBufferRange = { start: { x: first.x + 1, y: first.y + 1 }, end: { x: last.x + 1, y: last.y + 1 } };
    if (range.start.y > row || range.end.y < row) return [];
    return [{
      range,
      text: found.url,
      decorations: { underline: true, pointerCursor: true },
      activate: (event: MouseEvent, url: string) => { if (wantsLink(event)) openWebAddress(url, open); },
    }];
  });
}

/** Detect web addresses in the terminal's output. Returns the release. */
export function linkTerminal(term: Terminal): () => void {
  const provider = term.registerLinkProvider({
    provideLinks: (row, callback) => {
      const links = linksOnRow(term.buffer.active, row);
      callback(links.length ? links : undefined);
    },
  });
  return () => provider.dispose();
}
