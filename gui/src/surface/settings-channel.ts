/**
 * How a settings window tells the session what was chosen.
 *
 * The session's window owns the preferences: it holds the layout, it persists, it is the one
 * whose save must win. A settings window therefore persists nothing; it announces the keys that
 * changed, and the session applies them as it applies its own `/settings` commands. The channel
 * is the platform's broadcast between windows of one origin; where there is none (a test), a
 * change goes nowhere and says so.
 */
import type { Settings } from "../settings";

const CHANNEL = "wes-settings";

export type SettingsChange = Partial<Omit<Settings, "paneLayout">>;

interface Message { readonly kind: "settings"; readonly change: SettingsChange }

const channel = (): BroadcastChannel | undefined =>
  typeof BroadcastChannel === "undefined" ? undefined : new BroadcastChannel(CHANNEL);

/** The keys whose values differ between two settings, never the layout. */
export function changed(was: Settings, next: Settings): SettingsChange {
  const change: Record<string, unknown> = {};
  for (const key of Object.keys(next) as (keyof Settings)[]) {
    if (key === "paneLayout") continue;
    if (JSON.stringify(was[key]) !== JSON.stringify(next[key])) change[key] = next[key];
  }
  return change as SettingsChange;
}

/** Announces a change to every other window; true when there was a channel to say it on. */
export function announceSettings(change: SettingsChange): boolean {
  if (Object.keys(change).length === 0) return true;
  const out = channel();
  if (!out) return false;
  const message: Message = { kind: "settings", change };
  out.postMessage(message);
  out.close();
  return true;
}

/** Applies every change announced by another window, until the returned function is called. */
export function followSettings(apply: (change: SettingsChange) => void): () => void {
  const inbox = channel();
  if (!inbox) return () => undefined;
  const hear = (event: MessageEvent<unknown>) => {
    const message = event.data as Partial<Message> | null;
    if (message && message.kind === "settings" && message.change && typeof message.change === "object") apply(message.change);
  };
  inbox.addEventListener("message", hear);
  return () => { inbox.removeEventListener("message", hear); inbox.close(); };
}
