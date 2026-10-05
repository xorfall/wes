/**
 * The registry in force for this client: core entries and data-home overrides.
 * Cells and result windows share this store and receive its changes together.
 */
import { Registry, type HomeFile } from "./registry";

let current = Registry.core();
const listeners = new Set<() => void>();

function publish(next: Registry) {
  current = next;
  for (const listener of listeners) listener();
}

export const registryStore = {
  get(): Registry {
    return current;
  },
  subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  /** The data home's `presentations/` directory as the engine served it. Keeps last valid entries. */
  setHome(files: readonly HomeFile[], unreadable: readonly { readonly name: string; readonly message: string }[] = []) {
    publish(current.withHome(files, unreadable));
  },
  /** Back to the core entries only (tests). */
  reset() {
    publish(Registry.core());
  },
};
