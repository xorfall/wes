/** Names are labels, never filesystem paths. The backend remains authoritative. */
export function workspaceName(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0
    && new TextEncoder().encode(value).length <= 96 && !value.includes("..")
    && !/[\/\\\u0000-\u001f\u007f-\u009f]/u.test(value);
}

export function workspaceHeaders(workspace?: string): Record<string, string> {
  return workspace === undefined ? {} : { "X-Wes-Workspace": encodeURIComponent(workspace) };
}

/** Opening is explicit; reading an unknown binding must never create a workspace. */
export async function openWorkspace(name: string, create = true, identity?: string): Promise<string> {
  if (!workspaceName(name)) throw new Error("Workspace names must contain 1–96 bytes, without path separators, '..' or control characters.");
  const response = await fetch("/workspaces", { method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, create, ...(identity ? { identity } : {}) }), signal: AbortSignal.timeout(30_000) });
  if (!response.ok) throw new Error((await response.text()) || "The workspace could not be opened.");
  const opened = await response.json() as { workspace?: unknown; generation?: unknown; identity?: unknown };
  if (opened.workspace !== name || typeof opened.generation !== "string") throw new Error("The server did not confirm the requested workspace.");
  if (!workspaceIdentity(opened.identity)) throw new Error("The server did not confirm a stable workspace identity.");
  if (identity !== undefined && opened.identity !== identity) throw new Error("Workspace identity changed; reopen it explicitly before binding this saved view.");
  return opened.identity;
}

export function workspaceIdentity(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= 128 && !/[\u0000-\u0020\u007f-\u009f]/u.test(value);
}
/** Restored labels must never silently bind a new workspace that reused a deleted name. */
export async function restoreWorkspace(name: string, identity?: string): Promise<void> {
  if (!workspaceIdentity(identity)) throw new Error(`Reopen ${name} explicitly with /tabx to confirm this saved workspace binding.`);
  await openWorkspace(name, false, identity);
}

/** Retry an already-saved binding after an explicit opening gesture. */
export function announceWorkspaceOpen(name: string): void {
  if (typeof window !== "undefined") window.dispatchEvent?.(new CustomEvent("wes-workspace-opened", { detail: name }));
}
