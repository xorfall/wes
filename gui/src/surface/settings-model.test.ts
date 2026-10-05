import { afterEach, describe, expect, it, vi } from "vitest";
import { defaults } from "../settings";
import { emptyWorkspace, type Workspace } from "../workspace";
import { lineText } from "./MonoLine";
import { read, words } from "./commands";
import { appearanceRows, chose, previewOf, readSection, SECTIONS, sectionNamed } from "./settings-model";
import { readLanguage } from "./language";
import bundled from "./language-default.json";
import type { LanguagePackage } from "./language";

const workspace: Workspace = {
  ...emptyWorkspace,
  catalogue: {
    commands: [], annotations: [],
    providers: [
      { name: "sh", capabilities: [{ path: ["run"], summary: "", result: "", safe: false, parameters: [] }], credentials: [], ready: true },
      {
        name: "acme", capabilities: [], ready: false,
        credentials: [{ name: "key", supplied: true }, { name: "secret", supplied: false }],
      },
    ],
  },
  keeping: { automatic: true, under: 10 * 1024 * 1024 },
};

const row = (key: string) => appearanceRows(defaults).find((it) => it.key === key)!;

describe("a fact's column", () => {
  it("should_KeepTwoSpacesBeforeTheValue_When_TheNameOutgrowsTheColumn", () => {
    const line = readSection("keys", { settings: defaults, workspace: emptyWorkspace }).facts.find((it) => it[1]?.text === "the screens")!;
    expect(line[0]!.text).toBe("/graph /stale /env /settings /open  ");
    const short = readSection("keys", { settings: defaults, workspace: emptyWorkspace }).facts.find((it) => it[1]?.text === "focused cell: cancel what is running")!;
    expect(short[0]!.text).toHaveLength(16);
  });
});

describe("the keys section", () => {
  afterEach(() => vi.unstubAllGlobals());
  const said = () => readSection("keys", { settings: defaults, workspace: emptyWorkspace }).facts.map((it) => `${it[0]!.text.trim()} ${it[1]!.text}`);

  /** `p` pins the cell in the scrollback; a view's Pin input is a different control, and the line must not blur them. */
  it("should_SayWhoseKeyItIs_When_PromptAndCellKeysAreListed", () => {
    const pin = said().find((it) => it.startsWith("r  p  o"))!;
    expect(pin).toBe("r  p  o focused cell: repeat, pin the cell in the scrollback, inspect");
    expect(pin).not.toMatch(/input|result/);
    expect(said().filter((it) => /^(↑ ↓|⇥|⌃space|⇧⏎) /.test(it)).every((it) => it.includes(" prompt: "))).toBe(true);
    expect(said().filter((it) => /^(j  k|r  p  o|v  d|x) /.test(it)).every((it) => it.includes(" focused cell: "))).toBe(true);
  });

  it.each([["MacIntel", "⌘"], ["Win32", "⌃"], ["Linux x86_64", "⌃"]])("should_NameThePrimaryModifier_When_ThePlatformIs %s", (platform, glyph) => {
    vi.stubGlobal("navigator", { platform });
    expect(said()).toContain(`${glyph}⇧⏎ prompt: open the draft in the editor`);
    expect(said()).toContain(`${glyph}⏎ prompt: run, without accepting a completion`);
    expect(said()).toContain(`${glyph}M focused cell: its actions menu`);
    expect(said()).toContain("⌃⇧C prompt: toggle action label style");
    expect(said().join("\n")).not.toMatch(/^⌃c /m);
    expect(appearanceRows(defaults).find((it) => it.key === "tail")!.options.map((it) => it.what)).toContain(`hidden; ${glyph}M opens cell actions`);
  });
});

describe("the appearance section", () => {
  it("should_ShowTheThreeAppearanceRows_When_ItIsRead", () => {
    expect(appearanceRows(defaults).map((it) => it.label)).toEqual(["Palette", "Cell actions", "Action footers", "Focused cell", "Density", "Data font", "Interface font"]);
    expect(appearanceRows(defaults).map((it) => it.kind ?? "choice")).toEqual(["choice", "choice", "choice", "choice", "choice", "font", "font"]);
  });

  it("should_ShowACommandThatTheSurfaceAnswers_When_EachRowIsDrawn", () => {
    for (const it of appearanceRows(defaults)) {
      expect(read(it.command).kind, `${it.label}: ${it.command}`).not.toBe("trouble");
    }
  });

  it("should_MarkWhatIsAlreadyChosen_When_TheSettingsAreRead", () => {
    expect(row("palette").chosen).toBe("system");
    expect(row("chrome").chosen).toBe("keys");
    expect(row("tail").chosen).toBe("shown");
    expect(chose(defaults, row("tail"), { name: "hidden", what: "" }).surfaceTailKeys).toBe("hidden");
    expect(row("focus").chosen).toBe("outline");
    expect(chose(defaults, row("focus"), { name: "wash", what: "" }).surfaceFocus).toBe("wash");
    expect(chose({ ...defaults, surfaceFocus: "wash" }, row("focus"), { name: "outline", what: "" }).surfaceFocus).toBe("outline");
    expect(row("face").chosen).toBe("PT Mono");
    expect(row("sansFace").chosen).toBe("IBM Plex Sans");
    expect(appearanceRows({ ...defaults, surfaceDensity: "dense" }).find((it) => it.key === "density")?.chosen).toBe("dense");
  });

  it("should_WriteTheSetting_When_ACardIsChosen", () => {
    expect(chose(defaults, row("palette"), { name: "ink", what: "" }).surfacePalette).toBe("ink");
    expect(chose(defaults, row("chrome"), { name: "controls", what: "" }).surfaceChrome).toBe("controls");
    // Density is its own choice now, for any face.
    const faced = chose({ ...defaults, surfaceDensity: "dense" }, row("face"), { name: "Menlo", what: "" });
    expect(faced).toMatchObject({ surfaceFace: "Menlo", surfaceDensity: "dense" });
    expect(chose(defaults, row("density"), { name: "dense", what: "" }).surfaceDensity).toBe("dense");
    expect(chose(defaults, row("sansFace"), { name: "Avenir Next", what: "" }).surfaceSansFace).toBe("Avenir Next");
    expect(chose(defaults, row("face"), { name: 'x"; color: red', what: "" })).toEqual(defaults);
  });

  it("should_ChangeNothing_When_TheRowIsNotOneItKnows", () => {
    expect(chose(defaults, { label: "?", command: "?", chosen: "", options: [] }, { name: "x", what: "" }))
      .toEqual(defaults);
  });

  it("should_DrawThePreviewInWhatIsChosen_When_ItIsWritten", () => {
    expect(lineText(previewOf(defaults))).toContain("PT Mono 13 / 1.62 · IBM Plex Sans");
    expect(lineText(previewOf({ ...defaults, surfaceDensity: "dense", surfaceFace: "JetBrains Mono" })))
      .toContain("JetBrains Mono 13 / 1.4");
  });
});

describe("the other six sections", () => {
  it("should_IncludeDescribeAlongsideExistingSections", () => {
    expect(SECTIONS).toEqual(["appearance", "editor", "results", "keys", "connections", "aliases", "data", "limits"]);
  });

  it("should_FallBackToAppearance_When_TheWordIsNotASection", () => {
    expect(sectionNamed("connections")).toBe("connections");
    expect(sectionNamed("nonsense")).toBe("appearance");
    expect(sectionNamed(undefined)).toBe("appearance");
  });

  /** Appearance chooses the surface's own look; results chooses where `/open` puts a result. */
  it("should_OfferNoChoices_When_TheSectionChoosesNothing", () => {
    for (const section of SECTIONS.filter((it) => it !== "appearance" && it !== "results")) {
      expect(readSection(section, { settings: defaults, workspace }).rows).toEqual([]);
    }
  });

  it("should_OfferAWindowOrAScreen_When_TheResultsSectionIsRead", () => {
    const row = readSection("results", { settings: defaults, workspace }).rows[0];
    expect(row?.label).toBe("/open");
    expect(row?.command).toBe("/settings results open window");
    expect(row?.options.map((option) => option.name)).toEqual(["window", "screen"]);
    expect(row?.chosen).toBe(defaults.openIn);
    expect(chose(defaults, row!, { name: "window", what: "" }).openIn).toBe("window");
    expect(chose(defaults, row!, { name: "screen", what: "" }).openIn).toBe("screen");
  });

  it("should_SayWhatTheEngineSentAboutTheLanguage_When_ThePackageHasArrived", () => {
    const pack = readLanguage(bundled as LanguagePackage, "engine");
    const said = readSection("editor", { settings: defaults, workspace, pack }).facts.map(lineText);
    expect(said[0]).toContain("calc · version 1");
    expect(said[1]).toContain("the engine");
    expect(said.some((line) => line.startsWith("statements"))).toBe(true);
  });

  it("should_SayItIsWaiting_When_ThePackageHasNotArrived", () => {
    const view = readSection("editor", { settings: defaults, workspace });
    expect(view.facts).toEqual([]);
    expect(view.empty).toBe("waiting for the engine's language package");
  });

  it("should_CountEachProvidersCredentials_When_ConnectionsIsRead", () => {
    const said = readSection("connections", { settings: defaults, workspace }).facts.map(lineText);
    expect(said[0]).toContain("1 capabilities · no credentials wanted");
    expect(said[1]).toContain("1 of 2 credentials");
  });

  it("should_SayNoProviders_When_TheCatalogueIsEmpty", () => {
    expect(readSection("connections", { settings: defaults, workspace: emptyWorkspace }).empty)
      .toBe("no providers are configured yet");
  });

  it("should_SayNothingToSetYet_When_ThereAreNoAliases", () => {
    expect(readSection("aliases", { settings: defaults, workspace }).empty).toBe("nothing to set yet");
  });

  it("should_ListEachAlias_When_SomeAreDefined", () => {
    const aliased = { ...defaults, aliases: { o: "acme orders.list" } };
    expect(readSection("aliases", { settings: aliased, workspace }).facts.map(lineText)[0])
      .toContain("acme orders.list");
  });

  it("should_SayWhatIsBeingKept_When_DataIsRead", () => {
    const said = readSection("data", { settings: defaults, workspace }).facts.map(lineText);
    expect(said[0]).toContain("results under a size, automatically");
    expect(said[1]).toContain("10.0 MB");
  });
});

describe("the surface's own commands", () => {
  it("should_KeepAQuotedRunTogether_When_TheLineIsSplit", () => {
    expect(words('/theme font "PT Mono" dense')).toEqual(["/theme", "font", "PT Mono", "dense"]);
  });

  it("should_SummonAScreen_When_ItIsNamed", () => {
    expect(read("/graph")).toEqual({ kind: "screen", screen: "graph" });
    expect(read("/settings connections")).toEqual({ kind: "screen", screen: "settings", section: "connections" });
  });

  it("should_LeaveTheEnginesCommandsAlone_When_TheyAreNotClientOnes", () => {
    expect(read("acme orders.list").kind).toBe("engine");
    expect(read(":list sh").kind).toBe("engine");
  });

  it("should_SetThePalette_When_TheThemeCommandNamesOne", () => {
    const changed = (line: string) => {
      const typed = read(line);
      if (typed.kind !== "settings") throw new Error(`${line} was ${typed.kind}`);
      return typed.change(defaults);
    };
    expect(changed("/theme ink").surfacePalette).toBe("ink");
    expect(changed("/theme paper").surfacePalette).toBe("paper");
    expect(changed("/theme system").surfacePalette).toBe("system");
    expect(changed("/theme").surfacePalette).toBe("ink");
    expect(changed("/theme cell controls").surfaceChrome).toBe("controls");
    expect(changed("/theme keys hidden").surfaceTailKeys).toBe("hidden");
    expect(changed("/theme keys shown").surfaceTailKeys).toBe("shown");
    expect(changed("/theme focus wash").surfaceFocus).toBe("wash");
    expect(changed("/theme focus outline").surfaceFocus).toBe("outline");
    expect(changed('/theme font "JetBrains Mono"').surfaceFace).toBe("JetBrains Mono");
    expect(changed("/layout tb").direction).toBe("TB");
    expect(changed("/follow off").stayAtNewest).toBe(false);
    expect(changed("/follow").stayAtNewest).toBe(false);
  });

  it("should_SayWhatItTakes_When_TheArgumentIsWrong", () => {
    expect(read("/theme cell fancy")).toMatchObject({ kind: "trouble" });
    expect(read("/theme focus violet")).toMatchObject({ kind: "trouble", said: "'/theme focus' takes outline or wash" });
    expect(read("/layout sideways")).toMatchObject({ kind: "trouble" });
    expect(read("/follow maybe")).toMatchObject({ kind: "trouble" });
    expect(read("/nonsense")).toMatchObject({ kind: "trouble" });
  });
});
