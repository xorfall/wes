import { workspaceHeaders } from "./workspace-binding";
import { applicationLog } from "./application-log";
export interface AuthWorkspace { workspace: string; generation: string }
export interface AuthOption { schemes: string[]; credentialSlots: string[]; methods: string[] }
export interface AuthOperation { operation: string[]; state: "fixed" | "selected" | "selection-required"; selected: string[] | null; options: AuthOption[] }
export interface ProviderAuthentication { environment: string; revision: string; provider: string; operations: AuthOperation[]; credentials: { slot: string; reference: string; present: boolean; saved?: boolean }[]; persistenceSupported?: boolean; enabled: boolean; grantSeconds: number }
export interface AuthenticationReport extends AuthWorkspace { providers: ProviderAuthentication[] }
type Selection = Pick<ProviderAuthentication, "environment" | "revision" | "provider">;
export type AuthAction = Selection & ({ action: "configure"; auth: Record<string,string[]> } | { action: "supply"; slot: string; value: string; remember?: boolean } | { action: "forget"; slot: string } | { action: "grant" | "revoke" | "enable" });
export async function authenticationRequest(binding: AuthWorkspace, action?: AuthAction, signal?: AbortSignal): Promise<AuthenticationReport | { applied: true }> {
  try {
    const response = await fetch("/environment-authentication", { method: action ? "POST" : "GET", headers: { ...workspaceHeaders(binding.workspace), "X-Wes-Session": binding.generation, ...(action ? { "Content-Type": "application/json" } : {}) }, ...(action ? { body: JSON.stringify(action) } : {}), signal: signal ?? AbortSignal.timeout(30_000) });
    if (!response.ok) {
      // Even an unexpected proxy/error response must not echo submitted material into UI or Logs.
      throw new Error(action?.action === "supply" ? "Credential storage could not be confirmed. Refresh its status before retrying." : (await response.text()).slice(0,4096) || "Authentication request failed.");
    }
    const result = await response.json();
    if (result.workspace !== binding.workspace || result.generation !== binding.generation) throw new Error("Workspace changed; reopen /env.");
    return result;
  } catch (error) {
    if (signal?.aborted) throw error;
    const message = action?.action === "supply" ? "Credential storage could not be confirmed. Refresh its status before retrying." : error instanceof Error ? error.message : "Authentication operation could not be confirmed. Refresh before retrying.";
    applicationLog.add({ level:"error", code:"ENV_AUTH_CONTROL", source:"Authentication", operation:action?.action ?? "Read", workspace:binding.workspace, generation:binding.generation, message });
    throw new Error(message);
  }
}
