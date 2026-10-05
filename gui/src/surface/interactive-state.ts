import { useEffect, useMemo } from "react";
import type { Workspace } from "../workspace";

export interface InteractiveSnapshot {
  readonly answer: string;
  readonly busy: boolean;
  readonly problem?: string;
}

/** Session-owned state survives a cell moving between the pinned strip and scrollback. */
export function createInteractiveState() {
  let snapshot: InteractiveSnapshot = { answer: "", busy: false };
  const listeners = new Set<() => void>();
  return {
    getSnapshot: () => snapshot,
    subscribe: (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; },
    update: (next: Partial<InteractiveSnapshot>) => {
      snapshot = { ...snapshot, ...next };
      for (const listener of listeners) listener();
    },
  };
}
export type InteractiveState = ReturnType<typeof createInteractiveState>;
export type InteractiveStates = Map<string, InteractiveState>;
export const conversationKey = (node: string, run: string) => JSON.stringify([node, run]);

export function useInteractiveStates(workspace: Workspace, generation: string | undefined): InteractiveStates {
  const states = useMemo(() => new Map<string, InteractiveState>(), [generation]);
  useEffect(() => {
    const active = new Set(workspace.nodes.filter(node => node.interactive && node.conversationActive && node.state === "running" && node.run)
      .map(node => conversationKey(node.id, node.run!)));
    for (const key of states.keys()) if (!active.has(key)) states.delete(key);
  }, [workspace, states]);
  return states;
}
