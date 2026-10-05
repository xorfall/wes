/** Desktop client storage is independent of its ephemeral HTTP origin. No engine settings here. */
declare global { interface Window { __WES_DESKTOP__?: boolean } }
export interface PersistenceState { saving: boolean; error?: string }
let seed: unknown;
let ready = false;
let lastSaved = "";
let pending: string | undefined;
let draining: Promise<void> | undefined;
let state: PersistenceState = { saving: false };
const listeners = new Set<() => void>();
export const desktopSnapshot = (): unknown => seed;
export const persistenceState = (): PersistenceState => state;
export function subscribePersistence(listener: () => void): () => void {
  listeners.add(listener); return () => { listeners.delete(listener); };
}
function notify(next: PersistenceState): void {
  if (state.saving === next.saving && state.error === next.error) return;
  state = next; listeners.forEach(listener => listener());
}

/** Must finish before App mounts: its initial save effect must never race the stored settings. */
export async function initializeDesktopPreferences(fallback: unknown): Promise<void> {
  if (!window.__WES_DESKTOP__) return;
  seed = fallback;
  const abort = new AbortController();
  const timeout = setTimeout(() => abort.abort(), 5000);
  try {
    const response = await fetch("/client-preferences", { cache: "no-store", signal: abort.signal });
    if (!response.ok) throw new Error("preferences unavailable");
    const body: unknown = await response.json();
    if (!body || typeof body !== "object" || !("settings" in body)) throw new Error("invalid preferences");
    const value = (body as {settings: unknown}).settings;
    if (value !== null && (typeof value !== "object" || Array.isArray(value))) throw new Error("invalid preferences");
    seed = value ?? fallback;
    lastSaved = value === null ? "" : JSON.stringify(value);
    ready = true;
  } catch {
    // Defaults may render, but never overwrite an unreadable existing file.
    ready = false;
    notify({saving:false, error:"UI preferences could not be loaded. Restart to retry; saved settings have not been overwritten."});
  } finally { clearTimeout(timeout); }
}

/** One write at a time; coalesce rapid changes and acknowledge only after durable backend save. */
export function saveDesktopPreferences(value: unknown): Promise<void> {
  if (!ready) return Promise.resolve();
  seed = value;
  const body = JSON.stringify(value);
  if (!draining && body === lastSaved) { notify({saving:false}); return Promise.resolve(); }
  pending = body;
  notify({saving:true});
  if (!draining) {
    draining = Promise.resolve().then(async () => {
      try {
        while (pending !== undefined) {
          const next = pending; pending = undefined;
          if (next === lastSaved) continue;
          const response = await fetch("/client-preferences", {
            // Browsers cap outstanding keepalive bodies at 64 KiB. Larger UI layouts use
            // an ordinary request and the existing explicit flush before folder switches.
            method:"PUT", headers:{"Content-Type":"application/json"}, body:next, keepalive:new TextEncoder().encode(next).length<=60*1024,
          });
          if (!response.ok) throw new Error("preferences not saved");
          lastSaved = next;
        }
        notify({saving:false});
      } catch {
        pending = undefined;
        notify({saving:false, error:"UI preferences could not be saved. Change a setting to retry before closing the app."});
      } finally { draining = undefined; }
    });
  }
  return draining;
}

/** A data-folder switch must not race or silently drop a queued preference write. */
export async function flushDesktopPreferences(): Promise<void> {
  if (draining) {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([draining, new Promise<never>((_, reject) => {
        timer = setTimeout(() => {
          const error = "Saving layout and preferences is still pending. You can keep switching tabs; retry this operation after saving finishes.";
          notify({ saving: true, error });
          reject(new Error(error));
        }, 5000);
      })]);
    } finally { clearTimeout(timer); }
  }
  if (state.error) throw new Error(state.error);
}
