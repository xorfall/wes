import { terminalPane } from "./split-model";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { useState } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { emptyAliases } from "../aliases";
import type { Catalogue } from "../vocabulary";
import { lineText } from "./MonoLine";
import { Prompt, promptLine } from "./Prompt";
import { promptCompletion, roleOf } from "./prompt-complete";
import { variables } from "./cascade.test-support";
import { Split } from "./Split";
import { oneP, splitPane } from "./split-model";

/** A synthetic vocabulary: one meta command and one provider with one capability. */
const catalogue: Catalogue = {
  commands: [
    { name: "list", implemented: true, summary: "what there is", takes: ["providers"], open: false, parameters: [], variants: [] },
    { name: "calc", implemented: true, summary: "compute", takes: [], open: true, parameters: [], variants: [] },
  ],
  annotations: ["trace"],
  providers: [
    {
      name: "acme", ready: true, credentials: [],
      capabilities: [{ path: ["orders", "list"], summary: "orders", result: "List<Order>", safe: true, parameters: [] }],
    },
  ],
};

const names = ["orders", "id7"];

/** The primary modifier is the host's; these tests name macOS unless a case says otherwise. */
const platform = (name: string) => vi.stubGlobal("navigator", { platform: name });
beforeEach(() => platform("MacIntel"));
afterEach(() => vi.unstubAllGlobals());

/**
 * The prompt with somebody typing at it.
 *
 * Held as state here rather than driven with a fixed string, because what is being tested is the
 * list reacting to what was typed, and a controlled field cannot show that on its own.
 */
function Typing({ onSubmit, onGrow, onDraft, onChrome, start = "", history, historyScope, vocabulary = catalogue }: {
  readonly onSubmit?: (text: string) => void;
  readonly onGrow?: (text: string) => void;
  readonly onDraft?: (text: string) => void;
  readonly onChrome?: (chrome: string) => void;
  readonly start?: string;
  readonly vocabulary?: Catalogue;
  readonly history?: readonly string[];
  readonly historyScope?: string;
}) {
  const [draft, setDraft] = useState(start);
  return (
    <Prompt
      draft={draft}
      onDraft={text => { setDraft(text); onDraft?.(text); }}
      history={history}
      historyScope={historyScope}
      onSubmit={onSubmit ?? (() => undefined)}
      onGrow={onGrow ?? (() => undefined)}
      chromeName="keys"
      onChrome={onChrome ?? (() => undefined)}
      catalogue={vocabulary}
      names={names}
      aliases={emptyAliases}
    />
  );
}

function draw(onSubmit?: (text: string) => void, start?: string): ReactTestRenderer {
  let tree: ReactTestRenderer | undefined;
  act(() => { tree = create(<Typing {...(onSubmit ? { onSubmit } : {})} {...(start ? { start } : {})} />); });
  return tree!;
}

const field = (tree: ReactTestRenderer) => tree.root.findByProps({ className: "prompt-field" });

function press(tree: ReactTestRenderer, key: string, held: {
  ctrlKey?: boolean; shiftKey?: boolean; metaKey?: boolean; altKey?: boolean;
  nativeEvent?: { isComposing: boolean }; keyCode?: number;
  currentTarget?: { selectionStart: number; selectionEnd: number };
} = {}) {
  const prevented = vi.fn();
  act(() => {
    const end = field(tree).props.value.length;
    field(tree).props.onKeyDown({ key, currentTarget: { selectionStart: end, selectionEnd: end }, ...held,
      preventDefault: prevented, stopPropagation: () => undefined });
  });
  return prevented;
}

function type(tree: ReactTestRenderer, value: string) {
  act(() => {
    field(tree).props.onChange({ target: { value, selectionStart: value.length } });
  });
}

/** The rows of the open list, or nothing when it is shut. */
function offered(tree: ReactTestRenderer): string[] {
  const lists = tree.root.findAllByProps({ role: "listbox" });
  if (lists.length === 0) return [];
  return lists[0]!.props.children[0].map((row: { props: { segments: Parameters<typeof lineText>[0] } }) =>
    // The chosen row leads with `› ` and the rest with two spaces; neither is part of the name.
    lineText(row.props.segments).replace(/^›?\s*/, "").trim(),
  );
}

it.each(["first second", "$"])("keeps Option-arrow editing in a split console with draft %j", start => {
  const state = splitPane(oneP({ id: "console", title: "session" }), "right", terminalPane("terminal"));
  const onChange = vi.fn(), onDraft = vi.fn(), onSubmit = vi.fn();
  let tree!: ReactTestRenderer;
  act(() => {
    tree = create(<Split state={state} top={[]} prompt={[]} context={[]} onChange={onChange}
      content={pane => pane.terminal ? <span>synthetic terminal</span> : <Typing start={start} onDraft={onDraft} onSubmit={onSubmit} />} />);
  });
  expect(offered(tree).length > 0).toBe(start === "$");
  const target = { closest: (selector: string) => selector.split(", ").includes("textarea") ? target : null };
  const split = tree.root.findByProps({ className: "split" });
  for (const key of ["ArrowLeft", "ArrowRight"]) for (const shiftKey of [false, true]) {
    const event = { key, altKey: true, shiftKey, ctrlKey: false, metaKey: false,
      target, currentTarget: target, preventDefault: vi.fn(), stopPropagation: vi.fn() };
    act(() => {
      split.props.onKeyDownCapture(event);
      field(tree).props.onKeyDown(event);
      split.props.onKeyDown(event);
    });
    expect(event.preventDefault).not.toHaveBeenCalled();
    expect(event.stopPropagation).not.toHaveBeenCalled();
    expect(field(tree).props.value).toBe(start);
  }
  expect(onChange).not.toHaveBeenCalled();
  expect(onDraft).not.toHaveBeenCalled();
  expect(onSubmit).not.toHaveBeenCalled();
  act(() => tree.unmount());
});

describe("the prompt's command history", () => {
  const history = ["first command", "second command", "second command"];
  const drawHistory = (start = "unfinished draft", entries = history) => {
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Typing start={start} history={entries} historyScope="one" />); });
    return tree;
  };

  it("walks both directions, clamps at the oldest and restores the unsent draft", () => {
    const tree = drawHistory();
    press(tree, "ArrowDown");
    expect(field(tree).props.value).toBe("unfinished draft");
    press(tree, "ArrowUp");
    expect(field(tree).props.value).toBe("second command");
    press(tree, "ArrowUp"); press(tree, "ArrowUp");
    expect(field(tree).props.value).toBe("first command");
    press(tree, "ArrowDown");
    expect(field(tree).props.value).toBe("second command");
    press(tree, "ArrowDown"); press(tree, "ArrowDown");
    expect(field(tree).props.value).toBe("unfinished draft");
    act(() => tree.unmount());
    const empty = drawHistory("unfinished draft", []);
    press(empty, "ArrowUp"); press(empty, "ArrowDown");
    expect(field(empty).props.value).toBe("unfinished draft");
    act(() => empty.unmount());
  });

  it("keeps completion priority, then recalls without reopening suggestions or submitting", () => {
    const onSubmit = vi.fn();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Typing history={["$orders"]} onSubmit={onSubmit} />); });
    type(tree, "$"); press(tree, "ArrowDown"); press(tree, "Enter");
    expect(field(tree).props.value).toBe("$id7");
    press(tree, "ArrowUp");
    expect(field(tree).props.value).toBe("$orders");
    expect(offered(tree)).toEqual([]);
    expect(onSubmit).not.toHaveBeenCalled();
    press(tree, "ArrowDown");
    expect(field(tree).props.value).toBe("$id7");
    type(tree, "$");
    expect(offered(tree)).toEqual(["$orders", "$id7"]);
    act(() => tree.unmount());
  });

  it("starts a fresh walk after editing, submitting or handing the draft to the editor", () => {
    const tree = drawHistory();
    press(tree, "ArrowUp"); type(tree, "edited command"); press(tree, "ArrowDown");
    expect(field(tree).props.value).toBe("edited command");
    press(tree, "ArrowUp"); press(tree, "ArrowDown");
    expect(field(tree).props.value).toBe("edited command");
    press(tree, "ArrowUp"); press(tree, "Enter"); press(tree, "ArrowDown");
    expect(field(tree).props.value).toBe("second command");
    type(tree, "new draft"); press(tree, "ArrowUp");
    press(tree, "Enter", { shiftKey: true, metaKey: true }); press(tree, "ArrowDown");
    expect(field(tree).props.value).toBe("second command");
    act(() => tree.unmount());
  });

  it("freezes replay during a walk and never restores old history across workspace generations", () => {
    const tree = drawHistory();
    press(tree, "ArrowUp");
    act(() => tree.update(<Typing history={[...history, "arriving command"]} historyScope="one" />));
    press(tree, "ArrowUp");
    expect(field(tree).props.value).toBe("first command");
    act(() => tree.update(<Typing history={["new workspace command"]} historyScope="two" />));
    press(tree, "ArrowDown");
    expect(field(tree).props.value).toBe("first command");
    press(tree, "ArrowUp");
    expect(field(tree).props.value).toBe("new workspace command");
    act(() => tree.unmount());
  });

  it("leaves modified arrows, selections, composition and multiline caret movement native", () => {
    const tree = drawHistory("line one\nline two");
    for (const held of [{ altKey: true }, { ctrlKey: true }, { metaKey: true }, { shiftKey: true },
      { nativeEvent: { isComposing: true } }, { keyCode: 229 },
      { currentTarget: { selectionStart: 0, selectionEnd: 3 } }]) {
      expect(press(tree, "ArrowUp", held)).not.toHaveBeenCalled();
    }
    expect(press(tree, "ArrowUp")).not.toHaveBeenCalled();
    expect(press(tree, "ArrowDown", { currentTarget: { selectionStart: 2, selectionEnd: 2 } })).not.toHaveBeenCalled();
    expect(field(tree).props.value).toBe("line one\nline two");
    press(tree, "ArrowUp", { currentTarget: { selectionStart: 2, selectionEnd: 2 } });
    expect(field(tree).props.value).toBe("second command");
    press(tree, "ArrowDown");
    expect(field(tree).props.value).toBe("line one\nline two");
    act(() => tree.unmount());
  });

  it("places the real caret at the end of the recalled command in the same commit", () => {
    let value = "";
    const setSelectionRange = vi.fn();
    const box = { get value() { return value; }, setSelectionRange, scrollLeft: 0 };
    let tree!: ReactTestRenderer;
    act(() => {
      tree = create(<Typing history={history} onDraft={text => { value = text; }} />, {
        createNodeMock: node => node.type === "textarea" ? box : null,
      });
    });
    press(tree, "ArrowUp");
    expect(setSelectionRange).toHaveBeenLastCalledWith("second command".length, "second command".length);
    press(tree, "ArrowDown");
    expect(setSelectionRange).toHaveBeenLastCalledWith(0, 0);
    act(() => tree.unmount());
  });
});

describe("the prompt's completion list", () => {
  it("should_OpenOnAsking_When_ControlSpaceIsPressed", () => {
    const tree = draw();
    expect(offered(tree)).toEqual([]);
    press(tree, " ", { ctrlKey: true });
    expect(offered(tree).length).toBeGreaterThan(0);
    act(() => tree.unmount());
  });

  it("should_Close_When_EscapeIsPressed", () => {
    const tree = draw();
    press(tree, " ", { ctrlKey: true });
    expect(offered(tree).length).toBeGreaterThan(0);
    press(tree, "Escape");
    expect(offered(tree)).toEqual([]);
    act(() => tree.unmount());
  });

  it("should_OfferTheWorkspacesNames_When_ADollarIsTyped", () => {
    const tree = draw();
    type(tree, "$");
    expect(offered(tree)).toEqual(["$orders", "$id7"]);
    act(() => tree.unmount());
  });

  it("should_OfferTheEnginesMetaCommands_When_AColonIsTyped", () => {
    const tree = draw();
    type(tree, ":");
    expect(offered(tree)).toEqual([":list", ":calc"]);
    act(() => tree.unmount());
  });

  it("should_OfferTheProvidersTheEngineAnnounced_When_AWordIsStarted", () => {
    const tree = draw();
    type(tree, "acm");
    expect(offered(tree).some((row) => row.startsWith("acme"))).toBe(true);
    act(() => tree.unmount());
  });

  it("should_TakeTheChosenOneAndNotRun_When_EnterIsPressedWithTheListOpen", () => {
    const onSubmit = vi.fn();
    const tree = draw(onSubmit);
    type(tree, "$");
    press(tree, "Enter");
    expect(onSubmit).not.toHaveBeenCalled();
    expect(field(tree).props.value).toBe("$orders");
    expect(offered(tree)).toEqual([]);
    act(() => tree.unmount());
  });

  it("should_Run_When_TheChosenSuggestionIsAlreadyWhatWasTyped", () => {
    const onSubmit = vi.fn();
    const tree = draw(onSubmit);
    type(tree, "$orders");
    expect(offered(tree)).toEqual(["$orders"]);
    press(tree, "Enter");
    expect(onSubmit).toHaveBeenCalledWith("$orders");
    act(() => tree.unmount());
  });

  it("should_Run_When_EnterIsPressedWithNothingOpen", () => {
    const onSubmit = vi.fn();
    const tree = draw(onSubmit, "acme orders.list");
    press(tree, "Escape");
    press(tree, "Enter");
    expect(onSubmit).toHaveBeenCalledWith("acme orders.list");
    act(() => tree.unmount());
  });

  it("should_MoveThroughTheList_When_TheArrowsArePressed", () => {
    const tree = draw();
    type(tree, "$");
    press(tree, "ArrowDown");
    press(tree, "Enter");
    expect(field(tree).props.value).toBe("$id7");
    act(() => tree.unmount());
  });

  it("should_TakeTheChosenOne_When_TabIsPressed", () => {
    const tree = draw();
    type(tree, "$");
    press(tree, "Tab");
    expect(field(tree).props.value).toBe("$orders");
    act(() => tree.unmount());
  });
});

describe("how a suggestion is drawn", () => {
  it("should_ColourEachKindTheWayTheCommandLineWould_When_ItIsOffered", () => {
    expect(roleOf("meta")).toBe("mono-meta");
    expect(roleOf("provider")).toBe("mono-provider");
    expect(roleOf("parameter")).toBe("mono-param");
    expect(roleOf("reference")).toBe("mono-ref");
    expect(roleOf("program")).toBe("mono-ink");
  });
});

it("keeps the chevron untruncated and separated from the command at any font width", () => {
  const tree = draw();
  for (const draft of ["", "first\nsecond"]) {
    type(tree, draft);
    const prefix = tree.root.findByProps({ className: "mono-line prompt-prefix" });
    expect(prefix.findByType("span").children.join("")).toBe("❯");
    expect(prefix.props["aria-hidden"]).toBe("true");
    expect(field(tree).props.value).toBe(draft);
  }
  act(() => tree.unmount());

  // The runner has no font layout: assert the CSS contract that lets a fallback
  // chevron keep its own width, with spacing outside the glyph instead of in it.
  const css = readFileSync(new URL("./session.css", import.meta.url), "utf8");
  const prefix = css.match(/\.prompt-stack \.prompt-prefix\s*\{([^}]*)\}/)![1]!;
  expect(prefix).toMatch(/flex: 0 0 auto;/);
  expect(prefix).toMatch(/width: max-content;/);
  expect(prefix).toMatch(/overflow: visible;/);
  expect(prefix).toMatch(/text-overflow: clip;/);
  expect(css).toMatch(/\.prompt-stack\s*\{[^}]*gap: var\(--space-xs\);/);
  const inset = css.match(/\.session-in-pane \.session-prompt\s*\{([^}]*)\}/)![1]!;
  expect(inset).toMatch(/padding-left: var\(--space-sm\);/);
  expect(inset).toMatch(/padding-right: var\(--space-sm\);/);
  for (const density of ["normal", "dense"] as const) {
    const tokens = variables({ palette: "paper", density });
    expect(parseFloat(tokens.get("--space-xs")!)).toBeGreaterThan(0);
    expect(parseFloat(tokens.get("--space-sm")!)).toBeGreaterThan(0);
  }
});

describe("plain multiline prompt editing", () => {
  it("inserts a newline instead of accepting an open completion or opening the editor", () => {
    const onGrow = vi.fn(), onSubmit = vi.fn();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Typing start="$" onGrow={onGrow} onSubmit={onSubmit} />); });
    expect(offered(tree).length).toBeGreaterThan(0);
    press(tree, "Enter", { shiftKey: true });
    expect(field(tree).props.value).toBe("$\n");
    expect(field(tree).props.rows).toBe(2);
    expect(offered(tree)).toEqual([]);
    expect(onGrow).not.toHaveBeenCalled(); expect(onSubmit).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("replaces a selection and places the caret synchronously without indenting or pairing", () => {
    let value = "\tfirst selected end";
    const setSelectionRange = vi.fn();
    const box = { get value() { return value; }, setSelectionRange, scrollLeft: 12, scrollTop: 24 };
    const drawn = { scrollLeft: 0, scrollTop: 0 };
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Typing start={value} onDraft={text => { value = text; }} />, {
      createNodeMock: node => node.type === "textarea" ? box : node.props.className === "mono-line prompt-drawn" ? drawn : null,
    }); });
    press(tree, "Enter", { shiftKey: true, currentTarget: { selectionStart: 7, selectionEnd: 15 } });
    expect(field(tree).props.value).toBe("\tfirst \n end");
    expect(setSelectionRange).toHaveBeenLastCalledWith(8, 8);
    expect(drawn).toEqual({ scrollLeft: 12, scrollTop: 24 });
    box.scrollLeft = 30; box.scrollTop = 80;
    act(() => field(tree).props.onScroll());
    expect(drawn).toEqual({ scrollLeft: 30, scrollTop: 80 });
    type(tree, "\tfirst \n({");
    expect(field(tree).props.value).toBe("\tfirst \n({");
    expect(setSelectionRange).toHaveBeenLastCalledWith(value.length, value.length);
    act(() => tree.unmount());
  });

  it("preserves pasted whitespace and grows through trailing blank rows up to ten", () => {
    const tree = draw();
    const source = "\n\t:calc {\n  return 1;\n}\n\n";
    type(tree, source);
    expect(field(tree).props.value).toBe(source);
    expect(field(tree).props.rows).toBe(6);
    expect(field(tree).props.wrap).toBe("off");
    expect(field(tree).props.style?.textIndent).toBeUndefined();
    type(tree, source + "\n".repeat(20));
    expect(field(tree).props.rows).toBe(10);
    type(tree, "x"); expect(field(tree).props.rows).toBe(1);
    act(() => tree.unmount());
  });

  it("opens the exact draft only with Cmd+Shift+Enter, and keeps IME native", () => {
    const onGrow = vi.fn(), onSubmit = vi.fn();
    const source = "\n\t:calc {\n\n  return 1;\n}\n";
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Typing start={source} onGrow={onGrow} onSubmit={onSubmit} />); });
    for (const held of [{ nativeEvent: { isComposing: true } }, { keyCode: 229 }]) {
      expect(press(tree, "Enter", { shiftKey: true, ...held })).not.toHaveBeenCalled();
      expect(press(tree, "Enter", { shiftKey: true, metaKey: true, ...held })).not.toHaveBeenCalled();
    }
    expect(field(tree).props.value).toBe(source);
    press(tree, "Enter", { shiftKey: true, metaKey: true });
    expect(onGrow).toHaveBeenCalledExactlyOnceWith(source);
    expect(onSubmit).not.toHaveBeenCalled();
    press(tree, "Enter", { metaKey: true });
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith(source);
    act(() => tree.unmount());
  });
});

describe("the prompt's chords", () => {
  type Held = Parameters<typeof press>[2];
  /** A key as the field hears it, reporting whether the prompt took it from the field and the pane. */
  function chord(tree: ReactTestRenderer, key: string, held: Held = {}) {
    const prevented = vi.fn(), stopped = vi.fn();
    act(() => {
      const end = field(tree).props.value.length;
      field(tree).props.onKeyDown({ key, ctrlKey: false, shiftKey: false, metaKey: false, altKey: false,
        currentTarget: { selectionStart: end, selectionEnd: end }, ...held, preventDefault: prevented, stopPropagation: stopped });
    });
    return { taken: prevented.mock.calls.length > 0, stopped: stopped.mock.calls.length > 0 };
  }
  const native = { taken: false, stopped: false };
  const split = (tree: ReactTestRenderer) => tree.root.findByProps({ className: "split" });
  function drawWith(start: string) {
    const onSubmit = vi.fn(), onGrow = vi.fn(), onChrome = vi.fn();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<Typing start={start} onSubmit={onSubmit} onGrow={onGrow} onChrome={onChrome} />); });
    return { tree, onSubmit, onGrow, onChrome };
  }

  it("should_LeaveShiftTabAndModifiedTabToFocusAndPanes_When_TheListIsOpen", () => {
    const { tree } = drawWith("$");
    expect(offered(tree).length).toBeGreaterThan(0);
    for (const held of [{ shiftKey: true }, { ctrlKey: true }, { ctrlKey: true, shiftKey: true }, { altKey: true }, { metaKey: true }]) {
      expect(chord(tree, "Tab", held)).toEqual(native);
    }
    expect(field(tree).props.value).toBe("$");
    expect(chord(tree, "Tab")).toEqual({ taken: true, stopped: true });
    expect(field(tree).props.value).toBe("$orders");
    act(() => tree.unmount());
  });

  it("should_CyclePanes_When_CtrlTabIsPressedInTheSplitPrompt", () => {
    const state = splitPane(oneP({ id: "console", title: "session" }), "right", terminalPane("terminal"));
    const onChange = vi.fn();
    let tree!: ReactTestRenderer;
    act(() => {
      tree = create(<Split state={state} top={[]} prompt={[]} context={[]} onChange={onChange}
        content={pane => pane.terminal ? <span>synthetic terminal</span> : <Typing start="$" />} />);
    });
    const target = { closest: () => null };
    const event = { key: "Tab", ctrlKey: true, shiftKey: false, metaKey: false, altKey: false, target, defaultPrevented: false,
      currentTarget: { selectionStart: 1, selectionEnd: 1 }, preventDefault: vi.fn(), stopPropagation: vi.fn() };
    act(() => {
      split(tree).props.onKeyDownCapture(event);
      field(tree).props.onKeyDown(event);
      split(tree).props.onKeyDown(event);
    });
    expect(onChange).toHaveBeenCalledOnce();
    expect(field(tree).props.value).toBe("$");
    act(() => tree.unmount());
  });

  it("should_KeepCtrlCForCopying_When_TextIsSelected", () => {
    const { tree, onChrome } = drawWith("acme orders.list");
    for (const name of ["Win32", "Linux x86_64", "MacIntel"]) {
      platform(name);
      expect(chord(tree, "c", { ctrlKey: true, currentTarget: { selectionStart: 0, selectionEnd: 4 } })).toEqual(native);
      expect(chord(tree, "c", { metaKey: true, currentTarget: { selectionStart: 0, selectionEnd: 4 } })).toEqual(native);
    }
    expect(onChrome).not.toHaveBeenCalled();
    for (const held of [{ metaKey: true, shiftKey: true }, { ctrlKey: true, shiftKey: true, altKey: true }, { ctrlKey: true, shiftKey: true, metaKey: true }]) {
      expect(chord(tree, "C", held)).toEqual(native);
    }
    expect(onChrome).not.toHaveBeenCalled();
    expect(chord(tree, "C", { ctrlKey: true, shiftKey: true })).toEqual({ taken: true, stopped: true });
    expect(onChrome).toHaveBeenCalledExactlyOnceWith("controls");
    expect(field(tree).props.value).toBe("acme orders.list");
    act(() => tree.unmount());
  });

  it.each(["Win32", "Linux x86_64"])("should_RunAndOpenTheEditorWithCtrl_When_ThePlatformIs %s", name => {
    platform(name);
    const { tree, onSubmit, onGrow } = drawWith("$");
    expect(offered(tree).length).toBeGreaterThan(0);
    // AltGr arrives as Ctrl+Alt; Meta is the Windows key. Neither runs or opens anything.
    for (const held of [{ metaKey: true }, { metaKey: true, shiftKey: true }, { ctrlKey: true, altKey: true }, { ctrlKey: true, metaKey: true }]) {
      expect(chord(tree, "Enter", held)).toEqual(native);
    }
    for (const held of [{ nativeEvent: { isComposing: true } }, { keyCode: 229 }]) {
      expect(chord(tree, "Enter", { ctrlKey: true, ...held })).toEqual(native);
      expect(chord(tree, "Enter", { ctrlKey: true, shiftKey: true, ...held })).toEqual(native);
    }
    expect(onSubmit).not.toHaveBeenCalled(); expect(onGrow).not.toHaveBeenCalled();
    expect(chord(tree, "Enter", { ctrlKey: true, shiftKey: true })).toEqual({ taken: true, stopped: true });
    expect(onGrow).toHaveBeenCalledExactlyOnceWith("$");
    // The open list is not accepted: Ctrl+Enter runs what is written.
    chord(tree, "Enter", { ctrlKey: true });
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith("$");
    expect(field(tree).props.value).toBe("$");
    act(() => tree.unmount());
  });

  it("should_KeepCmdChordsAndLeaveCtrlEnterNative_When_ThePlatformIsMacOS", () => {
    const { tree, onSubmit, onGrow } = drawWith("$");
    for (const held of [{ ctrlKey: true }, { ctrlKey: true, shiftKey: true }, { metaKey: true, ctrlKey: true }, { metaKey: true, altKey: true }, { altKey: true }]) {
      expect(chord(tree, "Enter", held)).toEqual(native);
    }
    expect(onSubmit).not.toHaveBeenCalled(); expect(onGrow).not.toHaveBeenCalled();
    expect(field(tree).props.value).toBe("$");
    chord(tree, "Enter", { metaKey: true, shiftKey: true });
    expect(onGrow).toHaveBeenCalledExactlyOnceWith("$");
    chord(tree, "Enter", { metaKey: true });
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith("$");
    act(() => tree.unmount());
  });

  it("should_LeaveMacOSControlEditingKeysNative_When_TypingInThePrompt", () => {
    const { tree, onChrome } = drawWith("first line\nsecond");
    for (const key of ["a", "e", "b", "f", "n", "p", "k", "d", "h", "o", "t", "ArrowLeft", "ArrowRight"]) {
      expect(chord(tree, key, { ctrlKey: true })).toEqual(native);
      expect(chord(tree, key, { ctrlKey: true, shiftKey: true })).toEqual(native);
    }
    expect(field(tree).props.value).toBe("first line\nsecond");
    expect(onChrome).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("should_AskForTheListOnlyWithCtrlSpaceAlone_When_OtherModifiersAreHeld", () => {
    const { tree } = drawWith("");
    for (const held of [{ ctrlKey: true, shiftKey: true }, { ctrlKey: true, metaKey: true }, { ctrlKey: true, altKey: true }, { metaKey: true }]) {
      expect(chord(tree, " ", held)).toEqual(native);
    }
    expect(offered(tree)).toEqual([]);
    expect(chord(tree, " ", { ctrlKey: true, nativeEvent: { isComposing: true } })).toEqual(native);
    expect(chord(tree, " ", { ctrlKey: true })).toEqual({ taken: true, stopped: true });
    expect(offered(tree).length).toBeGreaterThan(0);
    act(() => tree.unmount());
  });
});


it("keeps ordinary multiline highlighted source identical to the editable prompt", () => {
  const source = '\n\tacme orders.list\n  status:"two  spaces"\n\n';
  expect(lineText(promptLine(source))).toBe(source);
  const tree = draw(undefined, source);
  const overlay = tree.root.findByProps({ className: "mono-line prompt-drawn" });
  const spans = overlay.findAllByType("span");
  expect(spans.slice(0, -1).map(span => span.children.join("")).join("")).toBe(source);
  expect(spans.at(-1)!.children.join("")).toBe("\u200b");
  expect(overlay.props["aria-hidden"]).toBe("true");
  expect(field(tree).props.value).toBe(source);
  act(() => tree.unmount());
});

it("matches the native viewport excluding persistent scrollbars and follows resize safely", () => {
  let resized!: () => void;
  const observe = vi.fn(), disconnect = vi.fn();
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: () => void) { resized = callback; }
    observe = observe;
    disconnect = disconnect;
  });
  const box = { value: "x", clientWidth: 285, clientHeight: 185, offsetWidth: 300, offsetHeight: 200,
    scrollLeft: 315, scrollTop: 215 };
  let x = 0, y = 0;
  const ink = { style: { width: "300px", height: "200px" },
    get scrollLeft() { return x; }, set scrollLeft(value: number) { x = Math.min(value, 600 - parseInt(this.style.width)); },
    get scrollTop() { return y; }, set scrollTop(value: number) { y = Math.min(value, 400 - parseInt(this.style.height)); },
  };
  let tree!: ReactTestRenderer;
  try {
    act(() => { tree = create(<Typing start="x" />, {
      createNodeMock: node => node.type === "textarea" ? box : node.props.className === "mono-line prompt-drawn" ? ink : null,
    }); });
    expect(observe).toHaveBeenCalledExactlyOnceWith(box);
    expect(ink.style).toEqual({ width: "285px", height: "185px" });
    expect([ink.scrollLeft, ink.scrollTop]).toEqual([315, 215]);
    box.clientWidth = 185; box.clientHeight = 85; box.scrollLeft = 415; box.scrollTop = 315;
    act(() => resized());
    expect(ink.style).toEqual({ width: "185px", height: "85px" });
    expect([ink.scrollLeft, ink.scrollTop]).toEqual([415, 315]);
    box.clientWidth = 0; box.clientHeight = 0; box.scrollLeft = 0; box.scrollTop = 0;
    act(() => resized());
    expect(ink.style).toEqual({ width: "185px", height: "85px" });
    expect([ink.scrollLeft, ink.scrollTop]).toEqual([415, 315]);
    act(() => tree.unmount());
    expect(disconnect).toHaveBeenCalledOnce();
    box.clientWidth = 100; box.clientHeight = 100;
    act(() => resized());
    expect(ink.style).toEqual({ width: "185px", height: "85px" });
  } finally { vi.unstubAllGlobals(); }
});

it("offers saved workspaces after tab and split commands with correctly quoted destinations", async () => {
  const { promptCompletion, asks } = await import("./prompt-complete");
  const { read } = await import("./commands");
  for (const line of ["/tab ", "/tabx ", "/rsplit ", "/split ", "/split down "]) {
    expect(asks(line, line.length)).toBe(true);
    const found = promptCompletion({ line, caret: line.length, catalogue, names: [], aliases: emptyAliases, workspaces: ["orders qa", 'say "hi"', "prod"] });
    const item = found.items.find(item => item.label === "orders qa")!;
    expect(item).toBeDefined();
    expect(read(line.slice(0, found.from) + item.text)).toMatchObject(line.startsWith("/tab") ? { workspace: "orders qa" } : { content: { workspace: "orders qa" } });
  }
  expect(read('/tab "say \\"hi\\""')).toMatchObject({ workspace: 'say "hi"' });
});

it("offers xterm as a terminal once, even when a saved workspace has that name", async () => {
  const { promptCompletion } = await import("./prompt-complete");
  const { read } = await import("./commands");
  const commands = ["/tab", "/tabx", "/split", "/split right", "/split down", ...["l", "r", "t", "b"].flatMap(direction => ["", "x"].map(suffix => `/${direction}split${suffix}`))];
  for (const command of commands) {
    for (const prefix of ["", "xt", '"xt']) {
      const line = `${command} ${prefix}`;
      const found = promptCompletion({ line, caret: line.length, catalogue, names: [], aliases: emptyAliases, workspaces: ["xterm", "xterm-team"] });
      const terminals = found.items.filter(item => (item.label ?? item.text) === "xterm");
      expect(terminals).toHaveLength(1);
      expect(terminals[0]!.detail).toBe("terminal");
      expect(read(line.slice(0, found.from) + terminals[0]!.text)).toMatchObject(command.startsWith("/tab") ? { kind: "terminal-tab" } : { content: { terminal: true } });
    }
  }
});


describe("native selection across workspace draft publication", () => {
  function harness(initial: string) {
    let value = initial;
    const box = {
      get value() { return value; },
      set value(next: string) { value = next; box.selectionStart = box.selectionEnd = next.length; box.selectionDirection = "none"; },
      selectionStart: initial.length, selectionEnd: initial.length, selectionDirection: "none",
      setSelectionRange: vi.fn((start: number, end: number, direction = "none") => {
        box.selectionStart = start; box.selectionEnd = end; box.selectionDirection = direction;
      }),
      scrollLeft: 0, scrollTop: 0,
    };
    const onDraft = vi.fn();
    const render = (draft: string) => <Prompt draft={draft} onDraft={onDraft} catalogue={catalogue} names={names}
      aliases={emptyAliases} onSubmit={() => {}} onGrow={() => {}} chromeName="keys" onChrome={() => {}} />;
    let tree!: ReactTestRenderer;
    act(() => { tree = create(render(initial), { createNodeMock: node => node.type === "textarea" ? box : null }); });
    return {
      tree, box, onDraft,
      edit(text: string, start: number, end = start, direction = "none", isComposing = false) {
        // The input event snapshots native selection before a controller publishes new props.
        act(() => field(tree).props.onChange({ target: { value: text, selectionStart: start, selectionEnd: end, selectionDirection: direction }, nativeEvent: { isComposing } }));
      },
      publish(text: string) {
        // React/browser controlled-value assignment collapses selection at the end.
        box.value = text;
        act(() => tree.update(render(text)));
      },
      close() { act(() => tree.unmount()); },
    };
  }

  it.each([
    [":inspect value:$qa_result", ":inspect vaue:$qa_result", 11],
    [":inspect value:$qa_result", ":inspect vale:$qa_result", 12],
    [":inspect value:$qa_result", ":inspect valXue:$qa_result", 13],
    [":inspect value:$qa_result", ":inspect value:$qa_rsult", 20],
    ["http request\n  url:özgür🦀", "http request\n  ur:özgür🦀", 17],
  ])("keeps an edit in the middle of %s at its native position", (source, edited, position) => {
    const h = harness(source);
    h.edit(edited, position);
    expect(h.onDraft).toHaveBeenCalledExactlyOnceWith(edited);
    expect(h.box.setSelectionRange).not.toHaveBeenCalled(); // old draft is still on screen
    h.publish(edited);
    expect([h.box.selectionStart, h.box.selectionEnd]).toEqual([position, position]);
    expect(h.box.value).toBe(edited);
    h.close();
  });

  it("preserves selection direction and lets newer input supersede pending placement", () => {
    const h = harness(":inspect value:$qa_result");
    h.edit(":inspect vaue:$qa_result", 11);
    h.edit(":inspect vaue:$qa_result!", 9, 13, "backward");
    h.publish(":inspect vaue:$qa_result!");
    expect(h.box.setSelectionRange).toHaveBeenCalledExactlyOnceWith(9, 13, "backward");
    h.close();
  });

  it("discards a pending selection when another owner replaces the draft", () => {
    const h = harness("abc");
    h.edit("ac", 1);
    h.publish("new draft");
    h.publish("ac");
    expect(h.box.setSelectionRange).not.toHaveBeenCalled();
    h.close();
  });

  it("does not take over native IME composition", () => {
    const h = harness("abc");
    h.edit("açbc", 2, 2, "none", true);
    h.publish("açbc");
    expect(h.box.setSelectionRange).not.toHaveBeenCalled();
    h.close();
  });
});


it("keeps every matching command reachable through a bounded suggestion window", () => {
  const vocabulary = { ...catalogue, commands: Array.from({ length: 55 }, (_, i) => ({
    ...catalogue.commands[0]!, name: `action${String(i).padStart(2, "0")}`,
  })) };
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<Typing start=":" vocabulary={vocabulary} />); });
  expect(offered(tree)).toHaveLength(8);
  expect(JSON.stringify(tree.toJSON())).toContain("1 / 55 suggestions");
  for (let i = 0; i < 48; i++) press(tree, "ArrowDown");
  expect(offered(tree)).toHaveLength(8);
  expect(offered(tree)).toContain(":action48");
  expect(JSON.stringify(tree.toJSON())).toContain("49 / 55 suggestions");
  press(tree, "Tab");
  expect(field(tree).props.value).toBe(":action48 ");
  type(tree, ":");
  press(tree, "ArrowUp");
  expect(offered(tree)).toContain(":action54");
  press(tree, "ArrowDown");
  expect(offered(tree)[0]).toBe(":action00");
  type(tree, ":action5");
  expect(offered(tree)).toHaveLength(5);
  press(tree, "Tab");
  expect(field(tree).props.value).toBe(":action50 ");
  act(() => tree.unmount());
});

it("preserves all client, workspace and reference matches before presentation", () => {
  const entries = Array.from({ length: 55 }, (_, i) => `item${i}`);
  const input = { catalogue, names: entries, aliases: emptyAliases, workspaces: entries };
  for (const line of ["$", "/tab $", "/goto "]) {
    expect(promptCompletion({ ...input, line, caret: line.length }).items).toHaveLength(55);
  }
  expect(promptCompletion({ ...input, line: "/", caret: 1 }).items.map(item => item.text)).toContain("/bsplitx");
});
