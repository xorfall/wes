import type { EnvironmentContext, Event } from "./protocol";
import type { Engine } from "./engine";

export type Environments = Extract<Event, { event: "environments" }>;
export type DraftContext = ReturnType<Engine["compositionInfo"]>;

export function sameContext(a: EnvironmentContext | undefined, b: EnvironmentContext | undefined): boolean {
  if (!a || !b) return a === b;
  return a.selected === b.selected && Object.keys(a.revisions).length === Object.keys(b.revisions).length
    && Object.entries(a.revisions).every(([name, revision]) => b.revisions[name] === revision);
}

export function contextStatus(state: Environments | undefined, selected: EnvironmentContext | undefined, draft: DraftContext) {
  const context = draft ? draft.context : selected;
  const name = context?.selected;
  const label = name ?? (context ? "Choose environment" : state ? "Local" : "Loading…");
  const missing = !!name && !!state && !state.revisions[name];
  const stale = !!name && !!state && !!state.revisions[name] && context?.revisions[name] !== state.revisions[name];
  const disabled = !!name && !!state && !missing && !state.enabled[name];
  const changed = !!draft && !sameContext(draft.context, selected);
  return { label, name, missing, stale, disabled, changed, sessionChanged: !!draft?.sessionChanged,
    unselected: !!context && !name };
}
