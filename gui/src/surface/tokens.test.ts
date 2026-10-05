import { describe, expect, it } from "vitest";
import { fontsCss, role, roleIds, rolesCss, variables, type Axes } from "./cascade.test-support";
import { defaults, monoFamily, resolveSurfacePalette, restoreSettings, sansFamily, SURFACE_FACES, surfaceTypeStyle } from "../settings";
import { readFileSync } from "node:fs";
import { viewTheme } from "@wes/view-sdk/theme";
import { appearanceRows, faceChosen } from "./settings-model";
import { read } from "./commands";

const paper: Axes = { palette: "paper", density: "normal" };
const ink: Axes = { palette: "ink", density: "normal" };
const white: Axes = { palette: "white", density: "normal" };

describe("the surface's palette", () => {
  it("should_ColourAProviderCallGreen_When_ThePaletteIsPaper", () => {
    expect(role("mono-provider", paper).get("color")).toBe("#2C6B3C");
  });

  it("should_ColourAProviderCallLighterGreen_When_ThePaletteIsInk", () => {
    expect(role("mono-provider", ink).get("color")).toBe("#8FBF7F");
  });

  it("should_TurnEveryMonoRole_When_ThePaletteChanges", () => {
    const eleven = ["ink", "dim", "faint", "meta", "provider", "param", "literal", "ref", "ok", "warn", "bad"];
    for (const name of eleven) {
      const onPaper = role(`mono-${name}`, paper).get("color");
      const onInk = role(`mono-${name}`, ink).get("color");
      expect(onPaper, name).toMatch(/^#[0-9A-F]{6}$/);
      expect(onInk, name).toMatch(/^#[0-9A-F]{6}$/);
      expect(onInk, name).not.toBe(onPaper);
    }
  });

  it("should_KeepTheGroundAndTheSunkStripApart_When_AnyPaletteIsChosen", () => {
    for (const axes of [paper, ink, white]) {
      const ground = variables(axes).get("--ground");
      const sunk = variables(axes).get("--sunk");
      expect(role("surface-terminal", axes).get("background-color")).toBe(ground);
      expect(role("surface-sunk", axes).get("background-color")).toBe(sunk);
      expect(ground).not.toBe(sunk);
    }
  });
});

/*
 * The third palette: paper's own ink on a pure white ground.
 *
 * It exists because paper's warmth is a choice, and somebody printing, projecting or simply wanting
 * a plain ground should not have to take the warmth with it. So the ground and the surfaces around
 * it change and nothing else does — the roles that carry meaning say the same thing they say on
 * paper, because what they mean has not changed.
 */
describe("the surface's third palette", () => {
  it("should_PaintThePageWhiteAndNothingElse_When_TheWhitePaletteIsChosen", () => {
    const tokens = variables(white);
    expect(tokens.get("--ground")).toBe("#FFFFFF");
    expect(tokens.get("--sunk")).toBe("#F5F5F5");
    expect(tokens.get("--line")).toBe("#E6E6E6");
    expect(tokens.get("--rail")).toBe("#DADADA");
    expect(tokens.get("--sel")).toBe("rgba(106, 75, 168, 0.07)");
    expect(tokens.get("--surface")).toBe("#FFFFFF");
    expect(tokens.get("--surface-variant")).toBe("#F5F5F5");
  });

  it("should_KeepEveryRoleColourAsPaper_When_TheWhitePaletteIsChosen", () => {
    const eleven = ["ink", "dim", "faint", "meta", "provider", "param", "literal", "ref", "ok", "warn", "bad"];
    for (const name of eleven) {
      expect(role(`mono-${name}`, white).get("color"), name).toBe(role(`mono-${name}`, paper).get("color"));
    }
    // And the rest of what carries meaning: the badges, the statuses, the frames and the rails.
    const ground = variables(paper).get("--ground");
    for (const id of roleIds()) {
      if (!/^(badge|status|frame|rail)-/.test(id)) continue;
      const onPaper = role(id, paper);
      // A frame or a rail at rest is the ground it sits on, and the ground is the one thing that moved.
      if (onPaper.get("background-color") === ground || onPaper.get("border-color") === ground) continue;
      expect(role(id, white).get("color"), id).toBe(onPaper.get("color"));
    }
  });

  it("should_ResolveItsOwnGround_When_EachOfTheThreePalettesIsAsked", () => {
    const grounds = [paper, ink, white].map((axes) => variables(axes).get("--ground"));
    expect(new Set(grounds).size).toBe(3);
    expect(grounds[2]).toBe("#FFFFFF");
    for (const axes of [paper, ink, white]) {
      expect(role("surface-terminal", axes).get("background-color")).toBe(variables(axes).get("--ground"));
      expect(role("surface-sunk", axes).get("background-color")).toBe(variables(axes).get("--sunk"));
    }
  });

  it("should_KeepTheChoice_When_TheThemeCommandIsTypedAndTheSettingsAreSaved", () => {
    const typed = read("/theme white");
    expect(typed.kind).toBe("settings");
    expect(typed.kind === "settings" && typed.change(defaults).surfacePalette).toBe("white");
    expect(restoreSettings({ ...defaults, surfacePalette: "white" }).surfacePalette).toBe("white");
    // Chosen, never inferred: the machine has a light mode and a dark one, and neither is white.
    expect(resolveSurfacePalette("white")).toBe("white");
    const card = appearanceRows({ ...defaults, surfacePalette: "white" })[0]!;
    expect(card.options.map((option) => `${option.name} · ${option.what}`)).toEqual([
      "paper · light · warm", "ink · dark", "white · plain light, no warmth", "system · follows macOS light or dark",
    ]);
    expect(card.chosen).toBe("white");
  });
});

describe("the surface's density", () => {
  it("should_ResolveTheLineHeightOfTheFourCombinations_When_BothAxesAreSet", () => {
    const combinations: [Axes, string][] = [
      [{ palette: "paper", density: "normal" }, "1.5"],
      [{ palette: "ink", density: "normal" }, "1.5"],
      [{ palette: "paper", density: "dense" }, "1.4"],
      [{ palette: "ink", density: "dense" }, "1.4"],
    ];
    for (const [axes, leading] of combinations) {
      expect(role("mono-ink", axes).get("line-height"), `${axes.palette}+${axes.density}`).toBe(leading);
      expect(role("mono-ink-strong", axes).get("line-height"), `${axes.palette}+${axes.density}`).toBe(leading);
    }
  });

  it("should_LeaveTheFaceAndSizeAlone_When_OnlyTheDensityChanges", () => {
    const normal = role("mono-ink", { palette: "paper", density: "normal" });
    const dense = role("mono-ink", { palette: "paper", density: "dense" });
    expect(dense.get("font-size")).toBe(normal.get("font-size"));
    expect(dense.get("font-family")).toBe(normal.get("font-family"));
    expect(dense.get("color")).toBe(normal.get("color"));
  });
});

describe("the surface's roles", () => {
  it("should_DefineEverySurfaceRole_When_TheStylesheetIsGenerated", () => {
    const named = [
      "mono-ink", "mono-dim", "mono-faint", "mono-meta", "mono-provider", "mono-param",
      "mono-literal", "mono-ref", "mono-ok", "mono-warn", "mono-bad",
      "mono-ref-strong", "mono-meta-strong", "mono-bad-strong", "mono-ink-strong",
      "surface-terminal", "surface-sunk",
      "frame-default", "frame-focus", "frame-stale", "frame-failed", "frame-pinned", "frame-live",
      "rail-default", "rail-focus", "rail-stale", "rail-failed", "rail-pinned", "rail-live", "rail-output",
      "badge-environment", "badge-revision", "badge-grant", "badge-target", "badge-traced",
      "badge-effects",
      ...Object.keys(viewTheme.roles),
      "graph-node-ok", "graph-node-running", "graph-node-stale", "graph-node-failed", "graph-node-selected",
      "series-line", "cell-action", "spacer", "option", "option-chosen", "chip-chosen",
    ];
    expect(roleIds().sort()).toEqual(named.sort());
  });

  it("should_KeepTheCellGround_When_ACellIsFocused", () => {
    // The violet boundary marks focus; the selection wash is an appearance choice in cell.css.
    for (const palette of [paper, ink]) {
      expect(role("frame-focus", palette).get("background-color")).toBe(role("frame-default", palette).get("background-color"));
      expect(role("frame-focus", palette).get("border-color")).toBe(role("frame-pinned", palette).get("border-color"));
    }
  });

  it("should_DressTheEffectsBadgeLikeTheOtherContextBadges_When_ARepeatWasForced", () => {
    // Decided as a context badge with a warning accent.
    expect(role("badge-effects", paper)).toEqual(role("badge-environment", paper));
    expect(role("badge-effects", ink).get("color")).toBe(role("mono-warn", ink).get("color"));
  });

  it("should_SetOnlyBold_When_ARoleIsOneOfTheFourStrongOnes", () => {
    for (const strong of ["mono-ref-strong", "mono-meta-strong", "mono-bad-strong", "mono-ink-strong"]) {
      const base = strong.replace("-strong", "");
      expect(role(strong, paper).get("font-weight"), strong).toBe("700");
      expect(role(base, paper).get("font-weight"), base).toBe("400");
      expect(role(strong, paper).get("color"), strong).toBe(role(base, paper).get("color"));
    }
  });

  it("should_NameAVariableAndNeverAValue_When_ARoleIsWritten", () => {
    const declarations = rolesCss.replace(/\/\*[\s\S]*?\*\//g, "");
    expect(declarations).not.toMatch(/#[0-9A-Fa-f]{3,8}\b/);
    expect(declarations).not.toMatch(/\brgba?\(/);
  });

  it("should_DrawTheRailsInTheStateColours_When_ACellIsInAState", () => {
    expect(role("rail-focus", paper).get("background-color")).toBe(role("mono-ref", paper).get("color"));
    expect(role("rail-stale", paper).get("background-color")).toBe(role("mono-warn", paper).get("color"));
    expect(role("rail-failed", paper).get("background-color")).toBe(role("mono-bad", paper).get("color"));
    expect(role("rail-live", paper).get("background-color")).toBe(role("mono-meta", paper).get("color"));
    // At rest a rail is the ground it sits on: present, and saying nothing.
    expect(role("rail-default", paper).get("background-color")).toBe(variables(paper).get("--ground"));
  });
});

describe("the surface's faces", () => {
  it("should_PreferTheInstalledFaceAndShipOne_When_DeclaringPTMono", () => {
    expect(fontsCss).toMatch(/font-family: "PT Mono";/);
    expect(fontsCss).toMatch(/src: local\("PT Mono"\), url\("\.\/fonts\/pt-mono-latin\.woff2"\) format\("woff2"\);/);
    expect(fontsCss).toMatch(/src: local\("IBM Plex Sans"\), url\("\.\/fonts\/ibm-plex-sans-latin\.woff2"\) format\("woff2"\);/);
  });

  it("ships PT Mono font subsets covering every Turkish letter", () => {
    const faces = [...fontsCss.matchAll(/@font-face\s*\{([^}]+)\}/g)].map(match => match[1]!)
      .filter(face => face.includes('font-family: "PT Mono";') && face.includes('url('));
    expect(faces.some(face => face.includes('pt-mono-latin-ext.woff2'))).toBe(true);
    const ranges = faces.flatMap(face => [...face.matchAll(/U\+([0-9A-F]+)(?:-([0-9A-F]+))?/g)]
      .map(match => [parseInt(match[1]!, 16), parseInt(match[2] ?? match[1]!, 16)]));
    for (const letter of "ĞğİıŞşÇçÖöÜü") {
      const point = letter.codePointAt(0)!;
      expect(ranges.some(([start, end]) => point >= start! && point <= end!), letter).toBe(true);
    }
  });

  it("should_FallThroughToAnotherMonospace_When_PTMonoIsMissing", () => {
    expect(variables(paper).get("--type-mono-family"))
      .toBe('"PT Mono", "JetBrains Mono", ui-monospace, Menlo, monospace');
    expect(variables(paper).get("--type-sans-family"))
      .toBe('"IBM Plex Sans", ui-sans-serif, system-ui, sans-serif');
  });

  /* Apple's, installed with macOS and not ours to ship, so `local()` is the whole of its src. */
  it("should_AskTheMachineAndShipNothing_When_DeclaringSFMono", () => {
    expect(fontsCss).toMatch(/font-family: "SF Mono";/);
    expect(fontsCss).toMatch(/src: local\("SF Mono"\), local\("SFMono-Regular"\);/);
    expect(fontsCss.split('font-family: "SF Mono"')[1]).not.toContain("url(");
  });

  /*
   * The same grid whichever face leads.
   *
   * A mono line's whole claim is that the column a character sits in is the column it means, so a
   * face somebody chose but does not have must land on the face they would have had — not on
   * whatever the browser happens to call monospace. Every chain is the design system's own, with
   * the chosen face lifted to its head.
   */
  it("should_LeadTheSystemsOwnChain_When_AnyFaceIsChosen", () => {
    const chain = variables(paper).get("--type-mono-family")!;
    expect(monoFamily("PT Mono")).toBe(chain);
    for (const face of SURFACE_FACES) {
      const chosen = monoFamily(face);
      expect(chosen, face).toMatch(new RegExp(`^"${face}", `));
      expect(chosen.split(", ").slice(1).join(", "), face).toBe(
        chain.split(", ").filter((it) => it !== `"${face}"`).join(", "),
      );
      expect(chosen, face).toMatch(/, monospace$/);
    }
  });

  it("should_SetTheSurfacesMonoFamily_When_TheFaceIsSwitched", () => {
    for (const face of SURFACE_FACES) {
      const settings = restoreSettings({ ...defaults, surfaceFace: face });
      expect(settings.surfaceFace).toBe(face);
      expect(monoFamily(settings.surfaceFace)).toMatch(new RegExp(`^"${face}", `));
    }
    // Any installed family may lead; the command reads a name and an optional density.
    expect(faceChosen('"SF Mono"')).toEqual({ face: "SF Mono" });
    expect(faceChosen('"PT Mono" dense')).toEqual({ face: "PT Mono", density: "dense" });
    expect(faceChosen("Menlo normal")).toEqual({ face: "Menlo", density: "normal" });
    expect(faceChosen("PT Mono 13 dense")).toEqual({ face: "PT Mono", density: "dense" });
    expect(restoreSettings({ ...defaults, surfaceFace: "Menlo" }).surfaceFace).toBe("Menlo");
  });

  /** A stored or typed name can only ever select a font: CSS in it is refused, not written. */
  it("should_RefuseANameThatIsNotAFamily_When_AFaceIsStoredOrTyped", () => {
    for (const hostile of ['Menlo"; color: red', "a{b}", "x;y", "back\\slash", "", " padded", "x".repeat(101)]) {
      expect(restoreSettings({ ...defaults, surfaceFace: hostile, surfaceSansFace: hostile }), hostile)
        .toMatchObject({ surfaceFace: "PT Mono", surfaceSansFace: "IBM Plex Sans" });
      if (hostile !== " padded") expect(faceChosen(hostile).face, hostile).toBeUndefined();
    }
  });

  it("should_CarryDataInMonoAndChromeInSans_When_ARoleChoosesAFace", () => {
    expect(role("mono-ink", paper).get("font-family")).toContain("PT Mono");
    expect(role("table-value", paper).get("font-family")).toContain("PT Mono");
    expect(role("screen-title", paper).get("font-family")).toContain("IBM Plex Sans");
    expect(role("screen-label", paper).get("font-family")).toContain("IBM Plex Sans");
    expect(role("badge-environment", paper).get("font-family")).toContain("IBM Plex Sans");
  });
});

describe("the chosen faces", () => {
  it("should_ReachEveryFamilyToken_When_DataAndInterfaceFacesAreChosen", () => {
    // Arrange: every family token the design system defines, mono and sans, bold included.
    const css = readFileSync(new URL("./tokens.css", import.meta.url), "utf8");
    const families = [...new Set([...css.matchAll(/(--[\w-]*family):/g)].map((match) => match[1]!))];
    const mono = families.filter((token) => token.startsWith("--type-mono")).sort();
    const sans = families.filter((token) => !token.startsWith("--type-mono")).sort();
    // Act
    const style = surfaceTypeStyle({ surfaceFace: "Menlo", surfaceSansFace: "Avenir Next" });
    // Assert: a token the settings do not write would keep part of the surface in the default face.
    expect(Object.keys(style).filter((token) => style[token] === monoFamily("Menlo")).sort()).toEqual(mono);
    expect(Object.keys(style).filter((token) => style[token] === sansFamily("Avenir Next")).sort()).toEqual(sans);
    expect(Object.keys(style)).toHaveLength(mono.length + sans.length);
  });

  it("should_LeadTheSystemsOwnSansChain_When_AnInterfaceFaceIsChosen", () => {
    const chain = variables(paper).get("--type-sans-family")!;
    expect(sansFamily("IBM Plex Sans")).toBe(chain);
    expect(sansFamily("Avenir Next")).toBe(`"Avenir Next", ${chain}`);
  });
});
