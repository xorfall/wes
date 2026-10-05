/**
 * The data folder's credential vault, where the platform has no system secure store. Remembered
 * credentials are readable only while it is unlocked; every restart starts locked. Passwords go
 * to the host in a request body and are never kept, logged or echoed here.
 */
import { useEffect, useRef, useState } from "react";
import { applicationLog } from "./application-log";

export type VaultState = "absent" | "locked" | "unlocked";
/** `system`: the platform's own secure store keeps remembered credentials (the Keychain on macOS). */
export type VaultReport = { kind: "system" } | { kind: "vault"; state: VaultState };
export type VaultAction =
  | { action: "create" | "unlock"; password: string }
  | { action: "lock" }
  | { action: "reset"; confirm: "reset" };

/** Matches the host's minimum; checked here only to say so before a request. */
export const MIN_PASSWORD_CHARS = 8;
const TIMEOUT_MS = 30_000;
const MESSAGE_CHARS = 512;
/** Every open vault control re-reads after any one of them changes the vault. */
const CHANGED = "changed";
const changes = new EventTarget();
const STATES: readonly VaultState[] = ["absent", "locked", "unlocked"];

function parse(body: unknown): VaultReport {
  const report = body as { kind?: unknown; state?: unknown } | null;
  if (report?.kind === "vault" && STATES.includes(report.state as VaultState)) return { kind: "vault", state: report.state as VaultState };
  return { kind: "system" };
}

export async function vaultRequest(action?: VaultAction, signal?: AbortSignal): Promise<VaultReport> {
  try {
    const response = await fetch("/credential-vault", {
      method: action ? "POST" : "GET", cache: "no-store",
      ...(action ? { headers: { "Content-Type": "application/json" }, body: JSON.stringify(action) } : {}),
      signal: signal ?? AbortSignal.timeout(TIMEOUT_MS),
    });
    if (!response.ok) throw new Error((await response.text()).slice(0, MESSAGE_CHARS) || "The credential vault could not be changed.");
    const report = parse(await response.json());
    if (action) changes.dispatchEvent(new Event(CHANGED));
    return report;
  } catch (error) {
    if (signal?.aborted) throw error;
    const message = error instanceof Error && error.message ? error.message : "The credential vault could not be reached.";
    applicationLog.add({ level: "error", code: "CREDENTIAL_VAULT", source: "Credential vault", operation: action?.action ?? "Read", message });
    throw new Error(message);
  }
}

export interface VaultControl {
  readonly report?: VaultReport;
  readonly busy: boolean;
  readonly message?: { readonly text: string; readonly failed: boolean };
  /** Resolves true when the host confirmed the action. */
  readonly act: (action: VaultAction, success?: string) => Promise<boolean>;
}

/** The vault's state, read when `enabled` and again after any vault change in this window. */
export function useCredentialVault(enabled = true): VaultControl {
  const [report, setReport] = useState<VaultReport>();
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<VaultControl["message"]>();
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => {
    if (!enabled) return;
    const abort = new AbortController();
    const read = () => void vaultRequest(undefined, abort.signal)
      .then(value => { if (!abort.signal.aborted) setReport(value); })
      .catch(error => { if (!abort.signal.aborted) setMessage({ text: (error as Error).message, failed: true }); });
    read();
    changes.addEventListener(CHANGED, read);
    return () => { abort.abort(); changes.removeEventListener(CHANGED, read); };
  }, [enabled]);
  const act = async (action: VaultAction, success?: string) => {
    if (busy) return false;
    setBusy(true); setMessage(undefined);
    try {
      const value = await vaultRequest(action);
      if (mounted.current) { setReport(value); if (success) setMessage({ text: success, failed: false }); }
      return true;
    } catch (error) {
      if (mounted.current) setMessage({ text: (error as Error).message, failed: true });
      return false;
    } finally { if (mounted.current) setBusy(false); }
  };
  return { ...(report ? { report } : {}), busy, ...(message ? { message } : {}), act };
}

/** The vault must be created or unlocked before a credential can be remembered. */
export function vaultNeeds(report: VaultReport | undefined): "create" | "unlock" | undefined {
  if (report?.kind !== "vault") return undefined;
  return report.state === "absent" ? "create" : report.state === "locked" ? "unlock" : undefined;
}
