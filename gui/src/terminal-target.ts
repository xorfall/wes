/** A remembered destination is a preference, never permission to reconnect. */
export type TerminalTarget = { environment: string; revision: string; target: string };
export const targetKey = (target?: TerminalTarget) => target ? JSON.stringify([target.environment, target.revision, target.target]) : "";
export function readTerminalTarget(value: unknown): TerminalTarget | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return;
  const v = value as Record<string, unknown>;
  const name = (s: unknown): s is string => typeof s === "string" && s.length > 0 && s.length <= 128 && !/[\u0000-\u0020]/.test(s);
  if (!name(v.environment) || !name(v.target) || typeof v.revision !== "string" || !/^sha256:[0-9a-f]{64}$/.test(v.revision)) return;
  return { environment: v.environment, revision: v.revision, target: v.target };
}
