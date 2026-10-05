import { afterEach, describe, expect, it, vi } from "vitest";
import { applePlatform, chordKey, composing, primaryGlyph, primaryHeld } from "./platform-keys";

const held = (metaKey: boolean, ctrlKey: boolean) => ({ metaKey, ctrlKey });

describe("the platform's primary modifier", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("should_MatchCodeMirrorsMod_When_ThePlatformIsNamed", () => {
    for (const platform of ["MacIntel", "iPhone", "iPad"]) expect(applePlatform({ platform })).toBe(true);
    for (const platform of ["Win32", "Linux x86_64", "X11", ""]) expect(applePlatform({ platform })).toBe(false);
    expect(applePlatform({})).toBe(false);
    // Node has its own `navigator` naming the host, so "no navigator" is stated rather than assumed.
    vi.stubGlobal("navigator", undefined);
    expect(applePlatform()).toBe(false);
  });

  it("should_ReadTheNavigatorWhenAsked_When_NoPlatformIsGiven", () => {
    vi.stubGlobal("navigator", { platform: "Win32" });
    expect([applePlatform(), primaryGlyph()]).toEqual([false, "⌃"]);
    vi.stubGlobal("navigator", { platform: "MacIntel" });
    expect([applePlatform(), primaryGlyph()]).toEqual([true, "⌘"]);
  });

  it("should_AcceptOnlyThePrimaryAlone_When_ModifiersAreHeld", () => {
    expect(primaryHeld(held(true, false), true)).toBe(true);
    expect(primaryHeld(held(false, true), true)).toBe(false);
    expect(primaryHeld(held(true, true), true)).toBe(false);
    expect(primaryHeld(held(false, true), false)).toBe(true);
    expect(primaryHeld(held(true, false), false)).toBe(false);
    expect(primaryHeld(held(true, true), false)).toBe(false);
    expect(primaryHeld(held(false, false), true)).toBe(false);
  });

  it("should_ReadThePhysicalKeyOnApple_When_OptionRewroteTheCharacter", () => {
    expect(chordKey({ key: "∑", code: "KeyW" }, true)).toBe("w");
    expect(chordKey({ key: "¡", code: "Digit1" }, true)).toBe("1");
    expect(chordKey({ key: "w" }, true)).toBe("w");
    expect(chordKey({ key: "ArrowLeft", code: "ArrowLeft" }, true)).toBe("arrowleft");
    expect(chordKey({ key: "z", code: "KeyW" }, false)).toBe("z");
  });

  it("should_SeeAComposition_When_AnyBrowserSignalsOne", () => {
    expect(composing({ nativeEvent: { isComposing: true } })).toBe(true);
    expect(composing({ isComposing: true })).toBe(true);
    expect(composing({ keyCode: 229 })).toBe(true);
    expect(composing({ keyCode: 13, isComposing: false, nativeEvent: { isComposing: false } })).toBe(false);
  });
});
