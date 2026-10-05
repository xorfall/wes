import { describe, expect, it, vi } from "vitest";
import { inputOf, focusPaneInput } from "./pane-focus";

/* A pane as a bare query surface: which selectors find something is the whole fixture. */
const pane = (found: readonly string[]) => {
  const elements = new Map(found.map((selector) => [selector, { focus: vi.fn(), selector }]));
  return {
    querySelector: (query: string) => [...elements.entries()].find(([selector]) => query.includes(selector))?.[1] ?? null,
    contains: () => false,
    focus: vi.fn(),
  } as unknown as HTMLElement;
};

describe("where a pane's keyboard input goes", () => {
  it("should_PreferThePrompt_When_ThePaneHasOne", () => {
    // Arrange
    const it_ = pane([".cm-content", ".prompt-field", ".screen"]);
    // Act / Assert
    expect((inputOf(it_) as unknown as { selector: string }).selector).toBe(".prompt-field");
  });

  it("should_FallBackToTheEditorThenTheScreen_When_ThereIsNoPrompt", () => {
    // Arrange / Act / Assert
    expect((inputOf(pane([".screen", ".cm-content"])) as unknown as { selector: string }).selector).toBe(".cm-content");
    expect((inputOf(pane([".screen"])) as unknown as { selector: string }).selector).toBe(".screen");
    expect(inputOf(pane([]))).toBeDefined();
  });

  it("should_FocusThePanesInput_When_FocusIsNotInsideThePane", () => {
    // Arrange
    const view = pane([".prompt-field"]);
    const root = { querySelector: () => view } as unknown as ParentNode;
    vi.stubGlobal("document", { activeElement: null });
    // Act
    const done = focusPaneInput(root, "p1");
    // Assert
    expect(done).toBe(true);
    expect((inputOf(view) as unknown as { focus: ReturnType<typeof vi.fn> }).focus).toHaveBeenCalledWith({ preventScroll: true });
    vi.unstubAllGlobals();
  });

  it("should_ReportNothing_When_ThePaneIsNotThere", () => {
    // Arrange / Act / Assert
    expect(focusPaneInput({ querySelector: () => null } as unknown as ParentNode, "p9")).toBe(false);
  });
});
