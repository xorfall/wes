/**
 * The one platform decision behind the surface's own shortcuts.
 *
 * CodeMirror's `Mod-` is Cmd where the platform calls itself a Mac and Ctrl everywhere else; the
 * prompt asks the same question, so `⌘⏎` in the editor and in the prompt are one key on each
 * platform. The decision is read when a key arrives, not at import, so tests can choose a platform.
 * Only chords that are free on both sides go through it: Ctrl+arrows (word movement elsewhere) and
 * Ctrl+A/E/K… (line movement on macOS) are never derived from a Cmd binding here.
 */

interface Modifiers {
  readonly metaKey: boolean;
  readonly ctrlKey: boolean;
}

interface Platform {
  readonly platform?: string;
}

/** Whether Cmd is the primary modifier, judged the way CodeMirror judges it. */
export function applePlatform(nav: Platform | undefined = typeof navigator === "undefined" ? undefined : navigator): boolean {
  return /Mac|iPhone|iPad|iPod/.test(nav?.platform ?? "");
}

/** The primary modifier held alone of the two: Cmd without Ctrl on Apple platforms, Ctrl without Cmd elsewhere. */
export function primaryHeld(event: Modifiers, apple = applePlatform()): boolean {
  return apple ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
}

/** How a hint writes the primary modifier, in the surface's glyphs: `⌘`, or `⌃` as in `⌃space`. */
export function primaryGlyph(apple = applePlatform()): string {
  return apple ? "⌘" : "⌃";
}

interface Composable {
  readonly isComposing?: boolean;
  readonly keyCode?: number;
  readonly nativeEvent?: { readonly isComposing?: boolean };
}

/**
 * Whether a key belongs to an input method's composition, and so to the text being composed.
 *
 * React's synthetic event keeps the flag on `nativeEvent`; a DOM event carries it itself. `229` is
 * the key code browsers report for the key that ends a composition, after the flag has cleared.
 */
export function composing(event: Composable): boolean {
  return !!(event.nativeEvent?.isComposing || event.isComposing || event.keyCode === 229);
}

interface Chord {
  readonly key: string;
  readonly code?: string;
}

/**
 * The letter or digit a chord was pressed on, lower-cased.
 *
 * On Apple platforms Option rewrites `key` (Option+W is `∑`, Option+1 is `¡`), so the physical
 * `code` names the key there when the event has one. Elsewhere `key` follows the layout.
 */
export function chordKey(event: Chord, apple = applePlatform()): string {
  const physical = apple && event.code ? /^(?:Key([A-Z])|Digit(\d))$/.exec(event.code) : null;
  return physical ? (physical[1] ?? physical[2]!).toLowerCase() : event.key.toLowerCase();
}
