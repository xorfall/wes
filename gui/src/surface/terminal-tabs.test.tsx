import { terminalPane } from "./split-model";
import { expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { allTerminals, openTerminalTab, selectTerminalTab, closeTerminalTab, terminalDirectory, terminalPlacement, requireTerminalPlacement, placeTerminalTab, max_terminal_tabs } from "./terminal-tabs";
import { focus, max_panes, nextPane, oneP, SESSION_PANE, splitPane, type SplitState } from "./split-model";
import { restoreSplit } from "./split-storage";
import { paneTransitions } from "./pane-transitions";
import { applyPaneCommand } from "./pane-command";
import { read } from "./commands";
import { Split } from "./Split";
const shell = () => oneP(terminalPane("p1", { cwd: "/synthetic" }));
const target = { environment: "QA", target: "remote", revision: `sha256:${"a".repeat(64)}` };
it("adds independent terminals in one pane, selects them and retains each cwd/history", () => {
  const initial = shell(), first = initial.panes[0]!.history!;
  let state = openTerminalTab(initial, "p1"), second = state.panes[0]!.history!;
  expect(state.panes).toHaveLength(1);
  expect(state.layout).toEqual(initial.layout);
  expect(first).not.toBe(second);
  expect(allTerminals(state).map(p => p.cwd)).toEqual(["/synthetic", "/synthetic"]);
  state = terminalDirectory(state, "p1", first, "/first");
  state = terminalDirectory(state, "p1", second, "/second");
  state = selectTerminalTab(state, "p1", first);
  expect(allTerminals(state).map(p => p.cwd)).toEqual(["/first", "/second"]);
  expect(restoreSplit(JSON.parse(JSON.stringify(state)))).toEqual(state);
  expect(read("/terminal-tab env:QA target:remote")).toEqual({kind:"terminal-tab", environment:"QA", target:"remote"});
  expect(read("/tab xterm")).toEqual({kind:"terminal-tab", activate:false});
  expect(read("/terminal-tab unexpected").kind).toBe("trouble");
});
it("bounds terminal capacity without altering existing work and refuses missing callers", () => {
  let state = shell();
  for (let i=1; i<4; i++) state=openTerminalTab(state,"p1");
  expect(allTerminals(state)).toHaveLength(4);
  expect(() => openTerminalTab(state,"p1")).toThrow("The terminal budget is full");
  expect(() => openTerminalTab(oneP(SESSION_PANE),"p1")).toThrow("source terminal");
  expect(() => applyPaneCommand(state,"/close","p1",s=>s,true,undefined,undefined,"gone")).toThrow("source terminal");
});
it("restores every target without launch consent and rejects duplicate history identities", () => {
  const state = openTerminalTab(shell(), "p1", target);
  const restored = restoreSplit(JSON.parse(JSON.stringify(state)));
  expect(allTerminals(restored)[1]).toMatchObject({terminalTarget:target});
  expect(allTerminals(restored)[1]!.terminalLaunch).toBeUndefined();
  expect(allTerminals(restored)[1]!.cwd).toBeUndefined();
  const pane=state.panes[0]!;
  const duplicate={...state,panes:[{...pane,terminalTabs:pane.terminalTabs!.map(t=>({...t,history:pane.history}))}]};
  expect(restoreSplit(duplicate).panes).toEqual([SESSION_PANE]);
  for (const terminalTabs of [[], Array(5).fill({history:pane.history}), [{history:pane.history,cwd:"../escape"}], [{history:pane.history,terminalTarget:{...target,revision:"latest"}}]]) {
    expect(restoreSplit({...state,panes:[{...pane,terminalTabs}]}).panes).toEqual([SESSION_PANE]);
  }
});
it("closes the caller even when another tab is selected and preserves the last-tab fallback", async () => {
  const initial = shell(), first=initial.panes[0]!.history!;
  let state=openTerminalTab(initial,"p1"), second=state.panes[0]!.history!;
  const forget=vi.fn().mockRejectedValueOnce(new Error("lost acknowledgement")).mockResolvedValue({});
  const transitions=paneTransitions({read:()=>state,commit:next=>{state=next;},forget,generation:()=>"g"});
  const closeCaller=(s:SplitState)=>applyPaneCommand(s,"/close","p1",shown=>shown,true,undefined,undefined,first);
  await expect(transitions.change(closeCaller)).rejects.toThrow("lost acknowledgement");
  expect(allTerminals(state)).toHaveLength(2);
  await transitions.change(closeCaller);
  expect(allTerminals(state).map(t=>t.history)).toEqual([second]);
  expect(forget.mock.calls).toEqual([[first],[first]]);
  await transitions.change(s=>applyPaneCommand(s,"/close","p1",shown=>shown,true,undefined,undefined,second));
  expect(state.panes).toEqual([SESSION_PANE]);
});
it("should_CloseThePane_When_TheLastTerminalTabIsClosed", () => {
  // Arrange: a session and a terminal pane beside it
  const state = splitPane(oneP(SESSION_PANE), "right", terminalPane("p2", { cwd: "/synthetic" }));
  // Act
  const next = closeTerminalTab(state, "p2", state.panes[1]!.history!);
  // Assert: the pane went with its last tab, and the session is what remains
  expect(next.panes.map(p => p.id)).toEqual(["p1"]);
  expect(next.focused).toBe("p1");
});
it("should_BecomeTheSession_When_TheLastTabOfTheOnlyPaneIsClosed", () => {
  const state = shell(); const history = state.panes[0]!.history!;
  const next = closeTerminalTab(state, "p1", history);
  expect(next.panes).toEqual([{ id: "p1", title: "session" }]);
});
it("should_OfferToCloseTheLastTerminalTab_ButNeverTheLastWorkspaceTab_When_TheRowsAreDrawn", () => {
  const props = { top: [], prompt: [], context: [] };
  const terminal = renderToStaticMarkup(<Split {...props} state={shell()} />);
  expect(terminal).toContain('aria-label="Close Terminal 1 tab"');
  const workspace = renderToStaticMarkup(<Split {...props} state={oneP({ ...SESSION_PANE, tabs: [{ workspace: undefined }] })} />);
  expect(workspace).not.toContain('aria-label="Close ');
});
it("keeps terminal views mounted when switching tabs or dividing their pane", async () => {
  let state=shell(); const first=state.panes[0]!.history!;
  let tree!:ReactTestRenderer;
  const props={top:[],prompt:[],context:[],content:(p:{history?:string})=><input data-history={p.history}/>};
  await act(async()=>{tree=create(<Split {...props} state={state}/>);});
  const original=tree.root.findByType("input");
  state=openTerminalTab(state,"p1");const second=state.panes[0]!.history!;
  await act(async()=>tree.update(<Split {...props} state={state}/>));
  expect(tree.root.findAllByType("input")[0]).toBe(original);
  expect(tree.root.findAllByProps({role:"tabpanel"}).map(p=>p.props.hidden)).toEqual([true,false]);
  state=selectTerminalTab(state,"p1",first);
  state=splitPane(state,"right",{id:"p2",title:"session"});
  await act(async()=>tree.update(<Split {...props} state={state}/>));
  expect(tree.root.findAllByType("input")[0]).toBe(original);
  state=closeTerminalTab(state,"p1",second);
  await act(async()=>tree.update(<Split {...props} state={state}/>));
  expect(tree.root.findAllByType("input")[0]).toBe(original);
  await act(async()=>tree.unmount());
});
it("shows neither redundant session titles nor the removed footer shortcuts", () => {
  const state=oneP({...SESSION_PANE,workspace:"synthetic"});
  const markup=renderToStaticMarkup(<Split state={state} top={[]} prompt={[]} context={[]}/>);
  expect(markup).not.toContain("split-pane-title");
  expect(markup).not.toContain("adjacent pane");
  expect(markup).not.toContain("focus a pane");
});

it("persists an approved revision in only its exact terminal tab and refuses stale approvals", async () => {
  const { acceptTerminalTarget } = await import("./terminal-tabs");
  const initial = openTerminalTab(shell(), "p1", target);
  const history = initial.panes[0]!.history!;
  const next = { ...target, revision: `sha256:${"b".repeat(64)}` };
  const changed = acceptTerminalTarget(initial, "p1", history, target, next);
  expect(changed.panes[0]!.terminalTabs![0]).toBe(initial.panes[0]!.terminalTabs![0]);
  expect(allTerminals(changed)[1]).toMatchObject({ history, terminalTarget: next });
  expect(restoreSplit(JSON.parse(JSON.stringify(changed))).panes[0]!.terminalTabs![1]!.terminalTarget).toEqual(next);
  expect(() => acceptTerminalTarget(changed, "p1", history, target, next)).toThrow("selection changed");
  expect(() => acceptTerminalTarget(initial, "p1", history, target, { ...next, target: "other" })).toThrow("selection changed");
  expect(() => acceptTerminalTarget(closeTerminalTab(initial, "p1", history), "p1", history, target, next)).toThrow("closed");
});

const tabs = (state: SplitState, id: string) => state.panes.find(p => p.id === id)!.terminalTabs!;
/** p1 a session; p2 and p3 terminals of the same (default) workspace. */
const withTerminals = () => splitPane(splitPane(oneP(SESSION_PANE), "right", terminalPane("p2", { cwd: "/two" })),
  "down", terminalPane("p3", { cwd: "/three" }), false, "p2");
it("should_KeepSelectionFocusAndVisits_When_AnInactiveTerminalTabIsAdded", () => {
  const state = shell(), selected = state.panes[0]!.history;
  const next = openTerminalTab(state, "p1", undefined, undefined, { activate: false });
  expect(next.panes[0]!.history).toBe(selected);
  expect(next.focused).toBe(state.focused);
  expect(next.visited).toBe(state.visited);
  expect(tabs(next, "p1")[1]).toMatchObject({ cwd: "/synthetic" });
  expect(new Set(tabs(next, "p1").map(t => t.history)).size).toBe(2);
});
it("should_OpenATerminalPaneToTheRight_When_TheWorkspaceHasNoTerminal", () => {
  const state = oneP(SESSION_PANE);
  const kept = placeTerminalTab(state, "p1", undefined, false);
  expect(kept.layout).toEqual({ axis: "right", ratio: 0.5, first: { pane: "p1" }, second: { pane: "p2" } });
  expect(kept.focused).toBe("p1");
  expect(kept.panes[1]).toMatchObject({ terminal: true });
  expect(tabs(kept, "p2")[0]!.cwd).toBeUndefined();
  expect(placeTerminalTab(state, "p1", undefined, true).focused).toBe("p2");
});
it("should_AddToTheMostRecentlyVisitedTerminalOfTheSameWorkspace_When_NoPaneIsNamed", () => {
  let state = focus(focus(focus(withTerminals(), "p3"), "p2"), "p1");
  expect(terminalPlacement(state, "p1").pane?.id).toBe("p2");
  const next = placeTerminalTab(state, "p1", undefined, false);
  expect(tabs(next, "p2")).toHaveLength(2);
  expect(tabs(next, "p2")[1]).toMatchObject({ cwd: "/two" });
  expect(next.panes.find(p => p.id === "p2")!.history).toBe(state.panes.find(p => p.id === "p2")!.history);
  expect(next.focused).toBe("p1");
  state = focus(focus(state, "p3"), "p1");
  const active = placeTerminalTab(state, "p1", undefined, true);
  expect(tabs(active, "p3")).toHaveLength(2);
  expect(active.focused).toBe("p3");
  expect(active.panes.find(p => p.id === "p3")!.history).toBe(tabs(active, "p3")[1]!.history);
});
it("should_IgnoreAndRefuseTerminalsOfAnotherWorkspace_When_PlacingATerminalTab", () => {
  const other = splitPane(oneP(SESSION_PANE), "right", { ...terminalPane("p2"), workspace: "other" });
  expect(terminalPlacement(other, "p1").pane).toBeUndefined();
  for (const pane of ["p2", "p1", "p9"]) expect(() => terminalPlacement(other, "p1", pane)).toThrow("not a terminal pane in this workspace");
  const created = placeTerminalTab(other, "p1", undefined, false);
  expect(created.panes).toHaveLength(3);
  expect(created.panes[2]).toMatchObject({ id: "p3", terminal: true });
  expect(created.panes[2]!.workspace).toBeUndefined();
});
it("should_InheritOnlyTheIssuingTabCwd_When_ATerminalAddsToItsOwnPane", () => {
  let state = openTerminalTab(shell(), "p1");
  const [first, second] = tabs(state, "p1").map(t => t.history);
  state = terminalDirectory(terminalDirectory(state, "p1", first!, "/first"), "p1", second!, "/second");
  const next = applyPaneCommand(state, "/tab xterm", "p1", s => s, true, undefined, undefined, first);
  expect(tabs(next, "p1").map(t => t.cwd)).toEqual(["/first", "/second", "/first"]);
  expect(next.panes[0]!.history).toBe(second);
  const targeted = applyPaneCommand(state, "/tabx xterm pane:p1", "p1", s => s, true, undefined, target, first);
  expect(tabs(targeted, "p1")[2]).toMatchObject({ terminalTarget: target, terminalLaunch: true });
  expect(tabs(targeted, "p1")[2]!.cwd).toBeUndefined();
  expect(targeted.panes[0]!.history).toBe(tabs(targeted, "p1")[2]!.history);
  expect(() => applyPaneCommand(state, "/tab xterm", "p1", s => s, true, undefined, undefined, "gone")).toThrow("source terminal");
});
it("should_EnforceTheTerminalBudgetEverywhere_And_ThePaneBudgetOnlyWhenCreating", () => {
  let state = withTerminals();
  while (allTerminals(state).length < max_terminal_tabs()) state = openTerminalTab(state, "p2");
  expect(() => terminalPlacement(state, "p1")).toThrow("terminal budget");
  expect(() => terminalPlacement(state, "p1", "p3")).toThrow("terminal budget");
  expect(() => applyPaneCommand(state, "/rsplit xterm", "p1", s => s)).toThrow("terminal budget");
  expect(() => applyPaneCommand(state, "/tabx xterm", "p1", s => s)).toThrow("terminal budget");
  let full = oneP(SESSION_PANE);
  while (full.panes.length < max_panes()) full = splitPane(full, "right", { id: nextPane(full), title: "session" });
  expect(() => terminalPlacement(full, "p1")).toThrow("pane budget");
  const withTerminal = { ...full, panes: full.panes.map(p => p.id === "p2" ? terminalPane("p2") : p) };
  expect(terminalPlacement(withTerminal, "p1").pane?.id).toBe("p2");
});
it("should_RefuseRatherThanReroute_When_TheSourceOrDestinationChangesAfterPlacement", () => {
  const state = withTerminals(), chosen = terminalPlacement(state, "p1", "p2");
  expect(requireTerminalPlacement(state, chosen, "p2")).toEqual(chosen);
  expect(() => requireTerminalPlacement(openTerminalTab(state, "p2", undefined, undefined, { activate: false }), chosen, "p2")).toThrow("destination changed");
  expect(() => requireTerminalPlacement(closeTerminalTab(state, "p2", state.panes[1]!.history!), chosen, "p2")).toThrow("destination changed");
  const elsewhere = selectTerminalTab(openTerminalTab(state, "p3"), "p3", state.panes[2]!.history!);
  expect(requireTerminalPlacement(elsewhere, chosen, "p2").pane).toBe(chosen.pane);
  // A shell reporting its directory replaces the pane object but leaves the destination as chosen.
  const moved = terminalDirectory(state, "p2", state.panes[1]!.history!, "/tmp/next");
  expect(requireTerminalPlacement(moved, chosen, "p2").pane).toBe(moved.panes[1]);
  const empty = oneP(SESSION_PANE), creating = terminalPlacement(empty, "p1");
  expect(creating.pane).toBeUndefined();
  expect(() => requireTerminalPlacement(splitPane(empty, "down", terminalPane("p2")), creating)).toThrow("destination changed");
  expect(() => requireTerminalPlacement({ ...empty, panes: [{ ...SESSION_PANE }] }, creating)).toThrow("source pane changed");
});
it("should_RefusePlacement_When_ADestinationTabTargetIsReplacedAfterPlacement", async () => {
  // Arrange: p2 holds an inactive targeted tab when the placement is chosen
  const { acceptTerminalTarget } = await import("./terminal-tabs");
  const state = openTerminalTab(withTerminals(), "p2", target, undefined, { activate: false });
  const chosen = terminalPlacement(state, "p1", "p2"), history = tabs(state, "p2")[1]!.history;
  // Act: an approval replaces that tab's revision without changing its history or selection
  const retargeted = acceptTerminalTarget(state, "p2", history, target, { ...target, revision: `sha256:${"b".repeat(64)}` });
  // Assert: the captured placement no longer describes the destination, while a cwd report still does
  expect(retargeted.panes[0]).toBe(state.panes[0]);
  expect(() => requireTerminalPlacement(retargeted, chosen, "p2")).toThrow("destination changed");
  const moved = terminalDirectory(state, "p2", state.panes[1]!.history!, "/tmp/next");
  expect(requireTerminalPlacement(moved, chosen, "p2").pane).toBe(moved.panes[1]);
});
