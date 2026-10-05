import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from "react";
import type { WorkPreview } from "../protocol";
import { StorageError } from "../storage-error";
import { MonoLine, type Segment } from "./MonoLine";

export interface DeleteWorkActions {
  readonly preview: () => Promise<WorkPreview>;
  readonly confirm: (token: string, additionalWork: boolean, protectedContent: boolean) => Promise<void>;
}

/** What the cell's controls strip and `Shift+D` reach: opening the review, never deleting. */
export interface DeleteWorkHandle {
  readonly review: () => void;
}

/** Commands named in the review before the rest are counted. */
const MAX_COMMANDS = 5;
/** Engine ids are UUIDs; they are the engine's vocabulary, not the user's. */
const UUID = /(?:\bcells?\s+)?\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi;
/** A shorter "id" would rewrite ordinary words; real cell ids are far longer. */
const MIN_ID_LENGTH = 8;
const UNDESCRIBED ="command without a description";

/** Engine text with every cell id — UUID-shaped or one the engine just named to us — replaced by words. */
export function publicText(text: string, known: readonly string[] = []): string {
  return known.filter(id => id.length >= MIN_ID_LENGTH)
    .reduce((rest, id) => rest.split(id).join("a command"), text.replace(UUID, "a command"));
}

const plural = (count: number, noun: string) => `${count} ${noun}${count === 1 ? "" : "s"}`;
const firstLine = (label: string | undefined) => label?.split("\n").map(line => line.trim()).find(line => line.length > 0);
const asError = (failure: unknown) => failure instanceof Error ? failure : new Error(String(failure));

/** Distinct command descriptions in order, with how many attempts share each. */
function describeCommands(preview: WorkPreview): { readonly text: string; readonly count: number }[] {
  const counts = new Map<string, number>();
  for (const cell of preview.cells) {
    const text = publicText(firstLine(preview.labels[cell]) ?? UNDESCRIBED, preview.cells);
    counts.set(text, (counts.get(text) ?? 0) + 1);
  }
  return [...counts].map(([text, count]) => ({ text, count }));
}

const said = (role: Segment["role"], text: string): Segment[] => [{ text, role }];

/** The engine owns the impact and eligibility; this question only reviews its authority. */
export const DeleteWork = forwardRef<DeleteWorkHandle, { readonly actions: DeleteWorkActions; readonly onDismiss?: () => void }>(
function DeleteWork({ actions, onDismiss }, handle) {
  const [opened, setOpened] = useState(false);
  const [preview, setPreview] = useState<WorkPreview>();
  const [error, setError] = useState<Error>();
  const [phase, setPhase] = useState<"idle" | "previewing" | "deleting" | "deleted">("idle");
  const [protectedContent, setProtectedContent] = useState(false);
  const epoch = useRef(0);
  const busy = useRef(false);
  const open = useRef(false);
  const known = useRef<readonly string[]>([]);
  const dismiss = useRef<HTMLButtonElement>(null);
  useEffect(() => () => { epoch.current++; }, []);
  // The trigger lives in the cell, so the keyboard user's next step is here: safest choice first.
  useEffect(() => { if (preview || error) dismiss.current?.focus?.(); }, [preview, error]);

  const review = async () => {
    if (busy.current) return;
    const request = ++epoch.current;
    busy.current = true; open.current = true;
    setOpened(true); setPreview(undefined); setError(undefined); setProtectedContent(false); setPhase("previewing");
    try {
      const result = await actions.preview();
      if (request === epoch.current) { known.current = result.cells; setPreview(result); }
    } catch (failure) {
      if (request === epoch.current) setError(asError(failure));
    } finally {
      if (request === epoch.current) { busy.current = false; setPhase("idle"); }
    }
  };
  // A second `Shift+D` or chip press must not restart a review whose choices are on screen.
  useImperativeHandle(handle, () => ({ review: () => { if (!open.current) void review(); } }));
  const close = () => {
    if (phase === "deleting") return;
    epoch.current++; busy.current = false; open.current = false;
    setOpened(false); setPreview(undefined); setError(undefined); setPhase("idle");
    onDismiss?.();
  };
  const confirm = async () => {
    if (!preview || busy.current || (preview.protected.length > 0 && !protectedContent)) return;
    const request = ++epoch.current;
    busy.current = true; setError(undefined); setPhase("deleting");
    // Consume the preview once, including when the reply is lost. Retrying needs fresh review.
    setPreview(undefined);
    try {
      await actions.confirm(preview.token, preview.cells.length > 1 || preview.dependents.length > 0, protectedContent);
      if (request === epoch.current) setPhase("deleted");
    } catch (failure) {
      if (request === epoch.current) { setError(asError(failure)); setPhase("idle"); }
    } finally { if (request === epoch.current) busy.current = false; }
  };

  if (!opened) return null;
  const status = phase === "previewing" ? "Checking deletion…"
    : phase === "deleting" ? "Deleting…"
    : phase === "deleted" ? "Deleted. Waiting for the workspace update." : "";
  const blockers = error instanceof StorageError ? error.blockers : [];
  const hidden = [...known.current, ...blockers.flatMap(blocker => blocker.cells)];
  const commands = preview ? describeCommands(preview) : [];
  const related = preview ? preview.cells.length - 1 : 0;
  return <div className="cell-deletion" onKeyDown={event => {
    // A question's text/buttons must never invoke the containing cell's repeat/delete keys.
    event.stopPropagation();
    if (event.key === "Escape") { event.preventDefault(); close(); }
  }}>
    <div role="status" aria-live="polite">{status && <DeletionLine segments={said("mono-dim", status)} />}</div>
    {error && <div role="alert">
      <DeletionLine segments={said("mono-bad", publicText(error.message, hidden))} />
      {blockers.map((blocker, index) => <DeletionLine key={index} segments={[
        { text: "  ", role: "mono-faint" },
        blocker.node ? { text: `$${blocker.node}`, role: "mono-ref" } : { text: "submission", role: "mono-dim" },
        { text: ` · ${blocker.state} · ${publicText(blocker.reason, hidden)}`, role: "mono-bad" },
      ]} />)}
    </div>}
    {preview && <div role="group" aria-label="Review cell deletion">
      <DeletionLine segments={said("mono-warn", related > 0
        ? `Delete this work and ${plural(related, "related attempt")}?`
        : "Delete this work from the workspace?")} />
      {related > 0 && commands.slice(0, MAX_COMMANDS).map(({ text, count }) => <DeletionLine key={text} segments={[
        { text: "  ", role: "mono-faint" }, { text },
        ...(count > 1 ? [{ text: ` ×${count}`, role: "mono-dim" } as Segment] : []),
      ]} />)}
      {related > 0 && commands.length > MAX_COMMANDS && <DeletionLine
        segments={[{ text: `  +${commands.length - MAX_COMMANDS} more`, role: "mono-dim" }]} />}
      {preview.nodes.length > 0 && <DeletionLine segments={[
        { text: "Results ", role: "mono-dim" },
        ...preview.nodes.flatMap((node, index): Segment[] => [
          ...(index > 0 ? [{ text: " " }] : []), { text: `$${node}`, role: "mono-ref" }]),
      ]} />}
      {preview.dependents.length > 0 && <DeletionLine segments={said("mono-warn",
        `Also removes ${plural(preview.dependents.length, "dependent command group")}.`)} />}
      <DeletionLine segments={said("mono-dim", "Removes its history and results kept only here. Completed external effects are not undone.")} />
      {preview.sharedWorkspaces.length > 0 && <DeletionLine segments={said("mono-dim",
        `Shared results stay for workspaces: ${preview.sharedWorkspaces.join(", ")}. Their history is unchanged.`)} />}
      {preview.protected.length > 0 && <label className="mono-warn cell-deletion-line"><input type="checkbox" checked={protectedContent}
        onChange={event => setProtectedContent(event.currentTarget.checked)} /> Also delete {plural(preview.protected.length, "protected result")} with no remaining references.</label>}
    </div>}
    <div className="cell-actions cell-deletion-actions" role="group" aria-label="Deletion choices">
      {preview && <button type="button" className="cell-action" disabled={phase !== "idle" || (preview.protected.length > 0 && !protectedContent)} onClick={() => void confirm()}>Delete work</button>}
      {error && <button type="button" className="cell-action" onClick={() => void review()}>Review again</button>}
      <button type="button" className="cell-action" ref={dismiss} disabled={phase === "deleting"} onClick={close}>{preview ? "Keep" : "Close"}</button>
    </div>
  </div>;
});

/** A mono line that wraps: a sentence of consequences must not lose its tail to an ellipsis. */
function DeletionLine({ segments }: { readonly segments: readonly Segment[] }) {
  return <MonoLine segments={segments} className="cell-deletion-line" />;
}
