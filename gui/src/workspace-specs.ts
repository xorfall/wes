import { workspaceHeaders } from "./workspace-binding";
/** User inspection of immutable workspace inputs; independent of shared library publication. */
export interface SpecWorkspace { workspace: string; generation: string }
export interface ImportedSpec { environment: string; alias: string; origin: string; revision: string; bytes: number }
export interface ImportedSpecResult { spec: ImportedSpec; source: string }
export async function workspaceSpecs<T>(binding: SpecWorkspace, selection?: ImportedSpec, signal?: AbortSignal): Promise<T> {
  const query = new URLSearchParams();
  if (selection) for (const field of ["environment", "alias", "revision"] as const) query.set(field, selection[field]);
  const response = await fetch(`/workspace-specs?${query}`, { headers: { ...workspaceHeaders(binding.workspace), "X-Wes-Session": binding.generation }, ...(signal ? { signal } : {}) });
  if (!response.ok) throw new Error((await response.text()).slice(0, 4096) || "Could not read workspace APIs.");
  const result = await response.json();
  if (result.workspace !== binding.workspace || result.generation !== binding.generation) throw new Error("Workspace changed; reopen /spec.");
  return result as T;
}
