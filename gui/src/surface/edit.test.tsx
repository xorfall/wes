import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { bundledPackage, readLanguage } from "./language";
import { lineText, MonoLine } from "./MonoLine";
import { boundName, editHead, editingTop, editKeys, outputHead, ranAt, type Bound } from "./edit-model";
import { EditScreen } from "./screens/Edit";
import { read } from "./commands";
import { readSession } from "./session-model";
import { sessionCells, sessionContext, sessionNow, sessionWorkspace } from "./session-fixture";
import { topLine } from "./session-model";

const language = readLanguage(bundledPackage, "engine");
const top = topLine({ ...sessionContext, connection: "connected" });
const bound: Bound = { node: "id7", name: "revenue", ran: "09:16" };

const program = [
  ":calc {",
  '  const paid = $orders.filter(o => o.status == "paid");',
  "  if (paid !== none) return paid.count();",
  "} > revenue",
].join("\n");

function textOf(node: { children: readonly unknown[] }): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : textOf(child as { children: readonly unknown[] })))
    .join("");
}

function draw(props: Partial<Parameters<typeof EditScreen>[0]> = {}): ReactTestRenderer {
  let tree: ReactTestRenderer | undefined;
  act(() => {
    tree = create(
      <EditScreen
        top={top}
        source={program}
        language={language}
        names={["orders", "revenue"]}
        context={[{ text: "DEV", role: "mono-meta" }]}
        onChange={() => undefined}
        onRun={() => undefined}
        onRunAgain={() => undefined}
        onClose={() => undefined}
        {...props}
      />,
    );
  });
  return tree!;
}

// The hints name the platform's primary modifier; these sentences are written for a Mac.
beforeEach(() => vi.stubGlobal("navigator", { platform: "MacIntel" }));
afterEach(() => vi.unstubAllGlobals());

const lines = (tree: ReactTestRenderer) => tree.root.findAllByType("pre").map(textOf);
/** The caret rows only — a key in the footer is not an accusation. */
const carets = (tree: ReactTestRenderer) =>
  tree.root.findAllByType("pre").filter((pre) => String(pre.props.className).includes("editor-carets")).map(textOf);

/*
 * Editor labels projected from session state.
 */
describe("what /edit says about itself", () => {
  it("should_NameWhatIsBoundAndHowToRunIt_When_TheHeadIsWritten", () => {
    // The screen's chrome prints `/edit` beside this, as it does for every summoned screen.
    expect(lineText(editHead(bound)))
      .toBe("$revenue   ⌘⏎ run  ·  ⌘R run again  ·  esc back to the session, the draft kept");
  });

  /** Nothing has run, so there is nothing to run again and the key is not offered. */
  it("should_OfferOnlyRun_When_NothingHasBeenRunYet", () => {
    expect(lineText(editHead(undefined)))
      .toBe("⌘⏎ run  ·  esc back to the session, the draft kept");
  });

  it("should_SayWhatIsBeingEdited_When_TheTopLineIsWritten", () => {
    expect(lineText(editingTop(top, bound))).toContain("·  editing $revenue");
    expect(lineText(editingTop(top, undefined))).toBe(lineText(top));
  });

  /** The pane is not a second result: it is the same node, and the line says so. */
  it("should_SayThePaneAndTheCellAreOneNode_When_SomethingIsBound", () => {
    expect(lineText(outputHead(bound)))
      .toBe("output   $revenue  ·  ran 09:16  ·  ⌘R runs the same node again  ·  the scrollback's cell updates with it");
  });

  it("should_SayItHasNotRun_When_NothingIsBound", () => {
    expect(lineText(outputHead(undefined))).toBe("output   not run yet  ·  ⌘⏎ runs it");
  });

  it("should_NameTheKeys_When_TheFooterIsWritten", () => {
    expect(lineText(editKeys()))
      .toBe("⌘⏎ run   ⌘R run again   ⇧⏎ newline   ⌘/ comment   ⇥ indent   ⌃space complete   esc back to the session, the draft kept");
  });

  /** CodeMirror's `Mod-` is Ctrl off Apple platforms, so every hint names Ctrl there; nothing else is offered. */
  it("should_NameCtrl_When_ThePlatformIsNotApple", () => {
    vi.stubGlobal("navigator", { platform: "Win32" });
    expect(lineText(editKeys()))
      .toBe("⌃⏎ run   ⌃R run again   ⇧⏎ newline   ⌃/ comment   ⇥ indent   ⌃space complete   esc back to the session, the draft kept");
    expect(lineText(editHead(bound))).toBe("$revenue   ⌃⏎ run  ·  ⌃R run again  ·  esc back to the session, the draft kept");
    expect(lineText(outputHead(undefined))).toBe("output   not run yet  ·  ⌃⏎ runs it");
    expect(lineText(outputHead(bound))).toContain("⌃R runs the same node again");
    expect(lineText(editKeys())).not.toMatch(/palette|tab complete/);
  });

  /** A run the engine refused made a cell and no result, and the head says that rather than an id. */
  it("should_SayNoResultWasMade_When_TheRunWasRefused", () => {
    expect(lineText(outputHead({ ran: "09:16" })))
      .toBe("output   it made no result  ·  ran 09:16  ·  ⌘R runs it again  ·  the scrollback's cell updates with it");
    expect(lineText(editHead({ ran: "09:16" })))
      .toBe("⌘⏎ run  ·  ⌘R run again  ·  esc back to the session, the draft kept");
  });

  it("should_CallItByItsNameOrItsId_When_AResultIsReferredTo", () => {
    expect(boundName(bound)).toBe("$revenue");
    expect(boundName({ node: "id9" })).toBe("id9");
    expect(boundName({ ran: "09:16" })).toBeUndefined();
    expect(boundName(undefined)).toBeUndefined();
    expect(ranAt("2026-09-20T09:16:00.000Z")).toMatch(/^\d\d:\d\d$/);
    expect(ranAt(undefined)).toBeUndefined();
    expect(ranAt("not a time")).toBeUndefined();
  });
});

describe("the /edit screen", () => {
  it("should_DrawTheHeadTheOutputHeadAndTheFooter_When_ItIsOpened", () => {
    const tree = draw({ bound });
    const said = lines(tree);
    expect(said).toContain("$revenue   ⌘⏎ run  ·  ⌘R run again  ·  esc back to the session, the draft kept");
    expect(said.some((line) => line.startsWith("output   $revenue"))).toBe(true);
    expect(said).toContain("⌘⏎ run   ⌘R run again   ⇧⏎ newline   ⌘/ comment   ⇥ indent   ⌃space complete   esc back to the session, the draft kept");
    act(() => tree.unmount());
  });

  /*
   * The mistake row sits under the block, pointing at the token's own column — never a line under
   * the expression, because a line says "somewhere in here" and the package knows exactly which.
   */
  it("should_PointAtTheOffendingTokensColumn_When_TheProgramHasAMistake", () => {
    const tree = draw();
    const said = lines(tree);
    expect(carets(tree)).toEqual([`${" ".repeat(11)}^^^`]);
    expect(said).toContain("no !== in this language   inequality is !=, and absence is none — read it with isSome()");
    act(() => tree.unmount());
  });

  it("should_DrawNoRow_When_TheProgramIsWhatTheLanguageAccepts", () => {
    const tree = draw({ source: ":calc { return $orders.count(); } > total" });
    expect(carets(tree)).toEqual([]);
    act(() => tree.unmount());
  });

  it("should_ShowTheBoundNodesOwnCell_When_SomethingHasRun", () => {
    const tree = draw({
      bound,
      output: <MonoLine segments={[{ text: "ok · scalar · kept" }]} className="cell-verdict" />,
    });
    expect(lines(tree)).toContain("ok · scalar · kept");
    act(() => tree.unmount());
  });

  /** `esc` goes back to the session and takes nothing with it: the draft is the session's to keep. */
  it("should_LeaveWithoutChangingTheDraft_When_EscapeIsPressed", () => {
    const onClose = vi.fn();
    const onChange = vi.fn();
    const tree = draw({ onClose, onChange });
    act(() => {
      tree.root.findByProps({ className: "screen" }).props.onKeyDown({
        key: "Escape", preventDefault() {}, stopPropagation() {},
      });
    });
    expect(onClose).toHaveBeenCalledOnce();
    expect(onChange).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("should_BeSummonedByName_When_TheCommandIsTyped", () => {
    expect(read("/edit")).toEqual({ kind: "screen", screen: "edit" });
    expect(read("/edit $revenue")).toEqual({ kind: "screen", screen: "edit", node: "revenue" });
    expect(read("/edit id7")).toEqual({ kind: "screen", screen: "edit", node: "id7" });
  });

  it("should_OpenANamedFileBuffer_When_TheContextIsEnvTypesOrViews", () => {
    for (const context of ["env", "types"] as const) {
      expect(read(`/edit ${context}`)).toEqual({ kind: "screen", screen: "edit", fileContext: context });
      expect(read(`/edit ${context} monitor`)).toEqual({
        kind: "screen", screen: "edit", fileContext: context, fileName: "monitor",
      });
    }
  });

  it("should_CarryTheFileContextIntoAPane_When_SplitOpensIt", () => {
    expect(read("/rsplit /edit types monitor")).toEqual({
      kind: "directional-split", direction: "right", takeFocus: false,
      content: { screen: "edit", fileContext: "types", fileName: "monitor" },
    });
  });
});

/*
 * The pane and the scrollback's cell are two views of one node.
 *
 * Not two results that happen to agree: the screen is handed the very cell the scrollback draws,
 * read from one session model, so a repeat started in either place is the same act on the same
 * node and both views move together because there is only one of them to move.
 */
describe("the bound node, seen twice", () => {
  it("should_HandTheEditorTheCellTheScrollbackDraws_When_ANodeIsBound", () => {
    const model = readSession({ workspace: sessionWorkspace, cells: sessionCells, context: sessionContext }, sessionNow);
    const node = sessionWorkspace.nodes[0]!.id;
    const owning = sessionCells.find((cell) => cell.nodes.includes(node))!;
    const inTheScrollback = model.cells.find((cell) => cell.id === owning.id);
    // The editor's pane looks the node's cell up the same way, in the same model.
    const forTheEditor = model.cells.find((cell) => sessionCells.find((it) => it.id === cell.id)?.nodes.includes(node));
    expect(forTheEditor).toBe(inTheScrollback);
    expect(forTheEditor?.verdict).toBe(inTheScrollback?.verdict);
  });
});

it("treats views as an ordinary result name, not a separate file context", () => {
  expect(read("/edit views")).toEqual({ kind: "screen", screen: "edit", node: "views" });
});
