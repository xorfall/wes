import { describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { MonoLine, lineText } from "./MonoLine";
import { Session } from "./Session";
import { joined,
  countsLine, contextLine, guardOf, marksOf, nodesOf, noteLine, pinBindingOf, readSession, refusalOf, stateOf, topLine, verdictOf,
} from "./session-model";
import { newCell } from "../cells";
import { apply as applyEvent, emptyWorkspace } from "../workspace";
import type { Event } from "../protocol";
import { sessionCells, sessionContext, sessionNow, sessionWorkspace } from "./session-fixture";
import { Cell, runAvailability, type Theme } from "./Cell";
import { commandLine, commandSegments } from "./command-line";
import { readFileSync } from "node:fs";

const input = { workspace: sessionWorkspace, cells: sessionCells, context: sessionContext };
const model = readSession(input, sessionNow);

function textOf(node: { children: readonly unknown[] }): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : textOf(child as { children: readonly unknown[] })))
    .join("");
}

function draw(chrome: Theme): ReactTestRenderer {
  let tree: ReactTestRenderer | undefined;
  act(() => { tree = create(<Session model={model} chrome={chrome} prompt={<MonoLine segments={[{ text: "❯ ", role: "mono-ref-strong" }]} className="session-prompt-line" />} />); });
  return tree!;
}

describe("the session's scrollback", () => {
  it("should_ReadFiveFixtureCellsInOrder_When_ProjectedFromTheWorkspace", () => {
    expect(model.cells.map((cell) => `${cell.time}  ${cell.rows.map((row) => lineText(row.segments)).join("\n")}`)).toEqual([
      '09:12  acme orders.list since:2026-09-01 status:"open"',
      "09:13  acme usage.by_region since:2026-09-01",
      "09:14  acme events.tail",
      "09:14  acme customer.get id:99999",
      "09:15  acme deps.graph",
    ]);
    // Each row carries the nodes it makes, for the gutter.
    expect(model.cells.map((cell) => cell.rows[0]!.nodes.map((node) => `${node.glyph} ${node.label}`).join(" "))).toEqual([
      "ready $orders", "ready $usage", "running $events", "failed lookup", "ready $deps",
    ]);
  });

  it("should_SayWhatEachCellIs_When_TheNodesAreRead", () => {
    expect(model.cells.map((cell) => cell.state)).toEqual(["pinned", "default", "live", "failed", "default"]);
  });

  it("should_SayEachVerdictInOneGrammar_When_TheCellsAreProjected", () => {
    expect(model.cells.map((cell) => lineText(joined(cell.verdict)))).toEqual([
      "ok · table 128×6 · pinned",
      "ok · bars 4 · kept",
      "running · 18 s · ● live",
      "failed · not found · not kept",
      "ok · graph 42/87 · kept",
    ]);
  });

  it("should_LeaveOutAFieldTheEngineHasNotSaid_When_TheVerdictIsWritten", () => {
    // The engine reports when a node started and never when it finished, so a finished command has
    // no duration the client can honestly print — and the separator goes with the missing field.
    const finished = lineText(joined(model.cells[0]!.verdict));
    expect(finished).not.toMatch(/ · {2}/);
    expect(finished).not.toMatch(/ms|\bs\b/);
    // A running one does have a time: how long it has been running.
    expect(lineText(joined(model.cells[2]!.verdict))).toContain("18 s");
  });

  it("should_KeepTheStateColoursTheDesignGives_When_TheVerdictIsWritten", () => {
    expect(model.cells[2]!.verdict[0]).toEqual({ segments: [{ text: "running", role: "mono-meta-strong" }], keep: true, slot: "state" });
    expect(model.cells[3]!.verdict[0]).toEqual({ segments: [{ text: "failed", role: "mono-bad-strong" }], keep: true, slot: "state" });
    expect(model.cells[0]!.verdict[0]).toEqual({ segments: [{ text: "ok", role: "mono-ok" }], keep: true, slot: "state" });
  });
});

describe("what a cell is", () => {
  const cell = sessionCells[1]!;
  const nodes = nodesOf(sessionWorkspace, cell);

  it.each(["{ container: Text, image: Text, logs: List<{ text: Text, partial: Bool }> }", "List<{ id: Text, nested: Option<{ first: Text, second: Text }> }>", "NamedRecord"])("retains the actual type before and after reconstructing workspace metadata: %s", type => {
    const metadata = [{ ...nodes[0]!, type }];
    for (const restored of [metadata, JSON.parse(JSON.stringify(metadata))]) {
      expect(lineText(joined(verdictOf("default", cell, restored, sessionNow)))).toBe(`ok · ${type} · kept`);
    }
  });

  it("should_PreferWhatIsHappeningNow_When_SeveralThingsAreTrueAtOnce", () => {
    expect(stateOf({ ...cell, state: "running", pinned: true }, nodes, true)).toBe("live");
    expect(stateOf({ ...cell, state: "unanswered", pinned: true }, nodes, true)).toBe("failed");
    expect(stateOf({ ...cell, pinned: true }, [{ ...nodes[0]!, state: "stale" }], false)).toBe("stale");
    expect(stateOf({ ...cell, pinned: true }, nodes, true)).toBe("pinned");
  });

  it("should_TakeTheFocusState_When_NothingElseIsTrueOfIt", () => {
    expect(stateOf(cell, nodes, true)).toBe("focus");
    expect(stateOf(cell, nodes, false)).toBe("default");
  });

  it("should_CountTheStaleNodes_When_TheCellHasGoneOutOfDate", () => {
    const stale = [{ ...nodes[0]!, state: "stale" as const }];
    expect(lineText(joined(verdictOf("stale", cell, stale, sessionNow)))).toBe("stale · The reason this result became stale was not recorded. · 1 stale");
  });

  it("should_CountStaleNodesOnceAndSayRetention_When_SeveralMixedResultsAreStale", () => {
    // Arrange
    const base = { ...nodes[0]!, doubt: undefined, evidence: undefined };
    const mixed = [
      { ...base, id: "kept-ready", state: "ready" as const, kept: true, handle: "h-kept" },
      { ...base, id: "stale-previous", state: "stale" as const, kept: false, handle: "h-previous" },
      { ...base, id: "stale-other", state: "stale" as const, kept: false, handle: "h-other" },
      { ...base, id: "failed-stage", state: "failed" as const, kept: false, handle: undefined, failure: "synthetic failure" },
    ];
    // Act
    const said = lineText(joined(verdictOf("stale", cell, mixed, sessionNow)));
    const pinned = lineText(joined(verdictOf("stale", { ...cell, pinned: true }, mixed, sessionNow)));
    // Assert
    expect(said).toBe("stale · 4 results · 1 ok · 1 failed · 2 stale · 1 of 3 kept");
    expect(said.match(/\d+ stale/g)).toHaveLength(1);
    expect(pinned).toBe("stale · 4 results · 1 ok · 1 failed · 2 stale · pinned");
  });
});

describe("the marks a cell carries", () => {
  it("should_CarryNothing_When_NoFactCouldHaveDiffered", () => {
    expect(marksOf(sessionCells[0]!, nodesOf(sessionWorkspace, sessionCells[0]!), sessionContext)).toEqual([]);
  });

  it("should_MarkTheEnvironment_When_ItIsNotTheSessionsOwn", () => {
    const node = {
      ...nodesOf(sessionWorkspace, sessionCells[0]!)[0]!,
      environment: { event: "node-environment" as const, node: "orders", environment: "PROD", target: "acme-us", revision: "rev 14", endpoint: null, origin: "command" },
    };
    expect(marksOf(sessionCells[0]!, [node], sessionContext)).toEqual([
      { kind: "environment", text: "PROD", title: "rev 14" },
      { kind: "target", text: "acme-us" },
    ]);
  });

  it("should_MarkTheAcknowledgedEffects_When_ARepeatWasForced", () => {
    const marks = marksOf({ ...sessionCells[0]!, acknowledgeEffects: true }, [], sessionContext);
    expect(marks).toEqual([{ kind: "effects", text: "effects acknowledged" }]);
  });
});

describe("a repeat that must be asked about", () => {
  it("should_NameWhatHangsOffIt_When_TheCellHasDependents", () => {
    const cell = sessionCells[0]!;
    const guard = guardOf(sessionWorkspace, cell, nodesOf(sessionWorkspace, cell), sessionContext);
    expect(guard?.dependents).toEqual(["$daily", "$totals"]);
    expect(guard?.what).toBe(cell.text);
  });

  it("should_NameTheEnvironment_When_TheCommandPerformsAnEffectAgain", () => {
    const cell = sessionCells[1]!;
    const nodes = nodesOf(sessionWorkspace, cell).map((node) => ({ ...node, repeatable: false }));
    expect(guardOf(sessionWorkspace, cell, nodes, sessionContext)?.against).toBe("DEV");
  });

  it("should_CarryTheUnknownOutcomeOnTheSharedGuard_When_AnEffectWasInterrupted", () => {
    const cell = sessionCells[1]!;
    const nodes = nodesOf(sessionWorkspace, cell).map((node) => ({ ...node, repeatable: false }));
    expect(guardOf(sessionWorkspace, cell, nodes, sessionContext)?.unknownOutcome).toBeUndefined();
    const doubt = nodes.map(node => ({ ...node, doubt: { capability: "UNSAFE", safe: false, when: "2026-10-04T09:00:00Z" } }));
    expect(guardOf(sessionWorkspace, cell, doubt, sessionContext)).toMatchObject({ against: "DEV", unknownOutcome: true });
    // A SAFE, independent node still needs no question: the guard is not widened by doubt alone.
    const safe = sessionCells[4]!;
    expect(guardOf(sessionWorkspace, safe, nodesOf(sessionWorkspace, safe).map(node => ({ ...node, doubt: { capability: "SAFE", safe: true, when: "" } })), sessionContext)).toBeUndefined();
  });

  it("should_NameAStreamSourceOnlyFromTheEnginesCreatedStatement_When_DependenciesSuggestOtherwise", () => {
    // Arrange
    const created = (node: string, dependsOn: string[], extra: Partial<Extract<Event, { event: "created" }>>): Event =>
      ({ event: "created", dependencyLifetime: "continuous", node, name: node, command: `synthetic ${node}`, dependsOn, interactive: false, ...extra });
    let workspace = emptyWorkspace;
    for (const event of [
      created("config", [], {}),
      // A true source whose only input is finite.
      created("source", ["config"], { streamOutput: true, streamSource: true }),
      // A consumer of a stream, and one whose dependency entry is not in this workspace at all.
      created("consumer", ["source"], { streamOutput: true, streamSource: false }),
      created("orphan", ["not-listed"], { streamOutput: true, streamSource: false }),
      // The field absent: no source is assumed, even with no streaming inputs visible.
      created("unstated", [], { streamOutput: true }),
    ]) workspace = applyEvent(workspace, event);
    workspace = { ...workspace, nodes: workspace.nodes.map(node => ({ ...node, state: "running" as const })) };
    const cell = (id: string, nodes: string[]) => ({ ...newCell(id), state: "answered" as const, nodes });
    // Act
    const model = readSession({ workspace, cells: [cell("one", ["source"]), cell("two", ["consumer"]), cell("three", ["orphan"]), cell("four", ["unstated"]),
      cell("five", ["source", "consumer"]), cell("six", ["config", "consumer"])], context: sessionContext }, sessionNow);
    const cancelLabel = (at: number) => {
      const shown = model.cells[at]!;
      let tree!: ReactTestRenderer;
      act(() => { tree = create(<Cell theme="keys" state={shown.state} rows={shown.rows} verdict={shown.verdict} streamOutput={shown.streamOutput} streamSource={shown.streamSource}
        pipeline={shown.pipeline} blocks={[]} actions={{ cancel: vi.fn() }} />); });
      const label = tree.root.findByProps({ "aria-keyshortcuts": "x" }).props["aria-label"] as string;
      act(() => tree.unmount());
      return label;
    };
    // Assert
    expect(model.cells.map(it => it.streamSource)).toEqual([true, false, false, false, true, false]);
    expect(model.cells.map((_, at) => cancelLabel(at))).toEqual(["x stop source", "x stop", "x stop", "x stop", "x stop pipeline", "x stop pipeline"]);
  });

  it("should_AskNothing_When_TheRepeatDisturbsNothing", () => {
    const cell = sessionCells[4]!;
    expect(guardOf(sessionWorkspace, cell, nodesOf(sessionWorkspace, cell), sessionContext)).toBeUndefined();
  });
});

describe("the lines around the scrollback", () => {
  it("should_ReadTheFixtureTopLine_When_TheSessionIsConnected", () => {
    expect(lineText(topLine(sessionContext))).toBe("wes  /  sales-api  ·  connected");
  });

  it("should_SayWhatACommandWouldRunAgainst_When_TheContextLineIsWritten", () => {
    expect(lineText(contextLine(sessionContext))).toBe("env: DEV  ·  rev 14  ·  grant 12 min  ·  → acme-eu");
  });

  it("should_CountTheWorkspace_When_TheFooterIsWritten", () => {
    expect(lineText(countsLine(sessionWorkspace))).toBe("9 nodes  ·  3 kept  ·  2 stale  ·  ⇣ following");
    expect(lineText(countsLine(sessionWorkspace, false))).toBe("9 nodes  ·  3 kept  ·  2 stale  ·  ⇣ not following");
  });

  it("should_AnnounceWhatJustWentStale_When_SomethingDid", () => {
    expect(lineText(noteLine(sessionWorkspace)!)).toBe("~ 2 results stale   /stale");
  });

  it("should_AnnounceNothing_When_NothingWentStale", () => {
    expect(noteLine({ ...sessionWorkspace, wentStale: [] })).toBeUndefined();
  });

  it("should_NameTheKeysTheSessionOffers_When_TheFooterIsWritten", () => {
    expect(lineText(model.keys)).toBe("↑↓ history  ·  ⇥ complete  ·  ⇧⏎ newline  ·  /graph /env /settings  ·  ? keys");
  });
});

describe("the session's chrome", () => {
  it("should_ReDressEveryCell_When_TheChromeChanges", () => {
    const tree = draw("keys");
    const chromes = () => tree.root.findAllByType("section").filter(node => node.props["data-cell"] !== undefined).map((cell) => String(cell.props["data-theme"]));
    expect(chromes()).toEqual(["keys", "keys", "keys", "keys", "keys"]);
    act(() => {
      tree.update(<Session model={model} chrome="controls" prompt={<MonoLine segments={[{ text: "❯ ", role: "mono-ref-strong" }]} className="session-prompt-line" />} />);
    });
    expect(chromes()).toEqual(["controls", "controls", "controls", "controls", "controls"]);
    // The states are what they were: chrome dresses a cell, it does not change what the cell is.
    expect(tree.root.findAllByType("section").filter(node => node.props["data-cell"] !== undefined).map((cell) => String(cell.props["data-state"])))
      .toEqual(["pinned", "default", "live", "failed", "default"]);
    act(() => tree.unmount());
  });

  it("should_DrawTheNoteInTheFooterOutsideScrollback_When_TheSessionIsRendered", () => {
    const tree = draw("keys");
    const lines = tree.root.findAllByType("pre").map((pre) => ({ name: String(pre.props.className), text: textOf(pre) }));
    expect(lines.find((line) => line.name.includes("session-top-line"))?.text).toBe("wes  /  sales-api  ·  connected");
    expect(lines.find((line) => line.name.includes("session-note"))?.text).toBe("~ 2 results stale   /stale");
    expect(tree.root.findByProps({ className: "session-scrollback" }).findAllByProps({ className: "session-note" })).toHaveLength(0);
    expect(tree.root.findByProps({ className: "session-footer" }).findAllByProps({ className: "session-note" })).toHaveLength(1);
    expect(lines.find((line) => line.name.includes("session-context"))?.text).toBe("env: DEV  ·  rev 14  ·  grant 12 min  ·  → acme-eu");
    expect(lines.find((line) => line.name.includes("session-counts"))?.text)
      .toBe("9 nodes  ·  3 kept  ·  2 stale  ·  ⇣ following");
    act(() => tree.unmount());
  });

  it("should_LetNoCellBeSquashed_When_TheScrollbackIsFull", () => {
    // Flex children shrink by default, and the line a squashed cell loses first is its verdict.
    const css = readFileSync(new URL("./session.css", import.meta.url), "utf8");
    expect(css).toMatch(/\.session-scrollback > \*\s*\{\s*flex: 0 0 auto;/);
    expect(css).toMatch(/\.session-scrollback\s*\{[^}]*overflow-y: auto;/);
  });

  it("should_TellWhichCellTookFocus_When_OneIsFocused", () => {
    const onFocus = vi.fn();
    let tree: ReactTestRenderer | undefined;
    act(() => {
      tree = create(<Session model={model} chrome="keys" prompt={null} onFocus={onFocus} />);
    });
    act(() => tree!.root.findAllByType("section").filter(node => node.props["data-cell"] !== undefined)[2]!.props.onFocus());
    expect(onFocus).toHaveBeenCalledWith("c3");
    act(() => tree!.unmount());
  });
  it("clears the cell focus tint when focus enters the prompt or leaves the session",()=>{
    const onFocus=vi.fn();let tree!:ReactTestRenderer;
    act(()=>{tree=create(<Session model={model} chrome="keys" prompt={null} onFocus={onFocus}/>);});
    const session=tree.root.findByProps({className:"session"});
    act(()=>session.props.onFocus({target:{closest:()=>null}}));
    expect(onFocus).toHaveBeenLastCalledWith(undefined);
    onFocus.mockClear();
    act(()=>session.props.onBlur({currentTarget:{contains:()=>true},relatedTarget:{}}));
    expect(onFocus).not.toHaveBeenCalled();
    act(()=>session.props.onBlur({currentTarget:{contains:()=>false},relatedTarget:null}));
    expect(onFocus).toHaveBeenCalledWith(undefined);
    act(()=>tree.unmount());
  });
  it("clears frame-reported focus when another pane takes focus without a session blur",()=>{
    const listeners=new Set<(event:{target:unknown})=>void>(),onFocus=vi.fn();
    const inside={},outside={};
    const ownerDocument={
      addEventListener:(_type:string,listener:(event:{target:unknown})=>void)=>listeners.add(listener),
      removeEventListener:(_type:string,listener:(event:{target:unknown})=>void)=>listeners.delete(listener),
    };
    let tree!:ReactTestRenderer;
    act(()=>{tree=create(<Session model={model} chrome="keys" prompt={null} onFocus={onFocus}/>,{
      createNodeMock:element=>element.props.className==="session"?{ownerDocument,contains:(target:unknown)=>target===inside}:null,
    });});
    act(()=>tree.root.findAllByType("section").filter(node => node.props["data-cell"] !== undefined)[2]!.props.onFocus());
    expect(onFocus).toHaveBeenLastCalledWith("c3");onFocus.mockClear();
    act(()=>listeners.forEach(listener=>listener({target:inside})));
    expect(onFocus).not.toHaveBeenCalled();
    act(()=>listeners.forEach(listener=>listener({target:outside})));
    expect(onFocus).toHaveBeenCalledWith(undefined);
    act(()=>tree.unmount());
    expect(listeners.size).toBe(0);
  });
});

describe("a command line's colours", () => {
  it("should_ColourAProviderCallAndItsParameters_When_ACommandIsRead", () => {
    expect(commandSegments('acme orders.list since:2026-09-01 status:"open"')).toEqual([
      { text: "acme", role: "mono-provider" },
      { text: " " },
      { text: "orders.list", role: "mono-provider" },
      { text: " " },
      { text: "since:", role: "mono-param" },
      { text: "2026-09-01", role: "mono-literal" },
      { text: " " },
      { text: "status:", role: "mono-param" },
      { text: '"open"', role: "mono-literal" },
    ]);
  });

  it("should_ColourAMetaWordAndARedirect_When_ACalculationIsRead", () => {
    expect(commandSegments(":calc $orders.count() > total")).toEqual([
      { text: ":calc", role: "mono-meta" },
      { text: " " },
      { text: "$orders", role: "mono-ref" },
      { text: ".", role: "mono-faint" },
      { text: "count", role: "mono-provider" },
      { text: "(", role: "mono-faint" },
      { text: ")", role: "mono-faint" },
      { text: " " },
      { text: ">", role: "mono-dim" },
      { text: " " },
      { text: "total", role: "mono-ref" },
    ]);
  });

  it("should_KeepAQuotedValueWhole_When_ItHasSpacesInIt", () => {
    expect(commandSegments('acme orders.list note:"two words"').map((segment) => segment.text).join(""))
      .toBe('acme orders.list note:"two words"');
  });

  it("should_KeepEverySpace_When_ALineIsRead", () => {
    const written = "acme  orders.list   since:2026-09-01 ";
    expect(commandSegments(written).map((segment) => segment.text).join("")).toBe(written);
  });
});

/**
 * A command the engine would not run.
 *
 * `:list sh` is refused with CHK015 and makes no node at all, so a cell that reads only its nodes
 * has nothing to read and used to say `ok · not kept` about something that never ran.
 */
describe("a command the engine refused", () => {
  const cell = { ...newCell(":list sh"), state: "answered" as const };
  const refused = {
    ...emptyWorkspace,
    attemptFailures: { [cell.lastRun]: "CHK015: ':list' has nothing called 'sh'" },
  };

  it("should_ReadTheRefusalFromTheAttempt_When_TheEngineSentOne", () => {
    expect(refusalOf(refused, cell)).toBe("CHK015: ':list' has nothing called 'sh'");
  });

  it("should_ReadItFromTheDiagnostics_When_TheAttemptDidNotCarryIt", () => {
    const reported = { ...cell, diagnostics: [
      { code: "CHK015", severity: "error" as const, message: "nothing called 'sh'", start: 0, end: 8, hints: [] },
    ] };
    expect(refusalOf(emptyWorkspace, reported)).toBe("CHK015: nothing called 'sh'");
  });

  it("should_SayItDidNotRunAndWhy_When_TheCellIsRead", () => {
    const model = readSession({ workspace: refused, cells: [cell], context: sessionContext }, sessionNow);
    expect(model.cells[0]?.state).toBe("failed");
    expect(lineText(joined(model.cells[0]!.verdict)))
      .toBe("not run · CHK015: ':list' has nothing called 'sh'");
  });

  it("should_StillSayOk_When_TheCommandRanAndSimplyMadeNoNodes", () => {
    const model = readSession({ workspace: emptyWorkspace, cells: [cell], context: sessionContext }, sessionNow);
    expect(model.cells[0]?.state).not.toBe("failed");
  });
});

/**
 * A command somebody stopped.
 *
 * Cancelling is not failing and it is certainly not `ok`: a cell that was stopped kept nothing and
 * finished nothing, and the surface said `ok · not kept` about it until this.
 */
describe("a command that was cancelled", () => {
  const cell = { ...newCell("sh run cmd:\"sleep 60\""), state: "answered" as const, nodes: ["n1"] };
  const stopped = {
    ...emptyWorkspace,
    nodes: [{
      id: "n1", command: cell.text, dependsOn: [], state: "cancelled" as const, provenance: {}, cautions: [],
      kept: false, cancellation: { code: "CANCELLED", reason: "stopped from the cell" },
    }],
  };

  it("should_SayCancelledAndWhy_When_TheVerdictIsWritten", () => {
    const model = readSession({ workspace: stopped, cells: [cell], context: sessionContext }, sessionNow);
    expect(lineText(joined(model.cells[0]!.verdict))).toBe("cancelled · stopped from the cell · not kept");
  });

  it("should_WearTheSameChromeAsAFailure_When_ItHasNoResult", () => {
    const model = readSession({ workspace: stopped, cells: [cell], context: sessionContext }, sessionNow);
    expect(model.cells[0]?.state).toBe("failed");
  });
});

/*
 * Following, which is about the reader's place in the scrollback and not about any one cell.
 *
 * Four transitions, and the first is the one that was missing: a command sent from halfway up the
 * scrollback has to bring the view down to its answer. `following.ts` already decided all of this
 * — `pinned` on ⏎, stay while at the bottom, stop the moment somebody scrolls up — and the session
 * simply had no way of telling it that a command had been sent.
 */
describe("what the scrollback follows", () => {
  /** A scrollback of a known size, so `scrollTop` can be read back as where the view ended up. */
  function box(overrides: Partial<{ scrollTop: number; clientHeight: number; scrollHeight: number }> = {}) {
    return { scrollTop: 0, clientHeight: 200, scrollHeight: 500, ...overrides };
  }

  function show(node: ReturnType<typeof box>, props: { following?: boolean; pinned?: number } = {}) {
    let tree: ReactTestRenderer | undefined;
    act(() => {
      tree = create(
        <Session model={model} chrome="keys" prompt={<MonoLine segments={[]} />} {...props} />,
        // Only the scrollback is asked about; every other element can stay the renderer's own.
        { createNodeMock: (element) => (element.props.className === "session-scrollback" ? node : null) },
      );
    });
    return tree!;
  }

  function again(tree: ReactTestRenderer, props: { following?: boolean; pinned?: number } = {}) {
    act(() => {
      tree.update(<Session model={model} chrome="keys" prompt={<MonoLine segments={[]} />} {...props} />);
    });
  }

  it("keeps a focused data control stationary while its footer appears, but honors a new submission", () => {
    const control = { closest: () => ({}) };
    const document = { activeElement: undefined as typeof control | undefined };
    const node = { ...box(), ownerDocument: document, contains: () => true };
    const tree = show(node);
    node.scrollTop = 300;
    document.activeElement = control;
    node.scrollHeight = 900;
    again(tree);
    expect(node.scrollTop).toBe(300);
    again(tree, { pinned: 1 });
    expect(node.scrollTop).toBe(900);
    act(() => tree.unmount());
  });

  it("should_JumpToTheNewestCell_When_ACommandIsSentFromHalfwayUp", () => {
    const node = box();
    const tree = show(node, { pinned: 0 });
    node.scrollTop = 0;
    node.scrollHeight = 900;
    again(tree, { pinned: 1 });
    expect(node.scrollTop).toBe(900);
    act(() => tree.unmount());
  });

  it("should_StayWithTheOutput_When_TheViewWasAlreadyAtTheBottom", () => {
    const node = box();
    const tree = show(node, { pinned: 0 });
    node.scrollTop = 300;
    node.scrollHeight = 900;
    again(tree, { pinned: 0 });
    expect(node.scrollTop).toBe(900);
    act(() => tree.unmount());
  });

  it("should_StayWhereTheReaderPutIt_When_TheyScrolledUpWhileItRan", () => {
    const node = box();
    const tree = show(node, { pinned: 0 });
    node.scrollTop = 20;
    node.scrollHeight = 900;
    again(tree, { pinned: 0 });
    expect(node.scrollTop).toBe(20);
    act(() => tree.unmount());
  });

  it("should_StayWhereTheReaderIs_When_ARenderFollowsGrowthThatCameWithoutOne", () => {
    let grew: () => void = () => {};
    vi.stubGlobal("ResizeObserver", class { constructor(callback: () => void) { grew = callback; } observe() {} disconnect() {} });
    const node = { ...box(), children: [{}], contains: () => false, ownerDocument: { activeElement: undefined } };
    const tree = show(node, { pinned: 0 });
    node.scrollTop = 300;
    /* A View frame reports its height after it draws: the scrollback grows with no render. */
    node.scrollHeight = 1200;
    act(() => grew());
    expect(node.scrollTop).toBe(1200);
    /* The reader scrolls up to read, then a render caused only by focus must not take them down. */
    node.scrollTop = 700;
    again(tree, { pinned: 0 });
    expect(node.scrollTop).toBe(700);
    /* Growth while reading above the bottom is not followed either. */
    node.scrollHeight = 1500;
    act(() => grew());
    expect(node.scrollTop).toBe(700);
    act(() => tree.unmount());
    vi.unstubAllGlobals();
  });

  it("should_NotJumpAtAll_When_FollowingIsOff", () => {
    const node = box();
    const tree = show(node, { pinned: 0, following: false });
    node.scrollTop = 20;
    node.scrollHeight = 900;
    again(tree, { pinned: 1, following: false });
    expect(node.scrollTop).toBe(20);
    /* And turning it back on does not act on the ⏎ that was pressed while it was off. */
    node.scrollHeight = 950;
    again(tree, { pinned: 1, following: true });
    expect(node.scrollTop).toBe(20);
    act(() => tree.unmount());
  });
});

describe("a multiline command on a cell", () => {
  const program = [":calc {", "  const gross = 84210.50;", "", "  return gross;", "} > revenue", ""].join("\n");
  it("projects every original source line after display metadata", () => {
    expect(lineText(commandLine(program, new Date("2026-09-20T09:16:00"))))
      .toBe("09:16  ❯ " + program);
  });
  it("keeps existing single-line provider roles", () => {
    expect(lineText(commandSegments("acme orders.list since:2026-09-01")))
      .toBe("acme orders.list since:2026-09-01");
  });
  it("keeps calc syntax roles while retaining all lines and the redirect exactly once", () => {
    const said = commandSegments(program);
    expect(said[0]).toEqual({ text: ":calc", role: "mono-meta" });
    expect(lineText(said)).toBe(program);
    expect(lineText(said).match(/> revenue/g)).toHaveLength(1);
    expect(said.some(segment => segment.role === "mono-literal" && segment.text === "84210.50")).toBe(true);
  });
});

it("distinguishes equally named workspace and environment without losing execution context", () => {
  const context = { ...sessionContext, workspace: "default", environment: "default" };
  expect(lineText(topLine(context))).toBe("wes  /  default  ·  connected");
  expect(lineText(contextLine(context))).toBe("env: default  ·  rev 14  ·  grant 12 min  ·  → acme-eu");
  expect(lineText(contextLine({ ...context, environment: undefined }))).toBe("rev 14  ·  grant 12 min  ·  → acme-eu");
});


it.each(["full", "pane"] as const)("keeps pinned cells outside the %s scrollback and restores chronological order", chromeMode => {
  let tree!: ReactTestRenderer;
  const pinned = readSession({ ...input, cells: sessionCells.map((cell, at) => ({ ...cell, pinned: at === 2 })) });
  act(() => { tree = create(<Session model={pinned} chrome="controls" chromeMode={chromeMode} prompt={null} />); });
  const strip = tree.root.findByProps({ "aria-label": "Pinned cells" });
  const log = tree.root.findByProps({ role: "log" });
  expect(strip.parent).toBe(log.parent);
  expect(strip.findAllByType(Cell).map(cell => cell.props.label)).toEqual([sessionCells[2]!.id]);
  expect(log.findAllByType(Cell).map(cell => cell.props.label)).toEqual(sessionCells.filter((_, at) => at !== 2).map(cell => cell.id));
  expect(pinned.cells[2]).toMatchObject({ state: "live", pinned: true });
  const unpinned = readSession({ ...input, cells: sessionCells.map(cell => ({ ...cell, pinned: false })) });
  act(() => tree.update(<Session model={unpinned} chrome="controls" chromeMode={chromeMode} prompt={null} />));
  expect(tree.root.findAllByProps({ "aria-label": "Pinned cells" })).toHaveLength(0);
  expect(tree.root.findByProps({ role: "log" }).findAllByType(Cell).map(cell => cell.props.label)).toEqual(sessionCells.map(cell => cell.id));
  act(() => tree.unmount());
});

describe("clearing a session viewport", () => {
  it("keeps the chronological clear boundary when its anchor is pinned and unpinned", () => {
    const base = { ...model, cells: model.cells.map(cell => ({ ...cell, pinned: false })) };
    const after = base.cells[1]!.id;
    let tree!: ReactTestRenderer;
    const draw = (pinnedIds: string[]) => <Session model={{ ...base, cells: base.cells.map(cell => ({ ...cell, pinned: pinnedIds.includes(cell.id) })) }} chrome="keys" prompt={null} clearRequest={{ after, revision: 1 }} />;
    act(() => { tree = create(draw([])); });
    const sequence = () => tree.root.findByProps({ className: "session-scrollback" }).children.map(child => typeof child === "string" ? child : child.type === Cell ? child.props.label : child.props.className);
    expect(sequence().slice(0, 3)).toEqual([base.cells[0]!.id, after, "session-clear-mark"]);
    act(() => tree.update(draw([after])));
    expect(sequence().slice(0, 2)).toEqual([base.cells[0]!.id, "session-clear-mark"]);
    act(() => tree.update(draw([base.cells[0]!.id, after])));
    expect(sequence()[0]).toBe("session-clear-mark");
    act(() => tree.update(draw([])));
    expect(sequence().slice(0, 3)).toEqual([base.cells[0]!.id, after, "session-clear-mark"]);
    expect(tree.root.findAllByType(Cell)).toHaveLength(base.cells.length);
    act(() => tree.unmount());
  });

  it("clears short history even with following off, preserves scroll-up, shrinks slack on output and handles resize", () => {
    let resize: (() => void) | undefined;
    const disconnect = vi.fn();
    vi.stubGlobal("ResizeObserver", class { constructor(callback: () => void) { resize = callback; } observe() {} disconnect = disconnect; });
    const box = { scrollTop: 0, clientHeight: 200, get scrollHeight() { return Math.max(this.clientHeight, 100 + fresh + parseFloat(tail.style.height || "0")); }, getBoundingClientRect: () => ({ top: 0 }) };
    let fresh = 50;
    const tail = { style: { height: "0px" }, getBoundingClientRect: () => ({ top: 100 + fresh - box.scrollTop }) };
    const mark = { getBoundingClientRect: () => ({ top: 100 - box.scrollTop }) };
    let tree!: ReactTestRenderer;
    const draw = (revision: number) => <Session model={model} chrome="keys" prompt={null} following={false} clearRequest={{ after: model.cells[1]!.id, revision }} />;
    act(() => { tree = create(draw(1), { createNodeMock: element => element.props.className === "session-scrollback" ? box : element.props.className === "session-clear-tail" ? tail : element.props.className === "session-clear-mark" ? mark : null }); });
    expect(tail.style.height).toBe("150px");
    expect(box.scrollTop).toBe(100);
    box.scrollTop = 20;
    fresh = 120;
    act(() => tree.update(draw(1)));
    expect(tail.style.height).toBe("80px");
    expect(box.scrollTop).toBe(20);
    act(() => tree.update(draw(2)));
    expect(box.scrollTop).toBe(100);
    box.clientHeight = 300;
    act(() => resize?.());
    expect(tail.style.height).toBe("180px");
    expect(box.scrollTop).toBe(100);
    act(() => tree.unmount());
    expect(disconnect).toHaveBeenCalled();
    vi.unstubAllGlobals();
  });
});

it("distinguishes process and HTTP outcomes from engine execution failure", () => {
  const cell = sessionCells[1]!;
  const nodes = nodesOf(sessionWorkspace, cell);
  for (const [name, data, label] of [
    ["ProcessOutput", { exitCode: 1, stdout: "", stderr: "" }, "exit 1"],
    ["ProcessOutput", { exitCode: 0, stdout: "", stderr: "" }, "exit 0"],
    ["HttpResponse", { status: 404, version: "HTTP/1.1", headers: [], body: "" }, "404"],
  ] as const) {
    const primitive = (name: string) => ({ kind: "primitive" as const, name });
    const fields = name === "ProcessOutput"
      ? [{ name: "exitCode", type: primitive("INT") }, { name: "stdout", type: primitive("BYTES") }, { name: "stderr", type: primitive("BYTES") }]
      : [{ name: "status", type: primitive("INT") }, { name: "version", type: primitive("TEXT") },
        { name: "headers", type: { kind: "list" as const, element: { kind: "record" as const, name: "HttpHeader", fields: [{ name: "name", type: primitive("TEXT") }, { name: "value", type: primitive("TEXT") }] } } },
        { name: "body", type: primitive("BYTES") }];
    const value = { type: { kind: "record" as const, name, fields }, data, state: "ready" as const };
    expect(lineText(joined(verdictOf("default", cell, nodes, sessionNow, undefined, value)))).toContain(label);
    expect(lineText(joined(verdictOf("failed", cell, nodes, sessionNow, "transport failed", value)))).toContain("transport failed");
    expect(stateOf(cell, nodes, false)).toBe("default");
  }
});

it("keeps stale announcements count-only and engine explanations on individual cells", () => {
  const staleReason = { code: "restore_not_retained", message: "No retained result was available when the workspace reopened. The command was not rerun." };
  const node = { ...sessionWorkspace.nodes[0]!, state: "stale" as const, staleReason };
  const fields = verdictOf("stale", sessionCells[0]!, [node], sessionNow);
  expect(lineText(joined(fields))).toContain(`stale · ${staleReason.message}`);
  const one = { ...sessionWorkspace, nodes: [node], wentStale: [node.id] };
  expect(lineText(noteLine(one)!)).toBe("~ 1 result stale   /stale");
  const mixed = { ...one, nodes: [node, { ...node, id: "second", staleReason: { code: "dependency_changed", message: "An upstream definition changed." } }], wentStale: [node.id, "second"] };
  expect(lineText(noteLine(mixed)!)).toBe("~ 2 results stale   /stale");
  expect(noteLine({ ...one, nodes: [{ ...node, state: "ready" }] })).toBeUndefined();
});

it("keeps routine stream invalidation on the cell without toggling the workspace banner", () => {
  const node = { ...sessionWorkspace.nodes[0]!, state: "stale" as const,
    staleReason: { code: "stream_updated", message: "An upstream stream changed its data or availability; this result is no longer current." } };
  const workspace = { ...sessionWorkspace, nodes: [node], wentStale: [node.id] };
  expect(noteLine(workspace)).toBeUndefined();
  expect(lineText(joined(verdictOf("stale", sessionCells[0]!, [node], sessionNow)))).toContain(node.staleReason.message);
  for (const reason of [undefined, { code: "dependency_changed", message: "Definition changed." }, { code: "restore_not_retained", message: "Not retained." }]) {
    const other = { ...node, id: "other", staleReason: reason };
    expect(lineText(noteLine({ ...workspace, nodes: [node, other], wentStale: [node.id, other.id] })!)).toBe("~ 1 result stale   /stale");
  }
  expect(workspace.nodes[0]!.state).toBe("stale");
});

it("should_KeepAnInputBehindResultOnTheCellWithoutTheWorkspaceBanner_When_ACalculationFinishedAnOlderInput", () => {
  // Arrange
  const node = { ...sessionWorkspace.nodes[0]!, state: "stale" as const,
    staleReason: { code: "input_behind", message: "The calculation completed an older input while newer input arrived; waiting to compute the latest committed input." } };
  const workspace = { ...sessionWorkspace, nodes: [node], wentStale: [node.id] };
  // Act
  const note = noteLine(workspace);
  const verdict = lineText(joined(verdictOf("stale", sessionCells[0]!, [node], sessionNow)));
  // Assert
  expect(note).toBeUndefined();
  expect(verdict).toContain(node.staleReason.message);
});

it("should_SayNewerInputIsWaiting_When_TheEngineMarksTheRunningCalculation", () => {
  // Arrange
  const running = { ...sessionWorkspace.nodes[0]!, state: "running" as const, updatePending: true };
  const other = { ...running, id: "other", updatePending: undefined };
  // Act
  const one = lineText(joined(verdictOf("live", sessionCells[0]!, [running], sessionNow)));
  const several = lineText(joined(verdictOf("live", sessionCells[0]!, [running, other], sessionNow)));
  const quiet = lineText(joined(verdictOf("live", sessionCells[0]!, [other], sessionNow)));
  // Assert
  expect(one).toContain("Newer input waiting; finishing current calculation");
  expect(several).toContain("newer input waiting");
  expect(quiet).not.toMatch(/newer input/i);
  expect(one).not.toMatch(/buffer|lost|record|\d+ (?:inputs|windows|updates)/i);
});


it("preserves corrective hints even when an attempt also supplies the refusal summary", () => {
  const cell = { ...newCell(":inspect total"), diagnostics: [{ code: "RES004", severity: "error" as const,
    message: "Target missing", start: 0, end: 14, hints: ["Did you mean :inspect $total?"] }] };
  const workspace = { ...emptyWorkspace, attemptFailures: { [cell.lastRun]: "RES004: Target missing" } };
  expect(refusalOf(workspace, cell)).toBe("RES004: Target missing\nHint: Did you mean :inspect $total?");
});


it.each(["full", "pane"] as const)("keeps command bands full width across names and cell changes in %s sessions", chromeMode => {
  let tree!: ReactTestRenderer;
  const render = (names: string[]) => <Session model={{ ...model, cells: names.map((label, index) => ({
    ...model.cells[0]!, id: `cell-${index}`, rows: [{ segments: [], nodes: [{ id: label, label, glyph: "ready" as const }] }],
  })) }} chrome="controls" chromeMode={chromeMode} prompt={null} />;
  act(() => { tree = create(render([])); });
  for (const names of [["$a"], ["$errorRate", "$a_very_long_result_name"], ["$services"], []]) {
    act(() => tree.update(render(names)));
    expect(tree.root.findAllByProps({className:"cell-band"})).toHaveLength(names.length);
    for(const band of tree.root.findAllByProps({className:"cell-band"})) {
      expect(band.findAllByProps({className:"cell-command"})).toHaveLength(1);
      expect(band.findAllByProps({className:"cell-name"})).toHaveLength(0);
    }
  }
  act(() => tree.unmount());
});
it("shows named error routing without turning the failed producer into a handled success",()=>{
  const workspace={...sessionWorkspace,nodes:sessionWorkspace.nodes.map(node=>node.state==="failed"?{...node,errorNames:["healthError"]}:node)};
  const cells=readSession({...input,workspace},sessionNow).cells;
  const failure=cells.find(cell=>cell.state==="failed")!;
  expect(lineText(joined(failure.verdict))).toContain("error → $healthError");expect(lineText(joined(failure.verdict))).toContain("failed");
});

it("reveals and focuses only this session's requested definition once per navigation", () => {
  const scrollIntoView = vi.fn(), focus = vi.fn(), register = vi.fn(() => () => {});
  const target = { dataset: { cell: "c2" }, scrollIntoView, focus };
  const surface = { querySelectorAll: () => [target] };
  const definition = { pane: "p2", register };
  const draw = (pane: string, revision: number) => <Session model={model} chrome="keys" prompt={null} definition={definition} jump={{ pane, cell: "c2", revision }} />;
  let tree!: ReactTestRenderer;
  act(() => { tree = create(draw("p1", 1), { createNodeMock: element => element.props.className === "session" ? surface : null }); });
  expect(focus).not.toHaveBeenCalled();
  act(() => tree.update(draw("p2", 2)));
  expect(scrollIntoView).toHaveBeenCalledExactlyOnceWith({ block: "center" });
  expect(focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
  act(() => tree.update(draw("p2", 2)));
  expect(focus).toHaveBeenCalledOnce();
  act(() => tree.update(draw("p2", 3)));
  expect(focus).toHaveBeenCalledTimes(2);
  expect(register).toHaveBeenCalledWith({ pane: "p2", workspace: undefined, cells: model.cells.map(cell => cell.id) });
  act(() => tree.unmount());
});

describe("a submission without an answer or refused before it started", () => {
  const read = (cell: ReturnType<typeof newCell>, workspace = emptyWorkspace) =>
    readSession({ workspace, cells: [cell], context: sessionContext }, sessionNow).cells[0]!;

  it("should_SayOutcomeUnknownWithoutClaimingNothingRan_When_ANodeFreeReplyWasLost", () => {
    // Arrange
    const lost = { ...newCell(":help"), state: "unanswered" as const };
    // Act
    const shown = read(lost);
    // Assert
    expect(lineText(joined(shown.verdict))).toBe("outcome unknown");
    expect(runAvailability(shown.state, shown.verdict, []).repeatVerb).toBe("repeat…");
  });

  it("should_KeepTheReasonInTheVerdictAndOfferRetry_When_AFirstSubmissionWasRefusedBeforeItStarted", () => {
    // Arrange
    const refused = { ...newCell("sandbox s { <b>x</b> }"), state: "answered" as const, submissionRefusal: "SBX004: <b>x</b> is not a member" };
    // Act
    const shown = read(refused);
    // Assert
    expect(shown.state).toBe("failed");
    expect(lineText(joined(shown.verdict))).toBe("refused · SBX004: <b>x</b> is not a member · nothing ran");
    expect(runAvailability(shown.state, shown.verdict, []).repeatVerb).toBe("retry");
  });

  it("should_ShowThePreviousResultsUnderTheRefusal_When_ARepeatWasRefusedBeforeItStarted", () => {
    // Arrange
    const node = nodesOf(sessionWorkspace, sessionCells[1]!)[0]!;
    const refused = { ...sessionCells[1]!, state: "answered" as const, lastRun: "refused-repeat", submissionRefusal: "Pre-execution validation failed" };
    // Act
    const shown = read(refused, { ...emptyWorkspace, nodes: [node] });
    // Assert
    expect(lineText(joined(shown.verdict))).toBe("not run · Pre-execution validation failed · previous results shown");
    expect(shown.previousAttemptResults).toBe(true);
    expect(shown.nodes.map(it => it.id)).toEqual([node.id]);
  });
});

describe("Pin binding status", () => {
  const command = ':view pin $chart instance:"chart-identity" revision:2 inputRevision:5 > chart_pin';
  const publication = { state: "available" as const, run: "r9", handle: "h9", uncertainHandle: null, problem: null, message: "Published" };
  const pinNode = (over: Partial<import("../workspace").WorkspaceNode>) => ({ ...sessionWorkspace.nodes[0]!, id: "chart_pin", name: "chart_pin", command, ...over });

  it("should_SayThePinIsWaitingForRetention_When_ThePinCommandRunsBeforePublication", () => {
    // Arrange
    const node = pinNode({ state: "running" });
    // Act
    const line = lineText(joined(verdictOf("live", sessionCells[1]!, [node], sessionNow)));
    // Assert
    expect(line).toContain("keeping input for Pin…");
    expect(line).not.toContain("pinned");
  });

  it("should_ReportBindingSeparatelyFromRetention_When_TheEngineReportsEachPinOutcome", () => {
    // Arrange
    const pending = pinNode({ state: "ready", kept: true, handle: "h9", publication: { ...publication, pinBinding: { state: "pending" } } });
    const bound = pinNode({ state: "ready", kept: true, handle: "h9", publication: { ...publication, pinBinding: { state: "bound" } } });
    const refused = pinNode({ state: "ready", kept: true, handle: "h9", publication: { ...publication, pinBinding: { state: "refused", problem: "View configuration changed" } } });
    // Act
    const [waiting, done, kept] = [pending, bound, refused].map(node => lineText(joined(verdictOf("default", sessionCells[1]!, [node], sessionNow))));
    // Assert
    expect(waiting).toContain("pinning view…");
    expect(done).toContain("view input pinned");
    expect(kept).toContain("kept");
    expect(kept).toContain("view not pinned: View configuration changed");
    expect(verdictOf("default", sessionCells[1]!, [refused], sessionNow).find(field => lineText(field.segments).startsWith("view not pinned"))?.keep).toBe(true);
  });

  it("should_ShowProgressOnlyForWorkInFlight_When_APinHasNotStartedOrIsPublishing", () => {
    // Arrange
    const waiting = pinNode({ state: "pending" });
    const publishing = pinNode({ state: "ready", publication: { ...publication, state: "pending", handle: null } });
    // Act
    const segments = [pinBindingOf(waiting), pinBindingOf(publishing)];
    // Assert
    expect(segments.map(segment => segment?.text)).toEqual([undefined, "keeping input for Pin…"]);
  });

  it("should_NotClaimAPin_When_AReadyResultHasNoReportedBinding", () => {
    // Arrange
    const plain = { ...sessionWorkspace.nodes[0]!, state: "ready" as const, publication };
    const lostIntent = pinNode({ state: "ready", publication });
    // Act
    const segments = [pinBindingOf(plain), pinBindingOf(lostIntent)];
    // Assert
    expect(segments).toEqual([undefined, undefined]);
  });

  it("should_KeepThePinBindingFromEachPublicationFrame_When_TheReducerAppliesIt", () => {
    // Arrange
    const created = applyEvent(emptyWorkspace, { event: "created", dependencyLifetime: "continuous", node: "chart_pin", name: "chart_pin", command, dependsOn: [], interactive: false });
    // Act
    const ready = applyEvent(created, { event: "ready", node: "chart_pin", type: "Text", handle: "h9", bytes: 3, provenance: {}, cautions: [], kept: true, publication: { ...publication, pinBinding: { state: "refused", problem: "Changed" } } });
    const replaced = applyEvent(ready, { event: "node", constructionComplete: false, node: "chart_pin", state: "running" });
    // Assert
    expect(ready.nodes[0]?.publication?.pinBinding).toEqual({ state: "refused", problem: "Changed" });
    expect(replaced.nodes[0]?.publication).toBeUndefined();
  });
});

it("should_NotListACompletedViewAmongRepeatDependents_When_ItsInputCellIsRepeated", () => {
  // Arrange
  const cell = { ...newCell("acme orders.list"), id: "source-cell", nodes: ["raw"] };
  const raw = { id: "raw", name: "raw", command: "acme orders.list", dependsOn: [], state: "ready" as const, provenance: {}, cautions: [], kept: false, repeatable: true };
  const view = (complete: boolean) => ({ id: "chart", name: "chart", command: ":view create Metric", dependsOn: ["raw"], state: "ready" as const, provenance: {}, cautions: [], kept: false,
    repeatable: false, dependencyLifetime: "creation" as const, constructionComplete: complete || undefined });
  // Act
  const [pending, built] = [false, true].map(complete => guardOf({ ...emptyWorkspace, nodes: [raw, view(complete)] }, cell, [raw], sessionContext));
  // Assert
  expect(pending?.dependents).toEqual(["$chart"]);
  expect(built).toBeUndefined();
});
