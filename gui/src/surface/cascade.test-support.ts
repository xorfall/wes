/**
 * What a role settles on, read out of the surface's own stylesheets.
 *
 * The surface is asserted through class names, text, and the value a role resolves to — never a
 * screenshot. A test runner has no cascade and no CSSOM, so this reads the two generated files and
 * does the only part of the cascade the surface uses: the root's variables, overlaid by whichever
 * of the two axis blocks the palette and density select, then substituted into the role's own
 * declarations. Nothing in the client imports this; it exists so tests can answer "what colour is
 * a provider call in ink?" without a browser.
 */
import { readFileSync } from "node:fs";
import { roleStyles } from "@wes/view-sdk/theme";

export type Palette = "paper" | "ink" | "white";
export type Density = "normal" | "dense";
export interface Axes {
  readonly palette: Palette;
  readonly density: Density;
}

interface Block {
  readonly selectors: readonly string[];
  readonly declarations: ReadonlyMap<string, string>;
}

const ROOT = ".wes-terminal";

export const tokensCss = read("tokens.css");
export const rolesCss = read("roles.css") + "\n" + roleStyles(ROOT);
export const fontsCss = read("fonts.css");

function read(name: string): string {
  return readFileSync(new URL(`./${name}`, import.meta.url), "utf8");
}

function parse(css: string): Block[] {
  const withoutComments = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const blocks: Block[] = [];
  for (const [, selector, body] of withoutComments.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const declarations = new Map<string, string>();
    for (const declaration of (body ?? "").split(";")) {
      const at = declaration.indexOf(":");
      if (at < 0) continue;
      declarations.set(declaration.slice(0, at).trim(), declaration.slice(at + 1).trim());
    }
    blocks.push({ selectors: (selector ?? "").split(",").map((s) => s.trim()), declarations });
  }
  return blocks;
}

const tokenBlocks = parse(tokensCss);
const roleBlocks = parse(rolesCss);

function declarationsOf(blocks: readonly Block[], selector: string): ReadonlyMap<string, string> {
  const found = blocks.find((block) => block.selectors.includes(selector));
  return found ? found.declarations : new Map();
}

/** The variables in force under one palette and one density, base first then each axis over it. */
export function variables(axes: Axes): Map<string, string> {
  const resolved = new Map(declarationsOf(tokenBlocks, ROOT));
  // Paper is the base, so only the other palettes have a block of their own to lay over it.
  if (axes.palette !== "paper") {
    for (const [name, value] of declarationsOf(tokenBlocks, `${ROOT}[data-palette="${axes.palette}"]`)) {
      resolved.set(name, value);
    }
  }
  if (axes.density === "dense") {
    for (const [name, value] of declarationsOf(tokenBlocks, `${ROOT}[data-density="dense"]`)) resolved.set(name, value);
  }
  return resolved;
}

function substitute(value: string, resolved: ReadonlyMap<string, string>): string {
  return value.replace(/var\((--[\w-]+)\)/g, (whole, name: string) => resolved.get(name) ?? whole);
}

/** Every property a role sets, with its variables replaced by the values the axes select. */
export function role(id: string, axes: Axes): Map<string, string> {
  const resolved = variables(axes);
  const out = new Map<string, string>();
  for (const [property, value] of declarationsOf(roleBlocks, `${ROOT} .${id}`)) {
    out.set(property, substitute(value, resolved));
  }
  return out;
}

/** The ids of every role the stylesheet defines. */
export function roleIds(): string[] {
  return roleBlocks
    .flatMap((block) => block.selectors)
    .filter((selector) => selector.startsWith(`${ROOT} .`))
    .map((selector) => selector.slice(`${ROOT} .`.length));
}
