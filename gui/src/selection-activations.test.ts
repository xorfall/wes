import { describe, expect, it, vi } from "vitest";
import { SelectionActivations } from "./selection-activations";
import type { Event, SelectionInput } from "./protocol";
const row = (index: number): SelectionInput => ({ kind: "row", source: "source", run: "snapshot", row: index });
const planned = (cell = "cell"): Extract<Event, { event: "planned" }> => ({ event: "planned", cell, text: "fixture read", nodes: ["target"] });

describe("selection activation ownership", () => {
  it("reconnect preserves waiting drafts and unresolved identities without replay; workspace changes clear them", async () => {
    const send = vi.fn().mockResolvedValue(undefined);
    const selections = new SelectionActivations(send, () => "next");
    const greeting = { event: "session", workspace: "fixture", generation: "one", cells: [] } as const;
    selections.observe(greeting);
    selections.wait("cell", "fixture read", "source", undefined);
    selections.observe(greeting);
    expect(selections.hasSource("source")).toBe(true);
    selections.activate(row(0)); selections.activate(row(1));
    selections.observe(greeting);
    expect(send).toHaveBeenCalledTimes(1);
    selections.observe(planned()); await Promise.resolve();
    expect(send).toHaveBeenCalledTimes(1);
    selections.activate(row(2));
    expect(send).toHaveBeenCalledTimes(2);
    selections.observe({ ...greeting, generation: "two" });
    selections.activate(row(3));
    expect(send).toHaveBeenCalledTimes(2);
  });
  it("Enter creates a waiting draft, first acknowledgement owns the only target, latest pending activation wins", async () => {
    const send = vi.fn().mockResolvedValue(undefined), updates = vi.fn();
    const selections = new SelectionActivations(send, () => "next");
    selections.subscribe(updates);
    selections.activate(row(0));
    selections.wait("cell", "fixture read", "source", undefined);
    expect(send).not.toHaveBeenCalled();
    expect(updates.mock.lastCall?.[0].state).toBe("waiting");
    selections.activate(row(1)); selections.activate(row(2)); selections.activate(row(3));
    expect(send).toHaveBeenCalledTimes(1);
    selections.observe(planned()); await Promise.resolve();
    expect(send).toHaveBeenCalledTimes(2);
    expect(send.mock.calls[1]).toEqual(["next", "fixture read", row(3), undefined, "cell"]);
  });
  it("unknown first outcome never causes another target or replay", async () => {
    const send = vi.fn().mockRejectedValue(new Error("connection lost"));
    const selections = new SelectionActivations(send);
    selections.wait("cell", "fixture read", "source", undefined);
    selections.activate(row(0)); await Promise.resolve();
    selections.activate(row(1)); await Promise.resolve();
    expect(send).toHaveBeenCalledTimes(1);
    selections.reset(); selections.observe(planned());
    expect(send).toHaveBeenCalledTimes(1);
  });
  it("a fresh activation after explicit refusal creates exactly one binding, including after history replay", () => {
    const send = vi.fn().mockResolvedValue(undefined);
    let sequence = 0;
    const selections = new SelectionActivations(send, () => `attempt-${++sequence}`);
    selections.wait("cell", "fixture read", "source", undefined);
    selections.activate(row(0));
    selections.observe({ ...planned(), nodes: [], failure: "source changed" });
    selections.activate(row(1));
    const accepted = { ...planned("attempt-1"), selectionSource: "source" };
    selections.observe(accepted);
    selections.observe({ ...accepted, restored: true });
    selections.activate(row(2));
    expect(send).toHaveBeenCalledTimes(3);
    expect(send.mock.calls[2]).toEqual(["attempt-2", "fixture read", row(2), undefined, "attempt-1"]);
  });
  it("new same-value activations run again; unrelated selection and clearing run nothing", async () => {
    const send = vi.fn().mockResolvedValue(undefined);
    let identity = 0;
    const selections = new SelectionActivations(send, () => `attempt-${++identity}`);
    selections.wait("cell", "fixture read", "source", undefined);
    selections.activate(row(0)); selections.observe(planned());
    selections.activate(row(0)); selections.activate(row(0));
    selections.activate({ ...row(0), source: "another" });
    expect(send).toHaveBeenCalledTimes(3);
    expect(send.mock.calls.map(call => call[0])).toEqual(["cell", "attempt-1", "attempt-2"]);
  });
  it("reopening restores bindings but never an activation intent", () => {
    const send = vi.fn().mockResolvedValue(undefined);
    const selections = new SelectionActivations(send, () => "new-attempt");
    selections.observe({ ...planned(), selectionSource: "source", restored: true });
    expect(send).not.toHaveBeenCalled();
    selections.activate(row(2));
    expect(send).toHaveBeenCalledWith("new-attempt", "fixture read", row(2), undefined, "cell");
  });
});
