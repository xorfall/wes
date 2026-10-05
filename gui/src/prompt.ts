import { UNFOCUSED, type Focus } from "./focus";

/** Leave arrows inside multiline text and active selections to the textarea. */
export function atHistoryBoundary(text: string, start: number, end: number, direction: -1 | 1): boolean {
  return start === end && (direction === -1
    ? !text.slice(0, start).includes("\n")
    : !text.slice(end).includes("\n"));
}

export interface Recall {
  readonly scope: string;
  readonly entries: readonly string[];
  readonly index: number;
  readonly draft: string;
}

/** Restored cells contain expanded commands; recover only fragments compatible with this focus. */
export function recallEntries(commands: readonly string[], focus: Focus = UNFOCUSED): readonly string[] {
  const prefix = focus.template.slice(0, focus.hole);
  const suffix = focus.template.slice(focus.hole + 1);
  const entries: string[] = [];
  for (const command of commands.slice(-500)) {
    let typed: string;
    if (command.length >= prefix.length + suffix.length && command.startsWith(prefix) && command.endsWith(suffix)) {
      typed = command.slice(prefix.length, command.length - suffix.length);
    } else if (command.startsWith(":") || command.startsWith("/")) typed = command;
    else continue;
    if (typed.trim() && entries.at(-1) !== typed) entries.push(typed);
  }
  return entries;
}

/** Freeze the navigation list until editing or returning to the draft; replay may arrive in batches. */
export function moveRecall(
  commands: readonly string[], focus: Focus, scope: string, text: string,
  current: Recall | undefined, direction: -1 | 1,
): { readonly text: string; readonly recall?: Recall } {
  const session = current?.scope === scope ? current : undefined;
  const entries = session?.entries ?? recallEntries(commands, focus);
  if (entries.length === 0 || (session === undefined && direction === 1)) return { text };
  const index = session === undefined ? entries.length - 1 : Math.max(0, session.index + direction);
  const draft = session?.draft ?? text;
  if (index >= entries.length) return { text: draft };
  return { text: entries[index]!, recall: { scope, entries, index, draft } };
}
