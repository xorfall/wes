import { terminalPane } from "./split-model";
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { lineText } from "./MonoLine";
import { Split, paneTitle, splitKeys, splitTop } from "./Split";
import {
  splitPane, arrangement, clearPane, close, cycle, divide, focus, focusNth, nextPane, open, openRemembered, oneP,
  paneFor, SESSION_PANE, sendToPane, template, titleOf, type Pane, type Shown, type SplitState,
} from "./split-model";
import { read } from "./commands";
import { topLine } from "./session-model";

const session: Pane = { id: "p1", title: "session" };
const graph: Pane = { id: "p2", title: "/graph $orders", command: "/graph $orders", shows: { screen: "graph" } };
const opened: Pane = { id: "p3", title: "/open acme orders.list", command: "/open acme orders.list", shows: { screen: "open", node: "orders" } };
const env: Pane = { id: "p4", title: "/env DEV", command: "/env DEV", shows: { screen: "env" } };

const two = open(oneP(session), graph);
const three = splitPane(two, "down", opened, true);
const four = splitPane(three, "down", env, true, "p1");

const top = topLine({ workspace: "sales-api", environment: "DEV", connection: "connected" });

function textOf(node: { children: readonly unknown[] }): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : textOf(child as { children: readonly unknown[] })))
    .join("");
}

function draw(state: SplitState, onChange = vi.fn()): { tree: ReactTestRenderer; onChange: typeof onChange } {
  let tree: ReactTestRenderer | undefined;
  act(() => {
    tree = create(
      <Split
        state={state}
        top={top}
        prompt={[{ text: "❯ ", role: "mono-ref-strong" }]}
        context={[{ text: "DEV", role: "mono-meta" }]}
        onChange={onChange}
        content={(pane: Pane) => <span>{`the screen's own content, in this pane (${pane.id})`}</span>}
      />,
    );
  });
  return { tree: tree!, onChange };
}

const press = (tree: ReactTestRenderer, key: string, held: Record<string, boolean | string> = {}) =>
  act(() =>
    tree.root.findByProps({ tabIndex: 0, className: "split" }).props.onKeyDown({
      key, altKey: false, ctrlKey: false, shiftKey: false, ...held,
      preventDefault() {}, stopPropagation() {},
    }),
  );

describe("the three arrangements", () => {
  it("should_LayTwoPanesSideBySide_When_TheSecondOpens", () => {
    expect(arrangement(two)).toBe(2);
    expect(template(two)).toBe('"p1 p2"');
  });

  it("should_StackThem_When_TheSplitWasDownwards", () => {
    expect(template(open(oneP(session), graph, "down"))).toBe('"p1" "p2"');
  });

  it("should_KeepTheFirstPaneWholeBesideTwoStacked_When_ThereAreThree", () => {
    expect(arrangement(three)).toBe(3);
    expect(template(three)).toBe('"p1 p2" "p1 p3"');
  });

  it("should_GiveAQuarterEach_When_ThereAreFour", () => {
    expect(arrangement(four)).toBe(4);
    expect(template(four)).toBe('"p1 p2" "p4 p3"');
  });

  it("should_LeaveExistingWorkUntouched_When_AFifthIsAskedFor", () => {
    const fifth = open(four, { id: "p5", title: "/settings", command: "/settings" });
    expect(fifth.panes).toHaveLength(4);
    expect(fifth).toBe(four);
  });
});

describe("which pane is focused", () => {
  it("should_FocusWhatJustOpened_When_APaneIsAdded", () => {
    expect(two.focused).toBe("p2");
    expect(four.focused).toBe("p4");
  });

  it("should_WrapRound_When_FocusIsCycled", () => {
    expect(cycle(three, 1).focused).toBe("p1");
    expect(cycle(cycle(three, 1), -1).focused).toBe("p3");
  });

  it("should_GoStraightToAPane_When_ItIsAskedForByNumber", () => {
    expect(focusNth(four, 2).focused).toBe("p2");
    expect(focusNth(four, 9).focused).toBe(four.focused);
  });

  it("should_ChangeNothing_When_AskedToFocusAPaneThatIsNotThere", () => {
    expect(focus(two, "nowhere")).toBe(two);
  });
});

describe("closing a pane", () => {
  it("should_TakeTheFocusWithIt_When_TheFocusedPaneCloses", () => {
    const left = close(three, "p3");
    expect(left.panes.map((pane) => pane.id)).toEqual(["p1", "p2"]);
    expect(left.focused).toBe("p2");
  });

  it("should_LeaveTheFocusAlone_When_AnotherPaneCloses", () => {
    expect(close(three, "p2").focused).toBe("p3");
  });

  it("should_RefuseToCloseTheLastOne_When_ThereIsOnlyOne", () => {
    expect(close(oneP(session), "p1")).toEqual(oneP(session));
  });

  it("keeps the final command context intact beside terminals and related-work projections", () => {
    let state = splitPane(oneP(session), "right", terminalPane("p2"), true);
    state = splitPane(state, "down", { id: "p3", title: "related", value: { node: "result", generation: "fixture", label: "result", related: true } });
    state = splitPane(state, "left", terminalPane("p4"));
    expect(close(state, "p1")).toBe(state);
    expect(close(focus(state, "p1"), "p1").focused).toBe("p1");
    expect(close(state, "p2").panes.map(p => p.id)).toEqual(["p1", "p3", "p4"]);
    expect(close(state, "p3").panes.map(p => p.id)).toEqual(["p1", "p2", "p4"]);
  });

  it("counts dismissible screens as command contexts and protects their content when last", () => {
    const state = splitPane(two, "down", terminalPane("p3"));
    const remaining = close(state, "p1");
    expect(remaining.panes.map(p => p.id)).toEqual(["p2", "p3"]);
    expect(close(remaining, "p2")).toBe(remaining);
    expect(remaining.panes[0]).toEqual(graph);
    expect(paneFor(remaining, graph.command!)).toBe("p2");
    expect(clearPane(remaining, "p2").panes[0]).toEqual({ id: "p2", title: "session" });
  });

  it("lets either of two sessions close and then protects the survivor regardless of identity", () => {
    const state = divide(oneP(session), "right");
    for (const id of ["p1", "p2"]) {
      const remaining = close(state, id);
      expect(remaining.panes).toHaveLength(1);
      expect(remaining.panes[0]!.id).not.toBe(id);
      const withShell = splitPane(remaining, "right", terminalPane("p3"));
      expect(close(withShell, remaining.panes[0]!.id)).toBe(withShell);
    }
  });
});

describe("the pane a command opens in", () => {
  it("should_BeRemembered_When_TheCommandOpensAPane", () => {
    expect(paneFor(three, "/graph $orders")).toBe("p2");
    expect(paneFor(three, "/settings")).toBeUndefined();
  });

  it("should_GoBackToTheSamePane_When_TheCommandIsRunAgain", () => {
    const again = openRemembered(three, { id: "fresh", title: "/graph $daily", command: "/graph $orders" });
    expect(again.panes).toHaveLength(3);
    expect(again.focused).toBe("p2");
    expect(again.panes.find((pane) => pane.id === "p2")?.title).toBe("/graph $daily");
  });

  it("should_OpenAPane_When_TheCommandHasNotBeenHereBefore", () => {
    const next = openRemembered(two, { id: "p3", title: "/env DEV", command: "/env DEV" });
    expect(next.panes).toHaveLength(3);
    expect(next.focused).toBe("p3");
  });

  it("should_ForgetAPaneThatIsGone_When_ItWasClosed", () => {
    expect(paneFor(close(three, "p2"), "/graph $orders")).toBeUndefined();
  });
});

describe("the split screen", () => {
  it("should_SayHowManyPanesThereAre_When_TheTopLineIsWritten", () => {
    expect(lineText(splitTop(top, 3))).toBe("wes  /  sales-api  ·  3 panes");
    expect(lineText(splitTop(top, 1))).toBe("wes  /  sales-api  ·  1 pane");
  });

  it("should_PointAtTheFocusedPane_When_TheTitlesAreWritten", () => {
    expect(lineText(paneTitle(session, true))).toBe("▸ session");
    expect(lineText(paneTitle(graph, false))).toBe("  /graph $orders");
  });

  it("should_OmitPaneCyclingCopy_When_TheFooterIsWritten", () => {
    expect(lineText(splitKeys()))
      .toBe("");
  });

  it("should_DrawEachPaneInItsArea_When_TheArrangementIsThree", () => {
    const { tree } = draw(three);
    const panes = tree.root.findAllByType("section");
    expect(panes.map((pane) => String(pane.props["aria-label"])))
      .toEqual(["session", "/graph $orders", "/open acme orders.list"]);
    expect(panes.map((pane) => pane.props.style.gridArea)).toEqual(["p1", "p2", "p3"]);
    expect(tree.root.findByProps({ className: "split-panes" }).props.style.gridTemplateAreas)
      .toBe('"p1 p2" "p1 p3"');
    act(() => tree.unmount());
  });

  it("should_SinkOnlyTheFocusedPane_When_TheScreenIsDrawn", () => {
    const { tree } = draw(three);
    const sunk = tree.root.findAllByType("section").filter((pane) => String(pane.props.className).includes("surface-sunk"));
    expect(sunk.map((pane) => String(pane.props["aria-label"]))).toEqual(["/open acme orders.list"]);
    act(() => tree.unmount());
  });

  it("should_MoveBetweenPanes_When_TheKeysArePressed", () => {
    const { tree, onChange } = draw(three);
    press(tree, "ArrowRight", { altKey: true });
    expect(onChange).toHaveBeenLastCalledWith(cycle(three, 1));
    press(tree, "ArrowLeft", { altKey: true });
    expect(onChange).toHaveBeenLastCalledWith(cycle(three, -1));
    press(tree, "Tab", { ctrlKey: true });
    expect(onChange).toHaveBeenLastCalledWith(cycle(three, 1));
    press(tree, "2", { altKey: true });
    expect(onChange).toHaveBeenLastCalledWith(focusNth(three, 2));
    act(() => tree.unmount());
  });

  it("should_CloseThisPane_When_AltAndWArePressed", () => {
    const { tree, onChange } = draw(three);
    press(tree, "w", { altKey: true });
    expect(onChange).toHaveBeenLastCalledWith(close(three, "p3"));
    act(() => tree.unmount());
  });

  it.each(["input", "textarea", "contenteditable descendant"])("leaves Option-arrow editing with %s", kind => {
    const { tree, onChange } = draw(three);
    const target = {
      isContentEditable: kind === "contenteditable descendant",
      closest: (selector: string) => selector.split(", ").includes(kind) ? target : null,
    };
    for (const key of ["ArrowLeft", "ArrowRight"]) for (const shiftKey of [false, true]) {
      const event = { key, altKey: true, shiftKey, target, preventDefault: vi.fn(), stopPropagation: vi.fn() };
      act(() => tree.root.findByProps({ className: "split" }).props.onKeyDown(event));
      expect(event.preventDefault).not.toHaveBeenCalled();
      expect(event.stopPropagation).not.toHaveBeenCalled();
    }
    expect(onChange).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  describe("Option chords on a Mac", () => {
    afterEach(() => vi.unstubAllGlobals());

    // Option rewrites `key` on a Mac: these are what a US layout reports for Option+W and Option+2.
    it.each([["KeyW", "∑", "close"], ["Digit2", "™", "focus"]])("reads the physical %s, not %j", (code, key, does) => {
      vi.stubGlobal("navigator", { platform: "MacIntel" });
      const { tree, onChange } = draw(three);
      press(tree, key, { altKey: true, code });
      expect(onChange).toHaveBeenLastCalledWith(does === "close" ? close(three, "p3") : focusNth(three, 2));
      act(() => tree.unmount());
    });

    it("follows the layout's key off Apple platforms", () => {
      vi.stubGlobal("navigator", { platform: "Win32" });
      const { tree, onChange } = draw(three);
      press(tree, "z", { altKey: true, code: "KeyW" });
      expect(onChange).not.toHaveBeenCalled();
      press(tree, "w", { altKey: true, code: "KeyZ" });
      expect(onChange).toHaveBeenLastCalledWith(close(three, "p3"));
      act(() => tree.unmount());
    });
  });

  it.each<Record<string, boolean>>([{ ctrlKey: true }, { metaKey: true }, { shiftKey: true }, { ctrlKey: true, metaKey: true }])("leaves Alt+W and Alt+2 alone with more held (%j)", held => {
    const { tree, onChange } = draw(three);
    press(tree, "w", { altKey: true, ...held });
    press(tree, "2", { altKey: true, ...held });
    expect(onChange).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it.each(["input", "textarea", "contenteditable descendant"])("leaves Alt+W and Alt+1..4 to text editing in %s", kind => {
    const { tree, onChange } = draw(three);
    const target = {
      isContentEditable: kind === "contenteditable descendant",
      closest: (selector: string) => selector.split(", ").includes(kind) ? target : null,
    };
    for (const key of ["w", "1", "2", "3", "4"]) {
      const event = { key, altKey: true, ctrlKey: false, metaKey: false, shiftKey: false, target, preventDefault: vi.fn(), stopPropagation: vi.fn() };
      act(() => tree.root.findByProps({ className: "split" }).props.onKeyDown(event));
      expect(event.preventDefault).not.toHaveBeenCalled();
    }
    expect(onChange).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it.each([{ nativeEvent: { isComposing: true } }, { isComposing: true }, { keyCode: 229 }])("leaves pane keys to an input method's composition (%j)", composition => {
    const { tree, onChange } = draw(three);
    const split = tree.root.findByProps({ className: "split" });
    for (const key of ["w", "2", "ArrowRight"]) {
      act(() => split.props.onKeyDown({ key, altKey: true, ctrlKey: false, metaKey: false, shiftKey: false, ...composition, preventDefault: vi.fn(), stopPropagation: vi.fn() }));
    }
    const escape = { key: "Escape", altKey: false, ctrlKey: false, metaKey: false, shiftKey: false, ...composition, preventDefault: vi.fn(), stopPropagation: vi.fn() };
    act(() => split.props.onKeyDownCapture(escape));
    expect(escape.preventDefault).not.toHaveBeenCalled();
    expect(onChange).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it.each([{ nativeEvent: { isComposing: true } }, { isComposing: true }, { keyCode: 229 }])("keeps a pane's screen through a composing Escape that propagates from capture to bubbling (%j)", composition => {
    const state = focus(two, "p2");
    const { tree, onChange } = draw(state);
    const split = tree.root.findByProps({ className: "split" });
    const key = (key: string, extra: object = {}) => ({ key, altKey: false, ctrlKey: false, metaKey: false, shiftKey: false, defaultPrevented: false, preventDefault: vi.fn(), stopPropagation: vi.fn(), ...extra });
    // The same event object travels through capture and then bubbling, as the browser delivers it.
    for (const composed of [key("Escape", composition), key("Tab", { ctrlKey: true, ...composition })]) {
      act(() => split.props.onKeyDownCapture(composed));
      act(() => split.props.onKeyDown(composed));
      expect(composed.preventDefault).not.toHaveBeenCalled();
    }
    expect(onChange).not.toHaveBeenCalled();
    const plain = key("Escape");
    act(() => split.props.onKeyDownCapture(plain));
    act(() => split.props.onKeyDown(plain));
    expect(onChange).toHaveBeenCalledExactlyOnceWith(clearPane(state, "p2"));
    act(() => tree.unmount());
  });

  it("leaves bubbling Escape and Ctrl+Tab alone while the split is inactive", () => {
    const state = focus(two, "p2");
    const onChange = vi.fn();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Split state={state} top={top} prompt={[]} context={[]} onChange={onChange} active={false} />); });
    press(tree, "Escape");
    press(tree, "Tab", { ctrlKey: true });
    expect(onChange).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("keeps the final wes pane when Alt+W is pressed with an xterm beside it", () => {
    const state = splitPane(oneP(session), "right", terminalPane("p2"));
    const { tree, onChange } = draw(state);
    press(tree, "w", { altKey: true });
    expect(onChange).toHaveBeenCalledExactlyOnceWith(state);
    expect(onChange.mock.calls[0]![0]).toBe(state);
    act(() => tree.unmount());
  });

  it("should_LeaveAKeyAlone_When_NoModifierIsHeld", () => {
    const { tree, onChange } = draw(three);
    press(tree, "2");
    press(tree, "w");
    expect(onChange).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("should_DrawWhatEachPaneHolds_When_ContentIsGiven", () => {
    const { tree } = draw(three);
    const said = tree.root.findAllByType("section").map((pane) => textOf(pane));
    expect(said[1]).toContain("the screen's own content, in this pane (p2)");
    act(() => tree.unmount());
  });
});

/*
 * Dividing the workspace, said out loud.
 *
 * Workspace controls expose splitting, closing and sending screens to panes.
 */
describe("the settings on the top line", () => {
  it("should_OpenTheSettings_When_TheGlyphAtTheRightIsPressed", () => {
    // Arrange
    const onSettings = vi.fn();
    let tree: ReactTestRenderer | undefined;
    act(() => { tree = create(<Split state={two} top={top} prompt={[]} context={[]} onSettings={onSettings} />); });
    const button = tree!.root.findByProps({ "aria-label": "Settings" });
    // Act
    act(() => button.props.onClick({ stopPropagation() {} }));
    // Assert: the press reaches the owner and never the pane under it; the glyph says what it is
    expect(onSettings).toHaveBeenCalledOnce();
    expect(button.props["data-hint"]).toBeUndefined();
    expect(button.props["aria-description"]).toBe("/settings");
    act(() => tree!.unmount());
  });

  it("should_OfferNoGlyph_When_NobodyAnswersIt", () => {
    const { tree } = draw(two);
    expect(tree.root.findAllByProps({ "aria-label": "Settings" })).toEqual([]);
    act(() => tree.unmount());
  });
});

describe("the commands that divide the workspace", () => {
  it("should_DivideIt_When_SplitIsTyped", () => {
    expect(read("/split")).toEqual({ kind: "split", how: "right" });
    expect(read("/split right")).toEqual({ kind: "split", how: "right" });
    expect(read("/split down")).toEqual({ kind: "split", how: "down" });
    expect(read("/split 3")).toEqual({ kind: "split", how: 3 });
    expect(read("/split 4")).toEqual({ kind: "split", how: 4 });
  });

  it("should_SayWhatItTakes_When_SplitIsGivenSomethingElse", () => {
    expect(read("/split sideways")).toEqual({ kind: "trouble", said: "'/split' takes right, down, 3 or 4" });
    expect(read("/split 5")).toMatchObject({ kind: "trouble" });
  });

  it("should_CloseTheFocusedPane_When_CloseIsTyped", () => {
    expect(read("/close")).toEqual({ kind: "close" });
  });

  it("should_MakeAsManyPanesAsWereAskedFor_When_TheWorkspaceIsDivided", () => {
    const one = oneP(SESSION_PANE);
    expect(divide(one, "right").panes).toHaveLength(2);
    expect(divide(one, "right").axis).toBe("right");
    expect(divide(one, "down").axis).toBe("down");
    expect(divide(one, 3).panes).toHaveLength(3);
    expect(divide(one, 4).panes).toHaveLength(4);
    // The session keeps the pane it is in: dividing the workspace is not leaving it.
    expect(divide(one, 4).panes[0]).toEqual(SESSION_PANE);
  });

  /** Asking for four when there are two adds two: what is in them is somebody's work. */
  it("should_KeepWhatIsAlreadyThere_When_ItIsDividedAgain", () => {
    const two = divide(oneP(SESSION_PANE), "right");
    const named = { ...two, panes: [two.panes[0]!, { ...two.panes[1]!, title: "/graph" }] };
    const four = divide(named, 4);
    expect(four.panes).toHaveLength(4);
    expect(four.panes[1]!.title).toBe("/graph");
  });

  it("should_NameAPaneNothingElseHas_When_AnotherIsOpened", () => {
    const one = oneP(SESSION_PANE);
    expect(nextPane(one)).toBe("p2");
    expect(nextPane(divide(one, 3))).toBe("p4");
  });

  it("should_NeverCloseTheLast_When_ThereIsOnlyOnePane", () => {
    const one = oneP(SESSION_PANE);
    expect(close(one, one.focused)).toBe(one);
    const two = divide(one, "right");
    expect(close(two, two.focused).panes).toHaveLength(1);
  });
});

/*
 * `… split` on the end of any screen command.
 *
 * The suffix is taken off before the command reads its own words, so what it names is untouched:
 * `/settings connections split` is still about connections and `/open $x split` still names `$x`.
 */
describe("sending a screen to a pane", () => {
  it("should_OpenInAPane_When_AnyScreenCommandIsSuffixed", () => {
    expect(read("/graph split")).toEqual({ kind: "screen", screen: "graph", inPane: true });
    expect(read("/stale split")).toEqual({ kind: "screen", screen: "stale", inPane: true });
    expect(read("/env split")).toEqual({ kind: "screen", screen: "env", inPane: true });
    expect(read("/edit split")).toEqual({ kind: "screen", screen: "edit", inPane: true });
    expect(read("/settings split")).toEqual({ kind: "screen", screen: "settings", section: "appearance", inPane: true });
  });

  it("should_StillNameWhatItNames_When_TheSuffixIsTakenOff", () => {
    expect(read("/open $orders split")).toEqual({ kind: "screen", screen: "open", node: "orders", inPane: true });
    expect(read("/edit $revenue split")).toEqual({ kind: "screen", screen: "edit", node: "revenue", inPane: true });
    expect(read("/settings connections split"))
      .toEqual({ kind: "screen", screen: "settings", section: "connections", inPane: true });
  });

  it("should_OpenOverTheWorkspace_When_ThereIsNoSuffix", () => {
    expect(read("/graph")).toEqual({ kind: "screen", screen: "graph" });
    expect(read("/open $orders")).toEqual({ kind: "screen", screen: "open", node: "orders" });
  });

  /** `split` is the suffix, and `/split` is the command; neither is ever read as the other. */
  it("should_NotReadItselfAsItsOwnSuffix_When_SplitIsTheCommand", () => {
    expect(read("/split")).toEqual({ kind: "split", how: "right" });
    expect(read("/split right")).toEqual({ kind: "split", how: "right" });
  });

  /*
   * Remembered first, a new pane next, the focused one last.
   *
   * The remembering is the point of the mode: an arrangement somebody built stays built, and they
   * stop having to say where things go.
   */
  it("should_RememberWhereACommandOpened_When_ItIsSentToAPaneAgain", () => {
    const one = oneP(SESSION_PANE);
    const graph = (state: SplitState) =>
      openRemembered(state, { id: nextPane(state), title: "/graph", command: "graph", shows: { screen: "graph" } });

    const sent = graph(one);
    expect(sent.panes).toHaveLength(2);
    expect(paneFor(sent, "graph")).toBe("p2");

    // The second `/graph split` lands in the same pane rather than making another.
    const again = graph(sent);
    expect(again.panes).toHaveLength(2);
    expect(again.focused).toBe("p2");
    expect(paneFor(again, "graph")).toBe("p2");

    // A different command gets a pane of its own while there is room for one.
    const env = openRemembered(again, { id: nextPane(again), title: "/env", command: "env", shows: { screen: "env" } });
    expect(env.panes).toHaveLength(3);
    expect(paneFor(env, "env")).toBe("p3");
    expect(paneFor(env, "graph")).toBe("p2");
  });

  it("should_CarryWhatThePaneShows_When_AScreenIsPutInOne", () => {
    const sent = openRemembered(oneP(SESSION_PANE), {
      id: "p2", title: "/open $orders", command: "open", shows: { screen: "open", node: "orders", tab: "details" },
    });
    expect(sent.panes[1]!.shows).toEqual({ screen: "open", node: "orders", tab: "details" });
    // A pane with nothing to show is the session, which is what a pane holds by default.
    expect(sent.panes[0]!.shows).toBeUndefined();
  });

  /** A pane named the same as one already there is that pane, told to show something else. */
  it("should_ReplaceWhatIsInIt_When_APaneIsOpenedByAnIdThatExists", () => {
    const two = divide(oneP(SESSION_PANE), "right");
    const sent = open(two, { id: "p2", title: "/graph", command: "graph", shows: { screen: "graph" } });
    expect(sent.panes).toHaveLength(2);
    expect(sent.panes[1]!.shows).toEqual({ screen: "graph" });
    expect(sent.focused).toBe("p2");
  });
});

/*
 * Where a suffixed screen command lands.
 *
 * Decided here rather than in the wiring, so the rule is one pure function with the whole of its
 * behaviour written down: remembered pane first, a new one while there is room, the focused one at
 * four. Whoever wires `/graph split` calls this and draws what comes back.
 */
describe("routing a screen to a pane", () => {
  const shown = (screen: Shown["screen"], rest: Partial<Shown> = {}): Shown => ({ screen, ...rest });
  const one = oneP(SESSION_PANE);

  it("should_SayWhatItHolds_When_APaneIsTitled", () => {
    expect(titleOf(undefined)).toBe("session");
    expect(titleOf(shown("graph"))).toBe("/graph");
    expect(titleOf(shown("open", { node: "orders" }))).toBe("/open $orders");
    expect(titleOf(shown("settings", { section: "connections" }))).toBe("/settings connections");
    expect(titleOf(shown("edit", { node: "revenue" }))).toBe("/edit $revenue");
  });

  it("should_OpenAPaneOfItsOwn_When_ThereIsRoomAndNothingIsRemembered", () => {
    const sent = sendToPane(one, shown("graph"));
    expect(sent.panes).toHaveLength(2);
    expect(sent.panes[1]).toMatchObject({ id: "p2", title: "/graph", command: "graph" });
    expect(sent.focused).toBe("p2");
    // The session keeps its own pane: sending a screen somewhere is not leaving the session.
    expect(sent.panes[0]).toEqual(SESSION_PANE);
  });

  it("should_GoBackToTheSamePane_When_TheSameScreenIsSentAgain", () => {
    const first = sendToPane(one, shown("graph"));
    const moved = focus(first, "p1");
    const again = sendToPane(moved, shown("graph"));
    expect(again.panes).toHaveLength(2);
    expect(again.focused).toBe("p2");
  });

  /** The same screen looking at different things is one screen, and shares one pane. */
  it("should_ShareOnePane_When_OneScreenLooksAtTwoThings", () => {
    const orders = sendToPane(one, shown("open", { node: "orders" }));
    const note = sendToPane(orders, shown("open", { node: "note" }));
    expect(note.panes).toHaveLength(2);
    expect(note.panes[1]).toMatchObject({ title: "/open $note", shows: { screen: "open", node: "note" } });
  });

  it("should_GiveEachScreenItsOwnPane_When_ThereIsStillRoom", () => {
    let state = sendToPane(one, shown("graph"));
    state = sendToPane(state, shown("env"));
    state = sendToPane(state, shown("open", { node: "orders" }));
    expect(state.panes.map((pane) => pane.title)).toEqual(["session", "/graph", "/env", "/open $orders"]);
    expect(paneFor(state, "graph")).toBe("p2");
    expect(paneFor(state, "env")).toBe("p3");
  });

  /** The pane limit is four; a fifth screen must be refused without replacing an existing pane. */
  it("should_RefuseAnotherScreen_When_ThereIsNoRoomLeft", () => {
    let state = sendToPane(one, shown("graph"));
    state = sendToPane(state, shown("env"));
    state = sendToPane(state, shown("open"));
    state = focus(state, "p3");
    const settings = sendToPane(state, shown("settings", { section: "data" }));
    expect(settings.panes).toHaveLength(4);
    expect(settings).toBe(state);
    expect(paneFor(settings, "settings")).toBeUndefined();
    // And `/env`, whose pane was taken, is no longer remembered anywhere it is not.
    expect(paneFor(settings, "env")).toBe("p3");
  });

  it("should_ShowTheSessionAgain_When_TheScreenInAPaneIsClosed", () => {
    const sent = sendToPane(one, shown("graph"));
    const cleared = clearPane(sent, "p2");
    expect(cleared.panes).toHaveLength(2);
    expect(cleared.panes[1]).toEqual({ id: "p2", title: "session" });
    // The pane is still there: closing a screen is not giving the room back, `/close` is.
    expect(cleared.panes[1]!.shows).toBeUndefined();
  });

  it("should_ChangeNothing_When_APaneThatIsNotThereIsCleared", () => {
    const sent = sendToPane(one, shown("graph"));
    expect(clearPane(sent, "p9").panes).toEqual(sent.panes);
  });
});


it("keeps execution context at session prompts rather than duplicating it below the panes", () => {
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<Split state={oneP(session)} top={top} prompt={[]} context={[{ text: "env: default", role: "mono-meta" }]} />); });
  expect(tree.root.findAllByProps({ className: "split-context" })).toHaveLength(0);
  act(() => tree.unmount());
});

it("keeps the workspace summary beside navigation in normal and expanded footers", () => {
  const status = [{ text: "~ 2 results stale   /stale", role: "mono-warn" as const }];
  expect(lineText(splitKeys(undefined, status))).toBe("~ 2 results stale   /stale");
  expect(lineText(splitKeys(3, status))).toBe("esc show all 3 panes  ·  ~ 2 results stale   /stale");
  expect(lineText(splitKeys(3, []))).toBe("esc show all 3 panes");
});
