import { useSyncExternalStore } from "react";
import { persistenceState, subscribePersistence } from "../desktop-preferences";

/** Save feedback must not redraw retained sessions, values or terminal panes. */
export function SaveStatus() {
  const persistence = useSyncExternalStore(subscribePersistence, persistenceState, persistenceState);
  return <div role="status" aria-live="polite" aria-atomic="true" className="surface-save-status" data-error={persistence.error ? "true" : undefined}>
    {persistence.error ?? (persistence.saving ? "Saving layout and preferences…" : "")}
  </div>;
}
