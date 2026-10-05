import { describe, expect, it } from "vitest";
import { defaults, restoreSettings } from "./settings";

describe("personal Surface preferences", () => {
  it("round trips every palette while preserving aliases and behavior preferences", () => {
    for (const surfacePalette of ["paper", "ink", "white", "system"] as const) {
      const saved = { ...defaults, surfacePalette, aliases: { twice: ":calc { return 2 * (_); }" },
        stayAtNewest: false, direction: "TB" as const, previewChars: 8000, requestTimeoutMs: 30000 };
      expect(restoreSettings(JSON.parse(JSON.stringify(saved)))).toEqual(saved);
    }
  });
  it("restores only current cell themes and defaults malformed or missing values", () => {
    for (const surfaceChrome of ["controls", "keys"] as const) {
      const saved = { ...defaults, surfaceChrome };
      expect(restoreSettings(JSON.parse(JSON.stringify(saved)))).toEqual(saved);
    }
    for (const surfaceChrome of [undefined, null, false, 7, {}, "unsupported"]) {
      expect(restoreSettings({ surfaceChrome }).surfaceChrome).toBe(defaults.surfaceChrome);
    }
  });
  it("ignores unsupported saved keys without losing current preferences or mutating stored data", () => {
    const stored = { appearance: "unsupported-client", theme: "unsupported-palette", termBackground: "unused",
      surfacePalette: "white", aliases: { ls: 'sh run cmd:"ls _"' }, previewChars: 8000 };
    const before = JSON.stringify(stored);
    const restored = restoreSettings(stored);
    expect(restored).toEqual({ ...defaults, surfacePalette: "white", aliases: stored.aliases, previewChars: 8000 });
    expect(JSON.stringify(stored)).toBe(before);
  });
  it("should_ShowTailKeysByDefaultAndRestoreHidden_When_PreferencesAreRead", () => {
    expect(defaults.surfaceTailKeys).toBe("shown");
    expect(restoreSettings({ surfaceTailKeys: "hidden" }).surfaceTailKeys).toBe("hidden");
    expect(restoreSettings({ surfaceTailKeys: "sideways" }).surfaceTailKeys).toBe("shown");
    expect(restoreSettings({}).surfaceTailKeys).toBe("shown");
  });
  it("should_OutlineAFocusedCellByDefaultAndRestoreTheWash_When_PreferencesAreRead", () => {
    expect(defaults.surfaceFocus).toBe("outline");
    expect(restoreSettings({ surfaceFocus: "wash" }).surfaceFocus).toBe("wash");
    expect(restoreSettings({ surfaceFocus: "violet" }).surfaceFocus).toBe("outline");
    expect(restoreSettings({}).surfaceFocus).toBe("outline");
  });
  it("recovers from malformed preferences and validates current fields", () => {
    expect(restoreSettings(null)).toEqual(defaults);
    expect(restoreSettings([])).toEqual(defaults);
    expect(restoreSettings({ surfacePalette: "unknown", surfaceChrome: "unknown", surfaceFace: 'un"known', surfaceSansFace: "a{b}",
      surfaceDensity: "unknown", direction: "unknown", previewChars: -1, requestTimeoutMs: NaN })).toEqual(defaults);
  });
});
