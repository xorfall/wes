/** The complete server-sanitized error, shared by inspection, copy and Logs. */
import type { ErrorRecord, SourceLocation } from "./protocol";
export function locationText(location: SourceLocation, at = 0): string {
  return `${at ? "called from " : ""}${location.source} · line ${location.line}, column ${location.column}`;
}
/** Compact card labels; exact source identities remain in full error text and logs. */
export function locationLines(locations: readonly SourceLocation[], cell?: string): string[] {
  return locations.map((location, at) => {
    const own = cell !== undefined && location.source === `cell ${cell}`;
    const source = own ? "this cell" : location.source.replace(
      /^cell ([0-9a-f]{8})-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i, "cell $1",
    );
    const repeatedOwn = own && at > 0 && locations[at - 1]!.source === location.source;
    return `${at ? "called from " : ""}${repeatedOwn ? "" : `${source} · `}line ${location.line}, column ${location.column}`;
  });
}
export function issueText(issue: ErrorRecord["issues"][number]): string {
  return `${issue.path || "/"} · ${issue.code}: ${issue.message}`;
}
export function failureText(reason: string | undefined, error?: ErrorRecord): string {
  const message = reason || error?.message || "";
  if (!error || (!error.issues.length && !error.locations?.length)) return message;
  return [`${error.code}: ${message}`, ...(error.locations ?? []).map(locationText), ...error.issues.map(issueText)].join("\n");
}
