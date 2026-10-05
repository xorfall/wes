import { describe, expect, it } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { readFileSync } from "node:fs";
import { MonoLine, caretsUnder, columnOf, lineText, sourceBreaks, type Segment } from "./MonoLine";

/** The command line of the surface's "Mono lines" screen, run for run. */
const command: Segment[] = [
  { text: "09:14  ", role: "mono-faint" },
  { text: "❯ ", role: "mono-ref-strong" },
  { text: "acme orders.list", role: "mono-provider" },
  { text: " " },
  { text: "since:", role: "mono-param" },
  { text: "2026-09-01", role: "mono-literal" },
  { text: " " },
  { text: "statis:", role: "mono-param" },
  { text: '"open"', role: "mono-literal" },
];
const MISTAKE = 7;

/** Its diagnostic row: a run of spaces, then carets under the parameter that does not exist. */
const diagnostic: Segment[] = [
  { text: " ".repeat(43) },
  { text: "^^^^^^^", role: "mono-bad" },
  { text: " no such parameter; the package offers status:", role: "mono-bad" },
];

function render(segments: readonly Segment[]): ReactTestRenderer {
  let tree: ReactTestRenderer | undefined;
  act(() => {
    tree = create(<MonoLine segments={segments} />);
  });
  return tree!;
}

function spans(tree: ReactTestRenderer): { className: string; text: string }[] {
  return tree.root
    .findAllByType("span")
    .map((span) => ({ className: String(span.props.className), text: String(span.children[0] ?? "") }));
}

describe("a mono line", () => {
  it("wraps source punctuation without inserting characters or breaking quoted punctuation", () => {
    const text = ':calc { return {message:"a,;[\\\"b",items:[1,2]}; }';
    const segments: Segment[] = [{text:text.slice(0,27),role:"mono-meta"},{text:text.slice(27),role:"mono-literal"}];
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<MonoLine segments={segments} sourceWrap/>); });
    const plain = (node: unknown): string => typeof node === "string" ? node : typeof node === "object" && node && "children" in node ? ((node as {children:unknown[]}).children ?? []).map(plain).join("") : "";
    expect(plain(tree.toJSON())).toBe(text); expect(lineText(segments)).toBe(text);
    expect(tree.root.findAllByType("wbr")).toHaveLength(sourceBreaks(text).size);
    for (const offset of sourceBreaks(text)) expect(offset < text.indexOf('"a') || offset > text.indexOf('",items')).toBe(true);
    act(() => tree.unmount());
  });
  it("should_ColourEachRunByItsRole_When_TheLineIsRendered", () => {
    const tree = render(command);
    expect(spans(tree).map((span) => span.className)).toEqual([
      "mono-faint", "mono-ref-strong", "mono-provider", "mono-ink",
      "mono-param", "mono-literal", "mono-ink", "mono-param", "mono-literal",
    ]);
    act(() => tree.unmount());
  });

  it("should_TakeOrdinaryText_When_ARunHasNoRole", () => {
    const tree = render([{ text: "plain" }]);
    expect(spans(tree)).toEqual([{ className: "mono-ink", text: "plain" }]);
    act(() => tree.unmount());
  });

  it("should_KeepThreeSpacesThree_When_ARunCarriesWhitespace", () => {
    const tree = render([{ text: "a", role: "mono-ink" }, { text: "   " }, { text: "b", role: "mono-ink" }]);
    expect(spans(tree)[1]!.text).toBe("   ");
    expect(lineText(command)).toBe('09:14  ❯ acme orders.list since:2026-09-01 statis:"open"');
    act(() => tree.unmount());
  });

  it("should_RenderOnePreElement_When_TheLineIsRendered", () => {
    const tree = render(command);
    const pre = tree.root.findByType("pre");
    expect(pre.props.className).toBe("mono-line");
    expect(tree.root.findAllByType("pre")).toHaveLength(1);
    act(() => tree.unmount());
  });

  it("should_PlaceItsOwnNameBesideTheLines_When_AClassNameIsGiven", () => {
    let tree: ReactTestRenderer | undefined;
    act(() => { tree = create(<MonoLine segments={command} className="command" />); });
    expect(tree!.root.findByType("pre").props.className).toBe("mono-line command");
    act(() => tree!.unmount());
  });
});

describe("mono line name hints", () => {
  it("keeps overflowing ordinary text free of tooltips", () => {
    const tree=render(command);
    expect(tree.root.findByType("pre").props["data-hint"]).toBeUndefined();
    expect(tree.root.findAll(node=>node.props.title!==undefined||node.props["data-variable-name"]!==undefined)).toHaveLength(0);
    act(()=>tree.unmount());
  });
  it("carries the full variable name only on its text span", () => {
    const tree=render([{text:"✓ "},{text:"$long…name",role:"mono-ref",variableName:"$long_variable_name"}]);
    expect(tree.root.findByType("pre").props["data-variable-name"]).toBeUndefined();
    expect(tree.root.findAllByType("span").map(span=>span.props["data-variable-name"])).toEqual([undefined,"$long_variable_name"]);
    act(()=>tree.unmount());
  });
});

describe("a diagnostic caret row", () => {
  it("should_StartInTheSameColumnAsTheToken_When_TheTwoLinesShareAFace", () => {
    expect(columnOf(command, MISTAKE)).toBe(43);
    expect(columnOf(diagnostic, 1)).toBe(43);
    expect(columnOf(command, MISTAKE)).toBe(columnOf(diagnostic, 1));
  });

  it("should_CoverExactlyTheToken_When_TheCaretsAreCounted", () => {
    expect(diagnostic[1]!.text).toHaveLength(command[MISTAKE]!.text.length);
  });

  it("should_BuildTheCaretRow_When_CaretsAreAskedForUnderARun", () => {
    const built = caretsUnder(command, MISTAKE, " no such parameter; the package offers status:");
    expect(lineText(built)).toBe(lineText(diagnostic));
    expect(built.map((segment) => segment.role)).toEqual([undefined, "mono-bad", "mono-bad"]);
  });

  it("should_SayNothing_When_ThereIsNoSuchRun", () => {
    expect(caretsUnder(command, 99)).toEqual([]);
  });
});

describe("the mono line's stylesheet", () => {
  const css = readFileSync(new URL("./mono-line.css", import.meta.url), "utf8");

  it("should_RefuseToWrapAndCutWithAnEllipsis_When_TheLineIsTooLong", () => {
    expect(css).toMatch(/\.mono-line\s*\{[^}]*white-space: pre;/);
    expect(css).toMatch(/\.mono-line\s*\{[^}]*overflow: hidden;/);
    expect(css).toMatch(/\.mono-line\s*\{[^}]*text-overflow: ellipsis;/);
    expect(css).not.toMatch(/white-space: pre-wrap/);
  });
});
