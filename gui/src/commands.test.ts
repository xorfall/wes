import { describe, expect, it } from "vitest";
import { clientCommandNames, run } from "./commands";
import { defaults } from "./settings";
import { emptyCatalogue, type Catalogue } from "./vocabulary";

const catalogue: Catalogue = {
  ...emptyCatalogue,
  commands: [
    {
      name: "list",
      implemented: true,
      summary: "lists what the engine knows about",
      takes: ["providers", "capabilities", "nodes", "importers", "commands", "workspaces"],
      open: false,
      parameters: [{ name: "provider", type: "Text", required: false, allowed: [], content: "" }],
      variants: [],
    },
    { name: "def", implemented: false, summary: "reserved, not implemented yet", takes: [], open: false, parameters: [], variants: [] },
  ],
};

describe("commands the client answers itself", () => {
  /** It was drawn as one collapsed paragraph, every command running into the next one's description. */
  it("should_PutOneCommandOnEachLine_When_AskedForHelp", () => {
    const lines = (run("/help", defaults).said[0]?.message ?? "").split("\n");

    expect(lines).toHaveLength(clientCommandNames().length);
    expect(lines[0]).toMatch(/^\/help {2,}what you are reading/);
  });

  /** A ragged second column is a column the eye cannot find. */
  it("should_LineTheDescriptionsUp_When_AskedForHelp", () => {
    const lines = (run("/help", defaults).said[0]?.message ?? "").split("\n");
    const starts = lines.map((line) => line.search(/\S.*?\s\s+\S/) >= 0 ? line.indexOf(line.trimStart().split(/\s\s+/)[1] ?? "") : -1);

    expect(new Set(starts).size).toBe(1);
  });

  /** Help that lists something else than what exists is worse than no help. */
  it("should_ListEveryCommandItAnswers_When_AskedForHelp", () => {
    const message = run("/help", defaults).said[0]?.message ?? "";

    for (const name of clientCommandNames()) {
      expect(message).toContain(`/${name}`);
    }
  });

  it("should_AskTheEngineFirst_When_AskedToDebug", () => {
    expect(run("/debug", defaults).askStorage).toBe(true);
  });

  /** It used to answer with the name of the field it set: "followNew: off". */
  it("should_SayWhatItDoes_When_FollowIsSwitched", () => {
    const off = run("/follow off", defaults);

    expect(off.settings?.stayAtNewest).toBe(false);
    expect(off.said[0]?.message).toBe("follow: off — the scrollback stays where you left it");
  });

  it("should_SwitchIt_When_FollowIsGivenNoArgument", () => {
    expect(run("/follow", { ...defaults, stayAtNewest: true }).settings?.stayAtNewest).toBe(false);
    expect(run("/follow", { ...defaults, stayAtNewest: false }).settings?.stayAtNewest).toBe(true);
  });

  it("should_RefuseAndSayWhatItTakes_When_FollowIsGivenSomethingElse", () => {
    const said = run("/follow maybe", defaults);

    expect(said.settings).toBeUndefined();
    expect(said.said[0]?.severity).toBe("error");
  });

  it("should_SayThereIsNoSuchOne_When_TheCommandIsNotItsOwn", () => {
    const said = run("/nonsense", defaults);

    expect(said.said[0]?.severity).toBe("error");
    expect(said.said[0]?.message).toContain("'/nonsense'");
  });

  /** It used to take an argument and ignore it: '/help list' gave the general list and no sign why. */
  it("should_AnswerAboutThatOne_When_HelpIsGivenAClientCommand", () => {
    const said = run("/help follow", defaults).said[0]?.message ?? "";

    expect(said).toContain("/follow");
    expect(said).toContain("newest cell");
    expect(said).not.toContain("/theme");
  });

  it("should_TakeItWithOrWithoutTheMark_When_HelpIsGivenACommandName", () => {
    expect(run("/help /follow", defaults).said[0]?.message)
      .toBe(run("/help follow", defaults).said[0]?.message);
  });

  it("should_AnswerFromTheCatalogue_When_HelpIsGivenAnEngineCommand", () => {
    const said = run("/help list", defaults, catalogue).said[0]?.message ?? "";

    expect(said).toContain(":list");
    expect(said).toContain("lists what the engine knows about");
    expect(said).toContain("takes one of: providers, capabilities, nodes");
  });

  it("should_TakeTheEngineMarkToo_When_HelpIsGivenAnEngineCommand", () => {
    expect(run("/help :list", defaults, catalogue).said[0]?.message)
      .toBe(run("/help list", defaults, catalogue).said[0]?.message);
  });

  /** ':list capabilities' is two levels deep and only the first has anything written about it. */
  it("should_SayItHasNoneOfItsOwn_When_AskedAboutAWordTheCommandTakes", () => {
    const said = run("/help list capabilities", defaults, catalogue).said[0]?.message ?? "";

    expect(said).toContain("'capabilities' has no help of its own");
    expect(said).toContain(":list capabilities");
  });

  it("should_SayItIsNotThereYet_When_AskedAboutAReservedCommand", () => {
    const said = run("/help def", defaults, catalogue).said[0]?.message ?? "";

    expect(said).toContain("reserved");
  });

  /** The engine's commands are not the client's, so the answer says where each list lives. */
  it("should_PointAtBothLists_When_TheNameIsNeither", () => {
    const said = run("/help nonsense", defaults, catalogue).said[0]?.message ?? "";

    expect(said).toContain("nothing here is called 'nonsense'");
    expect(said).toContain(":help");
    expect(said).toContain(":inspect");
  });

  it("should_StillAnswerAboutItsOwn_When_TheEngineHasNotSaidAnything", () => {
    expect(run("/help follow", defaults, undefined).said[0]?.message).toContain("/follow");
  });

  it("defines, lists and removes named aliases while preserving template whitespace", () => {
    const created = run('/alias ls = sh run cmd:"ls  _"', defaults).settings!;
    expect(created.aliases).toEqual({ ls: 'sh run cmd:"ls  _"' });
    expect(run('/alias', created).said[0]?.message).toContain('ls = sh run cmd:"ls  _"');
    expect(run('/theme paper', created).settings?.aliases).toEqual(created.aliases);
    expect(run('/unalias ls', created).settings?.aliases).toEqual({});
    expect(run('/unalias missing', created).said[0]?.severity).toBe('error');
    expect(run('/focus sh run _', created).said[0]?.severity).toBe('error');
  });
});

it("shares only the current palettes with personal client commands", () => {
  for (const surfacePalette of ["paper", "ink", "white", "system"] as const) {
    expect(run(`/theme ${surfacePalette}`, defaults).settings?.surfacePalette).toBe(surfacePalette);
  }
  expect(run("/theme", { ...defaults, surfacePalette: "ink" }).settings?.surfacePalette).toBe("paper");
  expect(run("/theme unknown", defaults).settings).toBeUndefined();
});
