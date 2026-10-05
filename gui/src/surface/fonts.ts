/**
 * The font families a picker offers: the faces the client ships, then the machine's own, split into
 * monospaced (data) and proportional (interface) by the platform's own fixed-pitch mark. Read once
 * per client from `GET /fonts`; where that route is absent or fails, the pickers offer the shipped
 * faces and the ones the browser can find by name.
 */
import { DEFAULT_FACE, DEFAULT_SANS_FACE, validFace } from "../settings";

export interface FontFamily {
  readonly name: string;
  readonly monospace: boolean;
}

export interface FontGroups {
  /** Faces the client carries itself: always available. */
  readonly shipped: readonly string[];
  /** Faces found on this machine, sorted, without the shipped ones. */
  readonly installed: readonly string[];
}

/** Faces the client knows the machine may have without asking the catalogue (system-hidden ones). */
const KNOWN_MONO = ["SF Mono"];
const KNOWN_SANS = ["system-ui"];

let catalogue: Promise<readonly FontFamily[]> | undefined;

/** The machine's families, read once; an unreadable answer reads as none. */
export function fontCatalogue(fetcher: typeof fetch = fetch): Promise<readonly FontFamily[]> {
  catalogue ??= fetcher("/fonts", { cache: "no-store" })
    .then(async (response) => (response.ok ? response.json() : { families: [] }) as Promise<{ families?: unknown }>)
    .then(({ families }) => (Array.isArray(families) ? families : [])
      .filter((family): family is FontFamily => typeof family?.name === "string" && validFace(family.name) && typeof family?.monospace === "boolean"))
    .catch(() => []);
  return catalogue;
}

/** Forget the read catalogue (tests; a font installed while the client runs). */
export function resetFontCatalogue() {
  catalogue = undefined;
}

/**
 * The groups one picker offers. Data fonts are monospaced only and interface fonts proportional
 * only; the chosen face is always offered, even when the catalogue does not list it.
 */
export function fontGroups(families: readonly FontFamily[], kind: "mono" | "sans", chosen: string, found: (name: string) => boolean): FontGroups {
  const shipped = [kind === "mono" ? DEFAULT_FACE : DEFAULT_SANS_FACE];
  const listed = families.filter((family) => family.monospace === (kind === "mono")).map((family) => family.name);
  const known = (kind === "mono" ? KNOWN_MONO : KNOWN_SANS).filter((name) => name === "system-ui" || found(name));
  const installed = [...new Set([...listed, ...known, ...(shipped.includes(chosen) ? [] : [chosen])])]
    .filter((name) => !shipped.includes(name))
    .sort((a, b) => a.localeCompare(b));
  return { shipped, installed };
}
