import { describe, expect, it } from "vitest";
import { complete, foreignAt, foreignCompletion, wantsSuggestions } from "./complete";
import { expand, expandCaret, focusOn, type Focus } from "./focus";
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
  ],
};

const shell: Catalogue = {
  ...emptyCatalogue,
  providers: [
    {
      name: "sh",
      credentials: [],
      ready: true,
      capabilities: [
        {
          path: ["run"],
          summary: "",
          result: "ProcessOutput",
          safe: false,
          parameters: [
            { name: "cmd", type: "Text", required: false, allowed: [], content: "sh" },
            { name: "timeout", type: "Duration", required: false, allowed: [], content: "" },
          ],
        },
      ],
    },
  ],
};

const offered = (line: string) =>
  complete(line, line.length, catalogue, []).items.map((item) => item.text);

describe("completing a meta command", () => {
  /** Until the vocabulary carried them, ':list ' offered 'provider:' and never the thing to list. */
  it("should_OfferTheWordsItTakes_When_NoneHasBeenWritten", () => {
    expect(offered(":list ")).toContain("capabilities");
    expect(offered(":list ")).toContain("nodes");
  });

  it("should_NarrowThem_When_SomeOfTheWordIsWritten", () => {
    expect(offered(":list c")).toEqual(["capabilities", "commands"]);
  });

  /** ':list' takes exactly one; offering a second would be offering something that fails the check. */
  it("should_StopOfferingThem_When_OneIsAlreadyWritten", () => {
    expect(offered(":list nodes ")).not.toContain("capabilities");
  });

  it("should_StillOfferTheArguments_When_TheWordIsWritten", () => {
    expect(offered(":list nodes ")).toContain("provider:");
  });

  it("should_OfferBoth_When_NothingIsWrittenYet", () => {
    expect(offered(":list ")).toContain("provider:");
  });

});

/**
 * The claim the whole mode rests on: locked to a template, you still reach everything. Completion runs
 * on the expanded line, so what it sees is a real command rather than the fragment in the box.
 */
describe("completing inside a template", () => {
  const focused = (template: string): Focus => focusOn(template) as Focus;

  const inside = (template: string, typed: string) => {
    const on = focused(template);
    return complete(expand(on, typed), expandCaret(on, typed.length), catalogue, ["bars"])
      .items.map((item) => item.text);
  };

  it("should_StillOfferTheCommands_When_TheTemplateAppends", () => {
    expect(inside("sh run", ":li")).toContain(":list");
  });

  it("should_StillOfferTheWords_When_TheTemplateEndsAtACommand", () => {
    expect(inside(":list", "prov")).toContain("providers");
  });

  /** Result references remain available inside an argument placeholder. */
  it("should_StillOfferTheNames_When_TheHoleIsAfterAKey", () => {
    expect(inside(":list _", "$ba")).toContain("$bars");
  });

  it("should_SeeTheWholeCommand_When_TheHoleIsMidWord", () => {
    const on = focused(":list nodes provider:_");
    const done = complete(expand(on, "$ba"), expandCaret(on, 3), catalogue, ["bars"]);

    expect(done.items.map((item) => item.text)).toContain("provider:$bars");
    expect(done.from).toBeLessThan(on.hole);
  });
});

/**
 * Knowing when to stop. wes's completion completes wes and ends at the quote; what is past it belongs
 * to another language and to whoever can read the machine the engine runs on.
 */
describe("a value written in another language", () => {
  const at = (line: string, caret = line.length) => foreignAt(line, caret, shell);

  it("should_SayWhatToAskAbout_When_TheCaretIsInsideIt", () => {
    expect(at('sh run cmd:"cat /usr/lo')).toEqual({
      language: "sh",
      written: "cat /usr/lo",
      caret: 11,
      from: 12,
      offsets: Array.from({ length: 12 }, (_, i) => i),
    });
  });

  it("should_SayNothing_When_TheParameterIsOnlyAValue", () => {
    expect(at('sh run timeout:"PT5')).toBeUndefined();
  });

  it("should_SayNothing_When_TheCaretIsInWesSOwnLanguage", () => {
    expect(at("sh run cm")).toBeUndefined();
    expect(at("lab-sensors re")).toBeUndefined();
  });

  /** An unquoted value ends at the first space, which is not a shell line. */
  it("should_SayNothing_When_TheValueIsNotQuoted", () => {
    expect(at("sh run cmd:ls")).toBeUndefined();
  });

  it("should_CountFromInsideTheQuote_When_TheCaretIsPartWayIn", () => {
    const found = at('sh run cmd:"ls -la"', 15);

    expect(found?.written).toBe("ls ");
    expect(found?.caret).toBe(3);
  });

  it("should_SayNothing_When_TheProviderIsNotKnown", () => {
    expect(at('nope run cmd:"ls')).toBeUndefined();
  });
});

/**
 * When a list is worth showing. The rule is about what somebody wrote, so it is measured on what they
 * wrote — not on the whole command, which under a template holds a colon nobody typed.
 */
describe("whether there is a question yet", () => {
  it("should_SayNo_When_NothingHasBeenTyped", () => {
    expect(wantsSuggestions("", 0)).toBe(false);
  });

  /** `ls` and `cd` are whole commands somebody meant to finish; two letters was one too few. */
  it("should_SayNo_When_TheWordIsStillAWholeCommand", () => {
    expect(wantsSuggestions("m", 1)).toBe(false);
    expect(wantsSuggestions("ls", 2)).toBe(false);
    expect(wantsSuggestions("cd", 2)).toBe(false);
  });

  it("should_SayYes_When_ThreeLettersAreThere", () => {
    expect(wantsSuggestions("mar", 3)).toBe(true);
    expect(wantsSuggestions("lsi", 3)).toBe(true);
  });

  /** A marker is a question on its own: it says which namespace and stops. */
  it("should_SayYesAtOnce_When_AMarkerWasWritten", () => {
    for (const marker of [":", "@", "$", "/"]) {
      expect(wantsSuggestions(marker, 1)).toBe(true);
    }
  });

  it("should_SayYesAtOnce_When_AKeyWasWritten", () => {
    expect(wantsSuggestions("symbol:", 7)).toBe(true);
  });

  /** Only the word the caret is in counts; an earlier long word is not this word. */
  it("should_SayNo_When_TheWordAtTheCaretIsShort", () => {
    expect(wantsSuggestions("lab-sensors re", 14)).toBe(false);
  });

  /**
   * The bug this moved for. Under '/focus sh run cmd:"_"' the expanded line holds a colon before a key
   * is pressed, and measuring there opened the list on an empty box.
   */
  it("should_SayNo_When_TheColonCameFromATemplate", () => {
    const template = 'sh run cmd:"_"';
    const on = focusOn(template) as Focus;

    expect(wantsSuggestions("", 0)).toBe(false);
    expect(wantsSuggestions(expand(on, ""), expandCaret(on, 0))).toBe(true);
  });
});

it("decodes outer escapes and maps shell insertion back through escaped text and emoji", () => {
  const line = String.raw`sh run cmd:"echo \"😀\" \\ path`;
  const foreign = foreignAt(line, line.length, shell)!;
  expect(foreign.written).toBe('echo "😀" \\ path');
  expect(foreign.caret).toBe(foreign.written.length);
  const text = String.raw`'path with'\''quote"'`;
  const answer = foreignCompletion(foreign, {
    from: foreign.written.indexOf("path"), items: [{ text, kind: "file" }],
  });
  expect(answer.from).toBe(line.indexOf("path"));
  const inserted = line.slice(0, answer.from) + answer.items[0]!.text;
  expect(foreignAt(inserted, inserted.length, shell)?.written).toBe('echo "😀" \\ ' + text);
  expect(foreignCompletion(foreign, { from: 7, items: [{ text: "bad", kind: "file" }] }).items).toEqual([]);
  expect(foreignCompletion(foreign, { from: -1, items: [] }).items).toEqual([]);
});

it("does not ask for inner completion after a closing quote or across an unfinished or invalid escape", () => {
  for (const line of ['sh run cmd:"echo"', String.raw`sh run cmd:"echo \q`, 'sh run cmd:"echo \\']) {
    expect(foreignAt(line, line.length, shell)).toBeUndefined();
  }
});

it("round trips newline, carriage return and tab escapes in inner-language completion", () => {
  const line = String.raw`sh run cmd:"echo\n\t`;
  const foreign = foreignAt(line, line.length, shell)!;
  expect(foreign.written).toBe("echo\n\t");
  const completion = foreignCompletion(foreign, { from: foreign.caret, items: [{ text: "İstanbul\r\n", kind: "file" }] });
  const inserted = line.slice(0, completion.from) + completion.items[0]!.text;
  expect(foreignAt(inserted, inserted.length, shell)?.written).toBe("echo\n\tİstanbul\r\n");
});

it("offers the complete HTTP annotation and continues provider completion after it", () => {
  const vocabulary = { ...shell, annotations: ["trace", "unchecked"] };
  expect(complete("@tr", 3, vocabulary, []).items.map(i => i.text)).toEqual(["@trace(http)"]);
  const line = "@trace(http) sh ";
  expect(complete(line, line.length, vocabulary, []).items.map(i => i.text)).toContain("run");
});

describe("advertised subcommand parameters", () => {
  const parameter = (name: string) => ({ name, type: "Text", required: false, allowed: [], content: "" });
  const commands: Catalogue = { ...emptyCatalogue, commands: [
    { name: "import", implemented: true, summary: "", takes: ["spec", "process"], open: true,
      parameters: [parameter("as")], variants: [
        { word: "spec", parameters: ["as", "file", "url", "endpoint"].map(parameter), takes: [], variants: [] },
        { word: "process", parameters: ["as", "bin"].map(parameter), takes: [], variants: [] },
      ] },
    { name: "package", implemented: true, summary: "", takes: ["load"], open: false, parameters: [], variants: [{word:"load", parameters:[parameter("path")], takes: [], variants: []}] },
    { name: "type", implemented: true, summary: "", takes: ["check"], open: true,
      parameters: [], variants: [
        { word: "check", parameters: [parameter("as")], takes: [], variants: [] },
      ] },
  ] };
  const offer = (line: string) => complete(line, line.length, commands, []).items.map(item => item.text);
  it("selects the advertised subcommand including after leading arguments and annotations", () => {
    expect(offer(":import spec f")).toEqual(["file:"]);
    expect(offer(":import spec ")).toEqual(["as:", "file:", "url:", "endpoint:"]);
    expect(offer(":import process ")).toEqual(["as:", "bin:"]);
    expect(offer(':import as:"demo" spec f')).toEqual(["file:"]);
    expect(offer("@trace(http) :import spec end")).toEqual(["endpoint:"]);
    expect(offer(":package load ")).toEqual(["path:"]);
    expect(offer(":type check ")).toEqual(["as:"]);
  });
  it("does not mistake text inside quoted values for argument keys or subcommands", () => {
    expect(offer(':import spec file:"path with as: text" ')).toEqual(["as:", "url:", "endpoint:"]);
    expect(offer(':import as:"demo spec" process ')).toEqual(["bin:"]);
    expect(offer(':import spec\nfi')).toEqual(["file:"]);
    expect(offer(':import spec file:"escaped \\" as: name" end')).toEqual(["endpoint:"]);
  });
  it("keeps the three-character rule and supports explicit short or empty completion", () => {
    expect(wantsSuggestions("fi", 2)).toBe(false);
    expect(wantsSuggestions("fil", 3)).toBe(true);
    expect(offer(":import spec ")).toContain("file:");
    expect(complete("", 0, commands, []).items.map(item => item.text)).toContain("import");
  });
});

describe("completion insertion separators", () => {
  it("marks ordinary commands and subcommands, but leaves parameter keys attached", () => {
    for (const line of [":li", ":list prov"]) {
      expect(complete(line, line.length, catalogue, []).items[0]?.separate).toBe(true);
    }
    for (const line of ["s", "sh r"]) {
      expect(complete(line, line.length, shell, []).items[0]?.separate).toBe(true);
    }
    const line = ":list nodes pro";
    const parameter = complete(line, line.length, catalogue, []).items[0];
    expect(parameter?.text).toBe("provider:");
    expect(parameter?.separate).toBeUndefined();
  });
  it("does not insert command separators into calculation string literals or references", () => {
    for (const line of [':calc { call("s', ':calc { call("sh", ["r', '$pri']) {
      const items = complete(line, line.length, shell, ['prices']).items;
      expect(items.length).toBeGreaterThan(0);
      expect(items.every(item => !item.separate)).toBe(true);
    }
  });
});

describe("nested command variants", () => {
  const parameter = (name: string) => ({ name, type: name === "replace" ? "Bool" : "Text", required: false, allowed: [], content: "" });
  const leaf = (word: string, names: readonly string[]) => ({ word, parameters: names.map(parameter), takes: [], variants: [] });
  // Shaped like the engine's vocabulary: importer kinds and plan/apply at the root, plan taking kinds again.
  const commands: Catalogue = { ...emptyCatalogue, commands: [
    { name: "import", implemented: true, summary: "Import a provider", takes: ["spec", "process", "plan", "apply"], open: true, parameters: [], variants: [
      leaf("spec", ["as", "replace", "file", "url", "endpoint"]),
      leaf("process", ["as", "replace", "bin"]),
      { word: "plan", parameters: [], takes: ["spec", "process"], variants: [leaf("spec", ["as", "file", "url", "endpoint"]), leaf("process", ["as", "bin"])] },
      leaf("apply", ["replace"]),
    ] },
  ] };
  const offer = (line: string) => complete(line, line.length, commands, []).items.map(item => item.text);

  it("should_OfferImporterKindsAndPlanApply_When_OnlyTheRootIsWritten", () => {
    // Arrange
    const line = ":import ";
    // Act
    const offered = offer(line);
    // Assert
    expect(offered).toEqual(["spec", "process", "plan", "apply"]);
  });

  it("should_FollowThePathSequentially_When_NestedVariantsAreWritten", () => {
    // Arrange
    const lines = [":import plan ", ":import plan spec ", ":import plan process ", ":import apply ", ":import spec "];
    // Act
    const [plan, planSpec, planProcess, apply, spec] = lines.map(offer);
    // Assert
    expect(plan).toEqual(["spec", "process"]);
    expect(planSpec).toEqual(["as:", "file:", "url:", "endpoint:"]);
    expect(planSpec).not.toContain("replace:");
    expect(planProcess).toEqual(["as:", "bin:"]);
    expect(apply).toEqual(["replace:"]);
    expect(spec).toEqual(["as:", "replace:", "file:", "url:", "endpoint:"]);
  });

  it("should_NotSelectASubcommand_When_ItsWordOnlyAppearsInsideAValue", () => {
    // Arrange
    const lines = [':import file:"plan" ', ':import spec file:"plan spec" ', ":import plan spec url:apply "];
    // Act
    const [root, spec, planSpec] = lines.map(offer);
    // Assert
    expect(root).toEqual(["spec", "process", "plan", "apply"]);
    expect(spec).toEqual(["as:", "replace:", "url:", "endpoint:"]);
    expect(planSpec).toEqual(["as:", "file:", "endpoint:"]);
  });

  it("should_StopOfferingSubcommands_When_AFreeWordEndsThePath", () => {
    // Arrange
    const line = ":import plan unknown ";
    // Act
    const offered = offer(line);
    // Assert
    expect(offered).toEqual([]);
  });
});
