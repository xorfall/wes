import { terminalPane } from "./split-model";
import { expect, it } from "vitest";
import { applyPaneCommand } from "./pane-command";
import { oneP, SESSION_PANE, splitPane, type Shown } from "./split-model";

const resolve = (shown: Shown) => shown;

it("keeps the final wes context and refuses a stale terminal route into it", () => {
  const state = splitPane(oneP(SESSION_PANE), "right", terminalPane("p2"), true);
  expect(applyPaneCommand(state, "/close", "p1", resolve)).toBe(state);
  expect(() => applyPaneCommand(state, "/close", "p1", resolve, true)).toThrow("source terminal tab has closed");
});

it("does not count a related-work projection as a surviving wes command context", () => {
  const state = splitPane(oneP(SESSION_PANE), "right", {
    id: "p2", title: "related", value: { node: "result", generation: "fixture", label: "result", related: true },
  });
  expect(applyPaneCommand(state, "/close", "p1", resolve)).toBe(state);
  expect(applyPaneCommand(state, "/close", "p2", resolve).panes).toEqual([SESSION_PANE]);
});

it("keeps a sole screen's return-to-session context without dismissing or replacing its screen", () => {
  const screen = { id: "p1", title: "/edit", shows: { screen: "edit" as const } };
  const sole = oneP(screen);
  for (const state of [sole, splitPane(sole, "right", terminalPane("p2"))]) {
    expect(applyPaneCommand(state, "/close", "p1", resolve)).toBe(state);
    expect(() => applyPaneCommand(state, "/close", "p1", resolve, true)).toThrow("source terminal tab has closed");
    expect(state.panes[0]).toBe(screen);
  }
});

it("closes an xterm independently and returns the last terminal pane to a session", () => {
  const state = splitPane(oneP(SESSION_PANE), "right", terminalPane("p2"), true);
  const remaining = applyPaneCommand(state, "/close", "p2", resolve, true);
  expect(remaining.panes).toEqual([SESSION_PANE]);
  expect(remaining.focused).toBe("p1");
  const soleTerminal = oneP(terminalPane("p7"));
  const session = applyPaneCommand(soleTerminal, "/close", "p7", resolve, true);
  expect(session.panes).toEqual([{ id: "p7", title: "session" }]);
  expect(applyPaneCommand(session, "/close", "p7", resolve)).toBe(session);
});
