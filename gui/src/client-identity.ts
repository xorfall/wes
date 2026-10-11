declare global { interface Window { readonly __WES_WINDOW_CLIENT__?: string } }

/** A host-owned native window outlives its WebContent process. Browser tabs remain ephemeral. */
export function desktopClient(): string | undefined {
  if (typeof window === "undefined" || window.__WES_DESKTOP__ !== true) return undefined;
  const client = window.__WES_WINDOW_CLIENT__;
  return typeof client === "string" && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(client) ? client : undefined;
}

/** The native load callback completes after document scripts start. Wait before creating engines. */
export async function initializeDesktopClient(): Promise<void> {
  if (typeof window === "undefined" || window.__WES_DESKTOP__ !== true || desktopClient()) return;
  await new Promise<void>((resolve, reject) => {
    const cleanup = () => { clearTimeout(timer); window.removeEventListener("wes:window-client", changed); };
    const changed = () => { if (desktopClient()) { cleanup(); resolve(); } };
    const timer = setTimeout(() => { cleanup(); reject(new Error("Native window identity is unavailable. Reload the window before using terminals.")); }, 5000);
    window.addEventListener("wes:window-client", changed);
    changed();
  });
}
