import { readFileSync } from "node:fs";
import { expect, it } from "vitest";
import { defineAlias, expandAlias, restoreAliases } from "./aliases";
import { complete } from "./complete";
import { run } from "./commands";
import { defaults, restoreSettings } from "./settings";
import { emptyCatalogue } from "./vocabulary";

it("expands the real tutorial recipe once and stores ordinary source independently of future aliases", () => {
  const recipe = JSON.parse(readFileSync(new URL("../../examples/aliases/recipe.json", import.meta.url), "utf8"));
  let settings = defaults;
  for (const command of recipe.define) settings = run(command, settings, emptyCatalogue).settings!;
  for (const { input, expanded } of recipe.cases) expect(expandAlias(input, settings.aliases, emptyCatalogue)).toBe(expanded);
  const recorded = expandAlias("twice 21", settings.aliases, emptyCatalogue);
  const changed = defineAlias(settings.aliases, "twice", ':calc { return 99; }', emptyCatalogue);
  expect(expandAlias("twice", changed, emptyCatalogue)).toContain("99");
  expect(recorded).toBe(':calc { return 2 * (21); }');
});

it("escapes quoted fragments without treating embedded source as another statement", () => {
  const aliases = defineAlias({}, "say", 'sh run cmd:"printf _ my_file"', emptyCatalogue);
  const typed = 'say "hello"\\path\n:calc { return 42; }';
  const expanded = expandAlias(typed, aliases, emptyCatalogue);
  const string = expanded.slice('sh run cmd:'.length);
  expect(JSON.parse(string)).toBe('printf "hello"\\path\n:calc { return 42; } my_file');
  expect(expandAlias(':list nodes', aliases, emptyCatalogue)).toBe(':list nodes');
});

it("refuses collisions at definition and again after an environment catalogue changes", () => {
  const aliases = defineAlias({}, "fetch", ':calc { return 1; }', emptyCatalogue);
  const catalogue = { ...emptyCatalogue, templates: [{ name: "fetch", body: "", parameters: [] }] };
  expect(() => defineAlias({}, "fetch", ':calc { return 1; }', catalogue)).toThrow(/already/);
  expect(() => expandAlias('fetch', aliases, catalogue)).toThrow(/conflicts/);
  expect(complete('fet', 3, catalogue, [], false, aliases).items.filter(x => x.kind === 'alias')).toHaveLength(0);
  expect(complete('fet', 3, emptyCatalogue, [], false, aliases).items).toContainEqual(expect.objectContaining({ text: 'fetch', kind: 'alias' }));
});

it("rejects recursive, ambiguous and oversized shortcuts and validates saved preferences", () => {
  const aliases = defineAlias({}, "one", 'two value:_', emptyCatalogue);
  expect(() => defineAlias(aliases, "two", 'one value:_', emptyCatalogue)).toThrow(/aliases/);
  expect(() => defineAlias({}, "one", '/reset', emptyCatalogue)).toThrow(/client/);
  expect(() => defineAlias({}, "one", 'sh run cmd:"_ _"', emptyCatalogue)).toThrow(/only one/);
  expect(() => defineAlias({}, "one", 'sh run cmd:"_ ', emptyCatalogue)).toThrow(/Close/);
  expect(() => defineAlias({}, "one", 'x'.repeat(5000), emptyCatalogue)).toThrow(/full/);
  expect(restoreAliases({ bad: 42, valid: ':list nodes' })).toEqual({ valid: ':list nodes' });
  const restored = restoreSettings({ surfacePalette: 'white', aliases });
  expect(restored.surfacePalette).toBe('white');
  expect(restored.aliases).toEqual(aliases);
  expect(expandAlias(':list nodes', restored.aliases, emptyCatalogue)).toBe(':list nodes');
  expect(() => expandAlias('valid extra', {valid: ':list nodes'}, emptyCatalogue)).toThrow(/no.*placeholder/);
});
