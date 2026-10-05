import { allTerminals } from "./terminal-tabs";
import type { SplitState } from "./split-model";

type Change = SplitState | ((state: SplitState) => SplitState);
/** Serialize explicit layout changes while native history retirement is in flight. */
export function paneTransitions(options: {
  read: () => SplitState; commit: (state: SplitState) => void | Promise<void>;
  forget: (history: string) => Promise<unknown>; generation: () => string | undefined;
}) {
  let pending: Promise<void> | undefined;
  let disposed = false;
  let lifetime = 0;
  return {
    change(change: Change): Promise<void> {
      const base = options.read(), generation = options.generation(), owner = lifetime;
      // Publication is synchronous; durability belongs to the caller's acknowledgement,
      // not the queue that admits the next navigation. Retirement still holds that queue.
      let saved: void | Promise<void> = undefined;
      const apply = () => {
        if (disposed || owner !== lifetime) return;
        if (options.generation() !== generation) throw new Error("Workspace changed; the pane change was discarded.");
        const previous = options.read();
        if (typeof change !== "function" && previous !== base) throw new Error("Layout changed while closing a terminal. Retry the pane action.");
        const next = typeof change === "function" ? change(previous) : change;
        const retained = new Set(allTerminals(next).map(pane => pane.history));
        const removed = allTerminals(previous).filter(pane => pane.history && !retained.has(pane.history));
        const commit = () => {
          if (!disposed && owner === lifetime && options.generation() === generation) saved = options.commit(next);
        };
        if (!removed.length) return commit();
        return Promise.all(removed.map(pane => options.forget(pane.history!))).then(commit);
      };
      let operation: Promise<void>;
      if (pending) operation = pending.then(apply, apply);
      else {
        try {
          const result = apply();
          if (!result) return Promise.resolve(saved);
          operation = result;
        } catch (error) { return Promise.reject(error); }
      }
      const tracked = operation.finally(() => { if (pending === tracked) pending = undefined; });
      pending = tracked;
      return tracked.then(() => saved);
    },
    resume() { disposed = false; },
    dispose() { disposed = true; lifetime += 1; },
  };
}
