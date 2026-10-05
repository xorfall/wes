import { terminalPane } from "./split-model";
import { expect, it, vi } from "vitest";
import { applyPaneCommand } from "./pane-command";
import { close, oneP, SESSION_PANE } from "./split-model";
import { restoreSplit } from "./split-storage";
const create = () => applyPaneCommand(oneP(SESSION_PANE), "/rsplit xterm", "p1", shown => shown);
it("creates opaque per-tab identities and restores them independently of process or pN labels", () => {
  const first = create();
  const both = applyPaneCommand(first, "/bsplit xterm", "p2", shown => shown);
  const ids = both.panes.filter(pane => pane.terminal).map(pane => pane.history);
  expect(ids[0]).toMatch(/^[a-f0-9-]{36}$/);
  expect(new Set(ids).size).toBe(2);
  expect(restoreSplit(JSON.parse(JSON.stringify(both)))).toEqual(both);
  expect(create().panes[1]!.history).not.toBe(ids[0]);
  const recreated = applyPaneCommand(close(first, "p2"), "/rsplit xterm", "p1", shown => shown);
  expect(recreated.panes[1]!.history).not.toBe(ids[0]);
});
it("rejects missing, retired, malformed and duplicate histories without inventing identities", () => {
  const state = create(), terminal = state.panes[1]!;
  const second = applyPaneCommand(state, "/bsplit xterm", "p2", shown => shown);
  const { terminalTabs: _tabs, ...oldPane } = terminal;
  const { history: _active, ...noActive } = terminal;
  const invalid = [
    oldPane, noActive, { ...terminal, cwd: "/old-pane-owned" },
    { ...terminal, terminalLaunch: true },
    { ...terminal, terminalTarget: { environment: "QA", target: "local", revision: `sha256:${"a".repeat(64)}` } },
    { ...SESSION_PANE, id: terminal.id, history: terminal.history },
    { ...terminal, terminalTabs: [{ history: "../../other-home" }] },
    { ...terminal, terminalTabs: [{}] },
    { ...terminal, history: "00000000-0000-0000-0000-000000000000" },
    { ...terminal, history: "22222222-2222-4222-8222-222222222222" },
  ];
  const random = vi.spyOn(crypto, "randomUUID");
  try {
    for (const pane of invalid) expect(restoreSplit({ ...state, panes: [state.panes[0], pane] }).panes).toEqual([SESSION_PANE]);
    const duplicate = { ...second, panes: second.panes.map(pane => pane.terminal
      ? { ...pane, history: terminal.history, terminalTabs: terminal.terminalTabs } : pane) };
    expect(restoreSplit(duplicate).panes).toEqual([SESSION_PANE]);
    expect(restoreSplit(JSON.parse(JSON.stringify(state)))).toEqual(state);
    expect(random).not.toHaveBeenCalled();
  } finally { random.mockRestore(); }
});

it("preserves validated terminal target preferences and rejects forged target selectors", () => {
  const target={environment:"qa",revision:`sha256:${"c".repeat(64)}`,target:"server"};
  const layout=oneP(terminalPane("p1", { terminalTarget:target }));
  expect(restoreSplit(layout).panes[0]!.terminalTabs![0]!.terminalTarget).toEqual(target);
  expect(restoreSplit({...layout,panes:[{...layout.panes[0],terminalTabs:[{history:layout.panes[0]!.history,terminalTarget:{...target,revision:"latest"}}]}]}).panes[0]!.terminal).toBeUndefined();
});

it("drops the fresh terminal launch gesture when restoring a captured destination", () => {
  const target = { environment: "DEV", revision: `sha256:${"a".repeat(64)}`, target: "remote" };
  const state = oneP(terminalPane("p1", { terminalLaunch: true, terminalTarget: target }));
  const restored = restoreSplit(JSON.parse(JSON.stringify(state)));
  expect(restored.panes[0]!.terminalTabs![0]!.terminalTarget).toEqual(target);
  expect(restored.panes[0]!.terminalTabs![0]!.terminalLaunch).toBeUndefined();
});
