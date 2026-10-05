import { terminalPane } from "./split-model";
import { expect, it, vi } from "vitest";
import { paneTransitions } from "./pane-transitions";
import { applyPaneCommand } from "./pane-command";
import { close, focus, oneP, SESSION_PANE, splitPane, type SplitState } from "./split-model";
const shell = (state = oneP(SESSION_PANE)) => splitPane(state, "right", terminalPane(`p${state.nextId}`));
function fixture() {
  let state = shell(), generation = "g1";
  const forget = vi.fn<(id: string) => Promise<unknown>>().mockResolvedValue({ forgotten: true });
  const commit = vi.fn((next: SplitState) => { state = next; });
  const transitions = paneTransitions({ read: () => state, commit, forget, generation: () => generation });
  return { transitions, forget, commit, state: () => state, generation: (value: string) => { generation = value; } };
}
it("retires command history before closing and retains the pane after a lost acknowledgement for retry", async () => {
  const f = fixture(), history = f.state().panes[1]!.history!;
  f.forget.mockRejectedValueOnce(new Error("lost acknowledgement"));
  await expect(f.transitions.change(state => close(state, "p2"))).rejects.toThrow("lost acknowledgement");
  expect(f.state().panes[1]!.history).toBe(history);
  expect(f.commit).not.toHaveBeenCalled();
  await f.transitions.change(state => close(state, "p2"));
  expect(f.forget.mock.calls).toEqual([[history], [history]]);
  expect(f.state().panes).toEqual([SESSION_PANE]);
});
it("serializes newer functional changes while forgetting and rejects stale concrete snapshots", async () => {
  const f = fixture(); let finish!: () => void;
  f.forget.mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve; }));
  const stale = focus(f.state(), "p2");
  const closing = f.transitions.change(state => close(state, "p2"));
  const adding = f.transitions.change(state => shell(state));
  const oldSnapshot = f.transitions.change(stale);
  expect(f.state().panes).toHaveLength(2);
  expect(f.commit).not.toHaveBeenCalled();
  finish(); await closing; await adding;
  await expect(oldSnapshot).rejects.toThrow("Layout changed");
  expect(f.state().panes.map(pane => pane.id)).toEqual(["p1", "p3"]);
  expect(f.forget).toHaveBeenCalledOnce();
});
it("forgets replaced and last wesx-closed terminal descriptors but not focus or shell lifetime changes", async () => {
  const f = fixture();
  await f.transitions.change(state => focus(state, "p2"));
  expect(f.forget).not.toHaveBeenCalled();
  await f.transitions.change(state => ({ ...state, panes: state.panes.map(pane => pane.id === "p2" ? { id: pane.id, title: "session" } : pane) }));
  expect(f.forget).toHaveBeenCalledOnce();
  const single = oneP(terminalPane("p1"));
  await f.transitions.change(single);
  await f.transitions.change(state => applyPaneCommand(state, "/close", "p1", shown => shown, true));
  expect(f.forget).toHaveBeenLastCalledWith(single.panes[0]!.history);
  expect(f.state().panes).toEqual([SESSION_PANE]);
});
it("disposal never forgets an open pane and stale generation acknowledgements cannot commit", async () => {
  const f = fixture();
  f.transitions.dispose();
  expect(f.forget).not.toHaveBeenCalled();
  f.transitions.resume(); // React StrictMode effect replay remains usable.
  let finish!: () => void;
  f.forget.mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve; }));
  const pending = f.transitions.change(state => close(state, "p2"));
  f.generation("g2"); finish(); await pending;
  expect(f.commit).not.toHaveBeenCalled();
  expect(f.state().panes).toHaveLength(2);
});
it("does not issue queued old-home forget requests after generation or lifecycle replacement", async () => {
  const f = fixture(); let finish!: () => void;
  f.forget.mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve; }));
  const first = f.transitions.change(state => close(state, "p2"));
  const second = f.transitions.change(state => close(state, "p2"));
  f.generation("different-home"); finish(); await first;
  await expect(second).rejects.toThrow("Workspace changed");
  expect(f.forget).toHaveBeenCalledOnce();
});
it("acknowledges close only after durable layout save and surfaces save failure separately from forget", async () => {
  let state = shell(), flush!: () => void;
  const forget = vi.fn().mockResolvedValue({ forgotten: true });
  const commit = vi.fn(async (next: SplitState) => { state = next; await new Promise<void>(resolve => { flush = resolve; }); });
  const transitions = paneTransitions({ read: () => state, commit, forget, generation: () => "g1" });
  const ack = vi.fn();
  const closing = transitions.change(previous => close(previous, "p2")).then(ack);
  await Promise.resolve(); await Promise.resolve();
  expect(forget).toHaveBeenCalledOnce();
  expect(state.panes).toEqual([SESSION_PANE]);
  expect(ack).not.toHaveBeenCalled();
  flush(); await closing; expect(ack).toHaveBeenCalledOnce();
  state = shell();
  commit.mockImplementationOnce(async next => { state = next; throw new Error("preferences could not be saved"); });
  await expect(transitions.change(previous => close(previous, "p2"))).rejects.toThrow("could not be saved");
  expect(state.panes).toEqual([SESSION_PANE]); // History is gone, but persistence failure is never acknowledged as success.
});
it("does not revive pending work across StrictMode cleanup/setup replay", async () => {
  const f = fixture(); let finish!: () => void;
  f.forget.mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve; }));
  const pending = f.transitions.change(state => close(state, "p2"));
  f.transitions.dispose(); f.transitions.resume();
  finish(); await pending;
  expect(f.commit).not.toHaveBeenCalled();
  await f.transitions.change(state => focus(state, "p2"));
  expect(f.state().focused).toBe("p2");
});

it("publishes tab navigation while earlier saves are pending, but acknowledges only durable writes", async () => {
  let state = shell();
  const saves: { resolve: () => void; reject: (error: Error) => void }[] = [];
  const commit = (next: SplitState) => {
    state = next;
    return new Promise<void>((resolve, reject) => saves.push({resolve, reject}));
  };
  const forget = vi.fn().mockResolvedValue(undefined);
  const transitions = paneTransitions({read: () => state, commit, forget, generation: () => "g1"});
  const acknowledged = vi.fn();
  const first = transitions.change(previous => focus(previous, "p1")).then(acknowledged);
  const second = transitions.change(previous => focus(previous, "p2"));
  expect(state.focused).toBe("p2");
  expect(saves).toHaveLength(2);
  expect(acknowledged).not.toHaveBeenCalled();
  const failed = expect(second).rejects.toThrow("save failed");
  saves[1]!.reject(new Error("save failed")); await failed;
  saves[0]!.resolve(); await first;
  expect(acknowledged).toHaveBeenCalledOnce();
  expect(state.focused).toBe("p2");
  expect(forget).not.toHaveBeenCalled();
});

it("releases the retirement queue after publication without waiting for its save", async () => {
  let state = shell(), forgotten!: () => void, saved!: () => void;
  const flush = new Promise<void>(resolve => { saved = resolve; });
  const transitions = paneTransitions({read: () => state, generation: () => "g1",
    forget: () => new Promise<void>(resolve => { forgotten = resolve; }),
    commit: next => { state = next; return flush; },
  });
  const closing = transitions.change(previous => close(previous, "p2"));
  const adding = transitions.change(previous => shell(previous));
  forgotten();
  for (let i = 0; i < 8; i++) await Promise.resolve();
  expect(state.panes.map(pane => pane.id)).toEqual(["p1", "p3"]);
  saved(); await closing; await adding;
});
