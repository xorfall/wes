import { budget } from "./limits/policy";
import type { Catalogue } from "./vocabulary";

/** Personal prompt shortcuts; never expanded by the engine, panels or replay. */
export type Aliases = Readonly<Record<string, string>>;
export const emptyAliases: Aliases = Object.freeze({});
function max_aliases():number { return budget("ui.aliases"); }
function max_bytes():number { return budget("ui.alias.bytes"); }
const validName = /^[A-Za-z][A-Za-z0-9-]{0,47}$/;

export function collision(name: string, catalogue: Catalogue): boolean {
  return catalogue.providers.some(p => p.name === name) ||
    (catalogue.templates ?? []).some(t => t.name === name);
}

function hole(template: string): { at: number; quoted: boolean } | undefined {
  let quoted = false, escaped = false;
  let found: { at: number; quoted: boolean } | undefined;
  for (let at = 0; at < template.length; at++) {
    const c = template[at];
    if (escaped) { escaped = false; continue; }
    if (quoted && c === "\\") { escaped = true; continue; }
    if (c === '"') { quoted = !quoted; continue; }
    // A standalone underscore is a hole; identifiers and paths with underscores survive.
    if (c === "_" && (at === 0 || /[\s:"([{,=]/.test(template[at - 1]!)) &&
        (at + 1 === template.length || /[\s")\]},;]/.test(template[at + 1]!))) {
      if (found) throw new Error("An alias may have only one standalone '_' placeholder.");
      found = { at, quoted };
    }
  }
  if (quoted) throw new Error("Close the quoted string in the alias template.");
  return found;
}

function validate(name: string, template: string, aliases: Aliases): void {
  if (!validName.test(name)) throw new Error("Alias names start with a letter and use letters, digits or hyphens (up to 48 characters).");
  if (!template.trim() || template.trimStart().startsWith("/")) throw new Error("An alias must expand to an engine command, not a client '/' command.");
  const first = template.trimStart().split(/\s/)[0]!;
  if (first === name || Object.hasOwn(aliases, first)) throw new Error("Aliases cannot call other aliases or themselves.");
  hole(template);
}

export function defineAlias(aliases: Aliases, name: string, template: string, catalogue: Catalogue): Aliases {
  if (collision(name, catalogue)) throw new Error(`'${name}' is already a provider or command template in this environment.`);
  validate(name, template, aliases);
  const next = { ...aliases, [name]: template };
  // Also reject turning another alias's target into a shortcut after its definition.
  for (const [key, value] of Object.entries(next)) validate(key, value, next);
  if (Object.keys(next).length > max_aliases() || new TextEncoder().encode(JSON.stringify(next)).length > max_bytes())
    throw new Error("The alias preferences budget is full. Remove an alias first.");
  return next;
}

export function restoreAliases(value: unknown): Aliases {
  if (!value || typeof value !== "object" || Array.isArray(value)) return emptyAliases;
  let result = emptyAliases;
  for (const [name, template] of Object.entries(value)) {
    if (typeof template !== "string") continue;
    try { result = defineAlias(result, name, template, { commands: [], annotations: [], providers: [] }); }
    catch { /* Invalid preference entries never become executable shortcuts. */ }
  }
  return result;
}

export function expandAlias(text: string, aliases: Aliases, catalogue: Catalogue): string {
  const match = /^\s*([A-Za-z][A-Za-z0-9-]*)(?:[ \t]+([\s\S]*))?$/.exec(text);
  if (!match || !Object.hasOwn(aliases, match[1]!)) return text;
  const name = match[1]!, template = aliases[name]!, args = match[2] ?? "";
  if (collision(name, catalogue)) throw new Error(`Alias '${name}' conflicts with a provider or command template. Remove or rename the alias before calling this name.`);
  validate(name, template, aliases);
  const slot = hole(template);
  if (!slot) {
    if (args.trim()) throw new Error(`Alias '${name}' has no '_' placeholder and takes no arguments.`);
    return template;
  }
  // Inside a string the typed fragment is data, including quotes/newlines/backslashes.
  // Outside it the fragment is intentional wes source. Neither form recursively expands.
  const inserted = slot.quoted ? JSON.stringify(args).slice(1, -1) : args;
  return template.slice(0, slot.at) + inserted + template.slice(slot.at + 1);
}
