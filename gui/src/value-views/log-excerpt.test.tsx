import { renderToStaticMarkup } from "react-dom/server";
import { readFileSync } from "node:fs";
import { act, create } from "react-test-renderer";
import { describe, expect, it } from "vitest";
import { ExactNumber } from "@wes/view-sdk";
import { valueViewModules } from "./registry";
import { decodeContract } from "./definition";
import excerpt, { prepare } from "../../../views/log-excerpt/View";
import { definition, type Input } from "../../../views/log-excerpt/contract";

const render = (input: Input, mode: "preview" | "expanded" | "window" = "expanded") =>
  renderToStaticMarkup(<excerpt.Component input={input} state={undefined as never} revision={0} emit={() => {}} slots={{}} context={{ mode, instance: null }} />);
const row = (ordinal: number, text: string, step: string | null = "Assemble bundle", level: string | null = null): Input["lines"][number] =>
  ({ ordinal, time: "2031-04-02T08:15:30.000Z", step, level, text });

/*
 * Independently synthetic excerpt: an invented packaging stage whose packer tool is missing a
 * required option, then a later gate step. Run, job, origin and every line are made up.
 */
const slice: Input = {
  view: "log-excerpt",
  title: "evidence for packer: error",
  artifact: { run: "synthetic-run-a", attempt: 1, job: "packaging", origin: "stage-log", digest: "c".repeat(64) },
  focus: { from: 9, to: 10 },
  lines: [
    row(7, "launching packer for bundle alpha"),
    row(9, "packer usage: packer --layout FILE [--quiet]"),
    row(10, "packer: error: option --layout is required", "Assemble bundle", "error"),
    row(11, "launcher: stopping after the packer step"),
    row(19, "stage Assemble bundle ended with status 2", "Release gate", "error"),
  ],
};

describe("LogExcerpt", () => {
  it("is a packaged view matched by its declared, bounded input", () => {
    expect(valueViewModules.named("log-excerpt")?.definition?.name).toBe("LogExcerpt");
    expect(() => decodeContract(definition, definition.input, slice)).not.toThrow();
    expect(() => decodeContract(definition, definition.input, { ...slice, view: "log" })).toThrow();
    expect(() => decodeContract(definition, definition.input, { ...slice, lines: Array.from({ length: 401 }, (_, i) => row(i + 1, "x")) })).toThrow();
  });

  it("marks the focus, counts gaps and step changes, and refuses disorder", () => {
    const model = prepare(slice);
    expect(model.items.map((i) => i.kind === "line" ? [i.row.ordinal, i.focused] : [i.kind, i.kind === "gap" ? i.count : i.text])).toEqual([
      ["scope", "Assemble bundle"], [7, false], ["gap", 1], [9, true], [10, true], [11, false], ["gap", 7], ["scope", "Release gate"], [19, false],
    ]);
    expect(model.missing).toBe(0);
    expect(() => prepare({ ...slice, lines: [row(10, "b"), row(9, "a")] })).toThrow("line 9 is out of order or repeated");
    expect(() => prepare({ ...slice, focus: { from: 10, to: 9 } })).toThrow("focus ends before it starts");
  });

  it("reads exact ordinals the same as plain ones", () => {
    const exact = { ...slice, focus: { from: new ExactNumber("9"), to: new ExactNumber("10") }, lines: slice.lines.map((r) => ({ ...r, ordinal: new ExactNumber(String(r.ordinal)) })) } as unknown as Input;
    expect(prepare(exact).items.filter((i) => i.kind === "line" && i.focused)).toHaveLength(2);
    expect(render(exact)).toContain("focus lines 9–10");
  });

  it("says when focus lines are not in the excerpt instead of inventing them", () => {
    const html = render({ ...slice, focus: { from: 8, to: 10 } });
    expect(html).toContain("1 focus line is not in this excerpt");
    expect(render(slice)).not.toContain("not in this excerpt");
    expect(render(slice)).toContain("stage-log · packaging · run synthetic-run-a attempt 1 · focus lines 9–10");
    expect(render(slice)).toContain('aria-current="true"');
    expect(render({ ...slice, lines: [] })).toContain("No lines.");
  });

  it("keeps gap and step rows as readable list items", () => {
    const html = render(slice);
    expect(html).not.toContain('role="separator"');
    expect(html.match(/<li class="log-excerpt-gap screen-label">…/g)).toHaveLength(2);
    expect(html).toContain('<li class="log-excerpt-scope screen-label">Release gate</li>');
  });

  it("never scrolls the hosting page to its focus", () => {
    expect(readFileSync(new URL("../../../views/log-excerpt/View.tsx", import.meta.url), "utf8")).not.toContain("scrollIntoView");
  });

  it("bounds its line list in every tier so the list, not the frame, scrolls", () => {
    expect(render(slice, "preview")).toContain('<ol class="log-excerpt-lines log-excerpt-preview">');
    expect(render(slice, "expanded")).toContain('<ol class="log-excerpt-lines">');
    expect(render(slice, "window")).toContain('<ol class="log-excerpt-lines log-excerpt-window">');
    const css = readFileSync(new URL("../../../views/log-excerpt/view.css", import.meta.url), "utf8");
    const bound = (selector: string) => Number(new RegExp(`^\\${selector} \\{[^}]*max-height:(\\d+)em`, "m").exec(css)?.[1]);
    const [preview, expanded, wide] = [".log-excerpt-preview", ".log-excerpt-lines", ".log-excerpt-window"].map(bound);
    expect(preview).toBeGreaterThan(0);
    expect(expanded).toBeGreaterThan(preview!);
    expect(wide).toBeGreaterThan(expanded!);
    expect(css).not.toContain("max-height:100%");
  });

  it("centres the focus inside its own list, and only when the focus moves", () => {
    const box = { offsetTop: 100, clientHeight: 240, scrollTop: 0 }, line = { offsetTop: 1000, offsetHeight: 18 };
    const view = (input: Input) => <excerpt.Component input={input} state={undefined as never} revision={0} emit={() => {}} slots={{}} context={{ mode: "expanded", instance: null }} />;
    let tree!: ReturnType<typeof create>;
    act(() => { tree = create(view(slice), { createNodeMock: (element) => element.type === "ol" ? box : line }); });
    expect(box.scrollTop).toBe(789);
    box.scrollTop = 5;
    act(() => tree.update(view({ ...slice, title: "same focus, new title" })));
    expect(box.scrollTop).toBe(5);
    line.offsetTop = 400;
    act(() => tree.update(view({ ...slice, focus: { from: 11, to: 11 } })));
    expect(box.scrollTop).toBe(189);
    act(() => tree.unmount());
  });
});
