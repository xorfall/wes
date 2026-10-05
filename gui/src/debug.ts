import type { Event } from "./protocol";

/**
 * What is being kept, and where.
 *
 * <p>Two halves that answer one question, and neither can answer it alone. The engine writes journals,
 * a lock and two tiers of results and the client cannot see any of it. Client settings belong to the
 * selected desktop data folder or browser storage; the Surface cell arrangement lives in memory.
 * The engine does not own that client-side arrangement.
 *
 * <p>The engine reports its own paths rather than the client holding a copy: `--home` moves
 * all of it and the live directory is named after a process id, so a second copy would go wrong.
 */
export interface Kept {
  readonly name: string;
  readonly where: string;
  readonly holds: string;
  /** Whether it survives the thing that wrote it stopping. */
  readonly durable: boolean;
  readonly files: number;
  readonly bytes: number;
}

/** What the browser is holding for this client, read now rather than remembered. */
export function keptHere(): readonly Kept[] {
  return [
    kept("settings", "wes.settings", "theme, layout, follow, preview size and request timeout"),
  ];
}

function kept(name: string, key: string, holds: string): Kept {
  const stored = read(key);
  return {
    name,
    where: `localStorage ${key}`,
    holds,
    durable: true,
    files: stored === undefined ? 0 : 1,
    // Two bytes per unit is what a browser counts this against, whatever the characters are.
    bytes: stored === undefined ? 0 : stored.length * 2,
  };
}

function read(key: string): string | undefined {
  try {
    return window.localStorage.getItem(key) ?? undefined;
  } catch {
    // A private window or a locked-down profile keeps nothing, which is an answer.
    return undefined;
  }
}

/**
 * The whole report, as lines.
 *
 * @param storage what the engine last said, or undefined if it has not answered yet
 * @param facts   what the client knows about itself
 */
export function report(
  storage: Extract<Event, { event: "storage" }> | undefined,
  facts: readonly [string, string][],
  client: { name: string; places: readonly Kept[]; note?: string } = { name: "this browser", places: keptHere() },
): string {
  const lines: string[] = [];
  facts.forEach(([name, said]) => lines.push(`${name.padEnd(14)}${said}`));

  const engine: readonly Kept[] = storage?.places ?? [];
  lines.push("");
  lines.push(
    storage === undefined
      ? "engine        did not answer; it may be an older one, or not running"
      : `engine        workspace '${storage.workspace}'`,
  );
  lines.push(...table(engine));
  if (storage?.retention) {
    lines.push("", "payloads      unique handles by retention reason (not a whole-disk quota)");
    for (const item of storage.retention.classes) lines.push(`  ${item.kind.padEnd(12)}${item.count} results, ${bytes(item.bytes)}`);
    lines.push(`  physical copies: live ${bytes(storage.retention.liveBytes)}, archive ${bytes(storage.retention.archiveBytes)}`);
    lines.push(`  private memory: ${storage.retention.privateCount} results, ${bytes(storage.retention.privateBytes)} logical charge`);
    lines.push("  Automatic cleanup is off. Unknown means no recorded retention reason, not permission to delete.");
  }

  lines.push("");
  lines.push(`client        ${client.name}`);
  lines.push(...(client.note ? [`  ${client.note}`] : table(client.places)));
  return lines.join("\n");
}

/** Durable first, because "does this survive a restart" is the question being asked of a file list. */
function table(places: readonly Kept[]): string[] {
  if (places.length === 0) {
    return ["  nothing"];
  }
  const width = Math.max(...places.map((place) => place.name.length)) + 2;
  // The path and the sentence hang under the name they belong to, not under the size beside it.
  const indent = "      ";
  return places.map((place) =>
    [
      `  ${place.name.padEnd(width)}${(place.durable ? "kept" : "temporary").padEnd(12)}${size(place)}`,
      `${indent}${place.where}`,
      `${indent}${place.holds}`,
    ].join("\n"),
  );
}

/**
 * How much is there. A count is only shown for a place that holds more than one thing — "1 files" is
 * noise, and "56 × 187.5 KB" reads as fifty-six files of 187.5 KB each, which is the wrong number by a
 * factor of fifty-six.
 */
function size(place: Kept): string {
  if (place.files === 0) {
    return "empty";
  }
  return place.files === 1 ? bytes(place.bytes) : `${place.files} files, ${bytes(place.bytes)}`;
}

export function bytes(count: number): string {
  if (count < 1024) {
    return `${count} B`;
  }
  const units = ["KB", "MB", "GB"];
  let size = count / 1024;
  let at = 0;
  while (size >= 1024 && at < units.length - 1) {
    size /= 1024;
    at++;
  }
  return `${size.toFixed(1)} ${units[at]}`;
}
