import { describe, expect, it } from "vitest";
import { defaults, restoreSettings } from "../settings";
import type { StoredValue } from "../protocol";
import { newCell } from "../cells";
import { emptyWorkspace, type Workspace, type WorkspaceNode } from "../workspace";
import type { Catalogue } from "../vocabulary";
import { lineText } from "./MonoLine";
import { read } from "./commands";
import { chose, readSection } from "./settings-model";
import { openRoute, peekRoute, readOpenRoute } from "./open-route";
import {
  capabilityOf, couldNotDraw, factLines, openFacts, openJson, openSubject, readOpen, resultNamed, when,
} from "./open-model";
import { prepareSync } from "../presentation/prepare";
import { present, runText } from "../presentation/present";
import { Registry } from "../presentation/registry";

/** The window's presentation of a stored value: the same tree the result tab draws. */
const windowed = (stored: StoredValue) => present({
  prepared: prepareSync(stored), registry: Registry.core(),
  context: { mode: "window", columns: 120, lines: 4000, density: "normal", locale: "en-GB", timeZone: "UTC" },
}).root;
import type { SessionContext } from "./session-model";

const catalogue: Catalogue = {
  commands: [],
  annotations: [],
  providers: [
    {
      name: "acme",
      ready: true,
      credentials: [],
      capabilities: [
        { path: ["orders", "list"], summary: "orders", result: "List<Order>", safe: true, parameters: [] },
        { path: ["orders", "create"], summary: "makes one", result: "Order", safe: false, parameters: [] },
      ],
    },
  ],
};

const context: SessionContext = { workspace: "sales-api", connection: "connected" };

/** All supported details at once, so the mapping can be checked against one node. */
const everything: WorkspaceNode = {
  id: "id7",
  name: "orders",
  command: "acme orders.create since:2026-09-01",
  dependsOn: [],
  state: "ready",
  startedAt: "2026-09-20T09:12:00.000Z",
  run: "9a2ccb70-42ff-4aa6-be2c-67d80ef1286e",
  type: "List<Order>",
  bytes: 2048,
  handle: "h1",
  kept: true,
  retention: "automatic",
  traced: true,
  interactive: true,
  repeatable: false,
  private: true,
  provenance: {},
  cautions: ["an unchecked argument was accepted", "the revision was overridden"],
  doubt: { capability: "acme orders.create", safe: false, when: "2026-09-20T09:12:30.000Z" },
  environment: {
    event: "node-environment",
    node: "id7",
    environment: "PROD",
    revision: "sha256:0ab",
    target: "https://api.acme.invalid",
    endpoint: "https://eu.api.acme.invalid",
    origin: "redirected from DEV",
  },
};

const workspace: Workspace = {
  ...emptyWorkspace,
  catalogue,
  nodes: [everything],
  keeping: { automatic: true, under: 10 * 1024 * 1024 },
};

const cell = { ...newCell("acme orders.create since:2026-09-01"), nodes: ["id7"], acknowledgeEffects: true };
const said = (input: Parameters<typeof openFacts>[0]) =>
  openFacts(input).flatMap((group) => group.facts.map((f) => `${f.name}=${f.value}`));

describe("what /open's details tab says about a result", () => {
  it("should_GroupTheFacts_When_ANodeIsRead", () => {
    expect(openFacts({ node: everything, cell, workspace, context, client: "c1" }).map((group) => group.name))
      .toEqual(["about the command", "about the run", "about the session"]);
  });

  it("should_CarryEveryFactTheEngineSent_When_TheNodeHasThemAll", () => {
    const facts = said({ node: everything, cell, workspace, context, client: "c1" });
    expect(facts).toContain("provider=acme");
    expect(facts).toContain("capability=orders create");
    expect(facts).toContain("unsafe=this capability performs an external action");
    expect(facts).toContain("traced=the call was recorded");
    expect(facts).toContain("interactive=it can be typed at while it runs");
    expect(facts).toContain("repeatable=no — running it again acts again");
    expect(facts).toContain("effects=acknowledged for this attempt");
    expect(facts).toContain("run=9a2ccb70-42ff-4aa6-be2c-67d80ef1286e");
    expect(facts).toContain("environment=PROD");
    expect(facts).toContain("revision=sha256:0ab");
    expect(facts).toContain("target=https://api.acme.invalid");
    expect(facts).toContain("endpoint=https://eu.api.acme.invalid");
    expect(facts).toContain("origin=redirected from DEV");
    expect(facts).toContain("caution=an unchecked argument was accepted");
    expect(facts).toContain("=the revision was overridden");
    expect(facts).toContain("private=memory only; gone when the engine restarts");
    expect(facts).toContain("retention=automatic");
    expect(facts).toContain("kept=yes");
    expect(facts).toContain("result=List<Order> · 2.0 KB");
    expect(facts).toContain("memory charge=2048 bytes · bounded private memory, not an archive size");
    expect(facts).toContain("client=c1");
    expect(facts).toContain("keeping=finite results at or below the encoded storage size, automatically");
    expect(facts).toContain("workspace=sales-api");
    expect(facts.some((fact) => fact.startsWith("ran at="))).toBe(true);
    expect(facts.some((fact) => fact.startsWith("doubt=a acme orders.create call was in flight"))).toBe(true);
  });

  /**
   * The rule the whole file is built on: a fact the engine has not sent is left out.
   *
   * Two unavailable facts are always in this state — a transfer grant with its expiry, and the time left
   * on a retention class — because neither has a field on the wire. Their absence is the honest
   * answer, and this is what keeps somebody from filling them in later.
   */
  it("should_DropEveryFactTheEngineDidNotSend_When_TheNodeIsBare", () => {
    const bare: WorkspaceNode = {
      id: "id8", command: "", dependsOn: [], state: "pending", provenance: {}, cautions: [], kept: false,
    };
    const facts = said({ node: bare, workspace: { ...workspace, nodes: [bare] }, context });
    for (const absent of [
      "provider", "capability", "unsafe", "traced", "interactive", "repeatable", "effects",
      "ran at", "run", "environment", "revision", "target", "endpoint", "origin", "grant",
      "caution", "doubt", "private", "retention", "result",
    ]) {
      expect(facts.some((fact) => fact.startsWith(`${absent}=`))).toBe(false);
    }
    // What is true whatever the engine has said: the node was not kept, and this is this session.
    expect(facts).toContain("kept=no");
    expect(facts).toContain("workspace=sales-api");
  });

  it("should_SayNothingAboutAGrant_When_TheEngineHasNotSentOne", () => {
    expect(said({ node: everything, workspace, context })).not.toContain("grant=");
    const granted = said({ node: everything, workspace, context: { ...context, grantMinutes: 12 } });
    expect(granted).toContain("grant=12 min left");
  });

  it("should_NameAProviderOnlyWhenTheCatalogueKnowsIt_When_TheCommandIsRead", () => {
    expect(capabilityOf("acme orders.list", catalogue)?.capability?.safe).toBe(true);
    expect(capabilityOf("@trace(x) acme orders.create", catalogue)?.capability?.safe).toBe(false);
    expect(capabilityOf("nothing like.this", catalogue)).toBeUndefined();
    expect(capabilityOf(":calc 1 + 1", catalogue)).toBeUndefined();
  });

  it("should_DrawEachFactAsTwoColumnsUnderItsHeading_When_TheTabIsWritten", () => {
    const lines = factLines(openFacts({ node: everything, cell, workspace, context, client: "c1" })).map(lineText);
    expect(lines[0]).toBe("about the command");
    expect(lines[1]).toBe("provider        acme");
    expect(lines).toContain("about the run");
    expect(lines).toContain("about the session");
  });

  it("should_ReadTheSubjectFromTheNode_When_TheScreenIsHeaded", () => {
    expect(lineText(openSubject({ node: everything, workspace, context })))
      .toContain("$orders   acme orders.create since:2026-09-01");
    expect(lineText(openSubject({ workspace, context }))).toBe("no result is open");
  });

  it("should_ReadTheLocalClock_When_ATimeIsWritten", () => {
    expect(when("2026-09-20T09:12:00.000Z")).toMatch(/^2026-09-\d\d \d\d:\d\d:\d\d$/);
    expect(when(undefined)).toBeUndefined();
    expect(when("not a time")).toBeUndefined();
  });
});

describe("the whole result, where the cell kept three rows", () => {
  const stored: StoredValue = {
    type: { kind: "list", element: { kind: "record", name: "Order", fields: [{ name: "id", type: { kind: "primitive", name: "Text" } }] } },
    data: Array.from({ length: 12 }, (_, at) => ({ id: `1043${at}` })),
    provenance: {},
  };

  it("should_DrawEveryRowUpToAPage_When_TheResultIsOpened", () => {
    const root = windowed(stored);
    expect(root.kind === "table" && root.rows).toHaveLength(12);
    expect(root.kind === "table" && root.columns.map((column) => column.name)).toEqual(["id"]);
    expect(root.more).toBeUndefined();
  });

  it("should_DrawARecordsFieldsAsRows_When_TheResultIsNotAList", () => {
    const one: StoredValue = {
      type: { kind: "record", name: "Output", fields: [{ name: "exitCode", type: { kind: "primitive", name: "INT" } }] },
      data: { exitCode: 0, stdout: "line one\nline two\n" },
      provenance: {},
    };
    const root = windowed(one);
    expect(root.kind === "fields" && root.rows.map((row) => row.name)).toEqual(["exitCode", "stdout"]);
    const stdout = root.kind === "fields" ? root.rows[1]!.node : undefined;
    expect(stdout?.kind === "text" && stdout.lines.map(runText)).toEqual(["line one", "line two"]);
  });

  it("should_WriteTheValueAsJson_When_TheJsonTabIsAsked", () => {
    expect(openJson(stored)).toContain('"id": "10430"');
    expect(openJson(undefined)).toBe("");
  });
});

describe("where an opened result goes", () => {
  it("should_NameTheNodeAndTheTab_When_TheRouteIsWritten", () => {
    expect(openRoute("id7", "details")).toBe("#open/id7/details");
    expect(readOpenRoute("#open/id7/details")).toEqual({ node: "id7", tab: "details" });
    expect(readOpenRoute("#open/id7")).toEqual({ node: "id7", tab: "result" });
  });

  it("should_NameThePiece_When_OnePieceOfTheResultIsPeekedAt", () => {
    expect(peekRoute("id7", "type", "shared")).toBe("#peek/id7/type?workspace=shared");
    expect(readOpenRoute("#peek/id7/type?workspace=shared")).toEqual({ node: "id7", tab: "result", peek: "type", workspace: "shared" });
    expect(readOpenRoute("#peek/id7/source")).toEqual({ node: "id7", tab: "result", peek: "source" });
    // A piece nobody can peek at falls back to the whole result.
    expect(readOpenRoute("#peek/id7/bogus")).toEqual({ node: "id7", tab: "result" });
    expect(readOpenRoute("#peek/")).toBeUndefined();
  });

  it("should_BeNoRouteAtAll_When_TheHashNamesSomethingElse", () => {
    expect(readOpenRoute("")).toBeUndefined();
    expect(readOpenRoute("#surface/cells-minimal/ink")).toBeUndefined();
    expect(readOpenRoute("#open/")).toBeUndefined();
  });

  it("should_SetWhereItOpens_When_TheCommandIsTyped", () => {
    const typed = read("/settings results open window");
    expect(typed.kind).toBe("settings");
    expect(typed.kind === "settings" && typed.change(defaults).openIn).toBe("window");
    expect(read("/settings results open screen")).toMatchObject({ kind: "settings" });
    expect(read("/settings results open sideways")).toMatchObject({ kind: "trouble" });
    // `/settings` and `/settings results` still open the screen.
    expect(read("/settings results")).toEqual({ kind: "screen", screen: "settings", section: "results" });
  });

  it("should_KeepTheChoice_When_TheSettingsAreSavedAndRead", () => {
    expect(restoreSettings({ ...defaults, openIn: "window" }).openIn).toBe("window");
    expect(restoreSettings({ ...defaults, openIn: "nonsense" }).openIn).toBe(defaults.openIn);
    const row = readSection("results", { settings: defaults, workspace }).rows[0]!;
    expect(restoreSettings(chose(defaults, row, { name: "window", what: "" })).openIn).toBe("window");
  });

  it("should_NameWhatToOpen_When_OpenIsTypedWithAReference", () => {
    expect(read("/open $orders")).toEqual({ kind: "screen", screen: "open", node: "orders" });
    expect(read("/open id7")).toEqual({ kind: "screen", screen: "open", node: "id7" });
    expect(read("/open")).toEqual({ kind: "screen", screen: "open" });
  });
});

/*
 * Every shape the engine can hand back, and some it should not.
 *
 * `/values/<handle>` is read with a cast, so what arrives is whatever the engine encoded — and a
 * shape nobody anticipated used to throw inside a render, which unmounts the client. These are the
 * representative wire shapes plus malformed values that a cast cannot rule out. None of them may throw, and none may come back empty when there is
 * something to say.
 */
describe("what /open does with any value at all", () => {
  const value = (type: unknown, data: unknown): StoredValue =>
    ({ type, data, provenance: {} }) as StoredValue;

  /** Synthetic examples of scalar, structured and malformed wire values. */
  const shapes: readonly (readonly [string, StoredValue])[] = [
    ["a record with bytes in it", value(
      { kind: "record", name: "ProcessOutput", fields: [
        { name: "exitCode", type: { kind: "primitive", name: "INT" } },
        { name: "stdout", type: { kind: "primitive", name: "BYTES" } },
      ] },
      { exitCode: 0, stdout: "b25lCg==", stderr: "" },
    )],
    ["a list of records the type did not name", value(
      { kind: "list", element: { kind: "unknown" } },
      [{ provider: "http", environment: "default" }, { provider: "sh", environment: "default" }],
    )],
    ["a scalar", value({ kind: "primitive", name: "INT" }, 2)],
    ["some text", value({ kind: "primitive", name: "TEXT" }, "hello")],
    ["a list of scalars", value({ kind: "list", element: { kind: "primitive", name: "INT" } }, [1, 2, 3])],
    ["an empty list", value({ kind: "list", element: { kind: "unknown" } }, [])],
    ["none", value({ kind: "option", element: { kind: "unknown" } }, { kind: "none" })],
    ["an unnamed record", value(
      { kind: "record", name: "", fields: [{ name: "a", type: { kind: "primitive", name: "INT" } }] },
      { a: 1, b: "x" },
    )],
    ["a record whose field is a list", value(
      { kind: "record", name: "R", fields: [{ name: "xs", type: { kind: "list", element: { kind: "unknown" } } }] },
      { xs: [1, 2, 3] },
    )],
    ["bytes that are not text", value({ kind: "primitive", name: "BYTES" }, "//79")],
    // What a cast cannot rule out, and what used to take the client down.
    ["no type at all", value(undefined, { a: 1 })],
    ["a type that is not an object", value("List<Order>", [1, 2])],
    ["a kind nothing knows", value({ kind: "matrix", rows: 2 }, [[1, 2]])],
    ["a record type with no fields array", value({ kind: "record", name: "R" }, { a: 1 })],
    ["a record type whose fields are junk", value({ kind: "record", name: "R", fields: [null, 7] }, { a: 1 })],
    ["no data at all", value({ kind: "primitive", name: "INT" }, undefined)],
    ["data that is null", value({ kind: "option", element: { kind: "unknown" } }, null)],
  ];

  const bare: WorkspaceNode = {
    id: "id9", command: "sh run cmd:\"echo\"", dependsOn: [], state: "ready",
    provenance: {}, cautions: [], kept: true, handle: "h9",
  };

  it("should_DrawSomethingForEveryShape_When_TheEngineHandsOneBack", () => {
    for (const [what, stored] of shapes) {
      const drawn = readOpen({ node: bare, workspace, context, stored });
      expect(() => drawn, what).not.toThrow();
      expect(lineText(drawn.subject), what).toContain("id9");
      expect(typeof drawn.json, what).toBe("string");
      expect(drawn.details.length, what).toBeGreaterThan(0);
      // Nothing may read as the failure line unless it really failed.
      expect(drawn.details.map(lineText).join("\n"), what).not.toContain("could not draw");
    }
  });

  it("should_PresentEveryShapeWithoutThrowing_When_TheWindowDrawsIt", () => {
    for (const [what, stored] of shapes) expect(() => windowed(stored), what).not.toThrow();
    // Bytes that are not text are said as a size, never as mojibake.
    const bytes = windowed(shapes[9]![1]);
    expect(JSON.stringify(bytes), "bytes").toContain("not text");
  });

  it("should_SayWhatWentWrongInTheRecordForm_When_APartCannotBeWorkedOut", () => {
    expect(lineText(couldNotDraw("details", "x is not a function")))
      .toBe("could not draw details    x is not a function");
    // The shape of the failure, not a thrown error: one tab says so and the rest still draw.
    const exploding = { get data(): unknown { throw new Error("the value could not be read"); }, type: { kind: "unknown" }, provenance: {} } as unknown as StoredValue;
    const drawn = readOpen({ node: bare, workspace, context, stored: exploding });
    expect(drawn.json).toBe("this value cannot be written as JSON");
    // The other two were never in doubt.
    expect(lineText(drawn.subject)).toContain("id9");
    expect(drawn.details.map(lineText).join("\n")).toContain("about the run");
  });

  it("should_SayNothingIsThere_When_NoResultIsOpen", () => {
    const drawn = readOpen({ workspace, context });
    expect(lineText(drawn.subject)).toBe("no result is open");
    expect(drawn.json).toBe("");
  });
});

/*
 * `/open` naming a result that is not there.
 *
 * A typo and a result that has been let go are the same thing from here, and both get the answer
 * every other mistyped command gets: a sentence at the prompt and a session left exactly as it was.
 */
describe("what /open says when it names nothing", () => {
  const two: Workspace = {
    ...emptyWorkspace,
    nodes: [
      { ...everything, id: "id7", name: "orders" },
      { ...everything, id: "id8", name: "note" },
      { ...everything, id: "id9", name: undefined },
    ],
  };

  it("should_NameWhatThereIsInstead_When_NoResultAnswersToThatName", () => {
    const asked = resultNamed(two, "x");
    expect(asked.node).toBeUndefined();
    expect(asked.trouble).toBe("'/open' knows no result called x · names: $orders $note $id9");
  });

  it("should_SayTheSame_When_AnIdIsNoLongerThere", () => {
    expect(resultNamed(two, "id404").trouble).toContain("knows no result called id404");
    expect(resultNamed(emptyWorkspace, "id404").trouble).toBe("'/open' knows no result called id404");
  });

  it("attributes shared lookup failures to the requesting editor", () => {
    expect(resultNamed(two, "view", "/edit").trouble)
      .toBe("'/edit' knows no result called view · names: $orders $note $id9");
    expect(resultNamed(two, undefined, "/edit").trouble).toBe("'/edit' takes a result: $orders $note $id9");
    expect(resultNamed(emptyWorkspace, undefined, "/edit").trouble).toBe("there is no result to edit yet");
    expect(resultNamed(two, "orders", "/edit")).toEqual({ node: "id7" });
  });

  it("should_FindIt_When_TheNameOrTheIdIsOneThatIsThere", () => {
    expect(resultNamed(two, "orders")).toEqual({ node: "id7" });
    expect(resultNamed(two, "id8")).toEqual({ node: "id8" });
    expect(resultNamed(two, "id9")).toEqual({ node: "id9" });
  });

  it("should_AskForOne_When_NothingIsFocusedAndNothingWasNamed", () => {
    expect(resultNamed(two, undefined).trouble).toBe("'/open' takes a result: $orders $note $id9");
    expect(resultNamed(emptyWorkspace, undefined).trouble).toBe("there is no result to open yet");
  });
});

it("explains the exact archive threshold measure for public results", () => {
  const facts = said({node:{...everything,private:false},cell,workspace,context});
  expect(facts).toContain("storage bytes=2048 bytes · encoded value, type and provenance");
  expect(facts.some(fact=>fact.startsWith("memory charge="))).toBe(false);
});
