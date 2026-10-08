import { useContext } from "react";
import type { StoredValue } from "../protocol";
import type { WorkspaceNode } from "../workspace";
import { ComposeContext, freshName, REFERABLE_NODE, resumeCommand } from "./dataset-management";
import { lineText, MonoLine } from "./MonoLine";
import { receiptHeadline, receiptRows, scanReceiptOf, type ScanReceipt } from "./scan-receipt";
import { continuationPreviewOf, reviewCommand } from "./scan-continuation";
import { ScanContinuationDetails } from "./ScanContinuationDetails";
import { lifetimeActive } from "./record-progress";
import "./record-progress.css";
import "./dataset-management.css";

/**
 * The analysis receipt — or the engine's continuation review — under its own disclosure, beside and
 * never instead of the generic value view. One host serves `/open` details and the Inspector, so both
 * draw the same facts and offer the same actions. Rows wrap here: this is where they are readable whole.
 * A withdrawn node draws nothing, whatever value is still passed in.
 */
export function ScanReceiptDetails({ value, open = false, node }: { readonly value: StoredValue | undefined; readonly open?: boolean; readonly node?: WorkspaceNode }) {
  if (node?.accessWithdrawn) return null;
  const receipt = scanReceiptOf(value);
  if (!receipt) {
    const preview = continuationPreviewOf(value);
    return preview ? <ScanContinuationDetails preview={preview} open={open} {...(node ? { node } : {})} /> : null;
  }
  const headline = receiptHeadline(receipt);
  return (
    <details className="scan-receipt" open={open}>
      <summary aria-label={lineText(headline)}><MonoLine segments={headline} className="scan-receipt-headline" /></summary>
      {receiptRows(receipt).map((row, at) => <MonoLine key={at} segments={row} className="scan-receipt-row" />)}
      <ResumeAction receipt={receipt} {...(node ? { node } : {})} />
      <ReviewAction receipt={receipt} {...(node ? { node } : {})} />
    </details>
  );
}

/**
 * Why this analysis cannot be resumed, or nothing when its own receipt permits it. Only the
 * engine's `durableResume` decides, and it is deliberately conservative: it offers resuming only a
 * cancelled analysis with a durable checkpoint, no applied finish, and headroom in duration and every
 * cumulative total of its latest granted bounds. A stopped analysis keeps its checkpoint but is not offered:
 * resuming the same code would meet the same stop. Status and attempt only choose the reason text,
 * never grant. A running node cannot be resumed until it stops.
 */
export function resumeRefusal(receipt: ScanReceipt, node: WorkspaceNode | undefined): string | undefined {
  if (!receipt.durableResume) return receipt.status === "complete" ? "complete · nothing to resume"
    : receipt.attempt === undefined ? "no durable checkpoint · nothing to resume from"
    : receipt.status === "stopped" ? "stopped · checkpoint kept, but only a cancelled analysis is offered resuming"
    : "this receipt does not permit resuming";
  // A ready analysis whose run the engine still calls open (a followed scan) is still running too.
  if (node?.state === "running" || node?.state === "pending" || lifetimeActive(node)) return "the analysis is still running";
  if (!node?.name) return "resuming refers to the analysis by name; this result has none";
  return undefined;
}

/**
 * Why a continuation review cannot be prepared for this analysis, or nothing. A review is read-only,
 * so it is offered for any durable checkpoint of a settled analysis; whether continuing is possible is
 * the review's own answer, never guessed here from the receipt's counters.
 */
export function reviewRefusal(receipt: ScanReceipt, node: WorkspaceNode | undefined): string | undefined {
  if (receipt.attempt === undefined) return "no durable checkpoint · nothing to continue from";
  if (receipt.status === "complete") return "complete · nothing to continue";
  if (node?.state === "running" || node?.state === "pending" || lifetimeActive(node)) return "the analysis is still running";
  if (!node || !REFERABLE_NODE.test(node.id)) return "the analysis has no node id a command can name";
  return undefined;
}

/**
 * Resume this analysis from its own checkpoint, in a new cell and under its latest granted bounds —
 * those of its first attempt only until a continuation raises them. The command is written into the
 * prompt; the original cell and its source are never refreshed.
 */
function ResumeAction({ receipt, node }: { readonly receipt: ScanReceipt; readonly node?: WorkspaceNode }) {
  const composer = useContext(ComposeContext);
  const refusal = resumeRefusal(receipt, node);
  const target = composer ? freshName("continuation", composer.taken) : undefined;
  const why = refusal ?? (!composer ? "resuming is prepared in the session" : !target ? "no unused result name is available" : undefined);
  return <div className="management-actions" role="group" aria-label="Analysis resume">
    <button type="button" className="cell-action" disabled={why !== undefined}
      onClick={() => { if (!why && composer && target && node?.name) composer.compose(resumeCommand(`$${node.name}`, target)); }}>resume analysis…</button>
    <MonoLine segments={[{ text: why ?? "a new cell resumes from the checkpoint under its latest granted bounds; nothing runs until you submit it", role: "mono-faint" }]} className="value-line" />
  </div>;
}

/**
 * Prepare a read-only review of continuing this analysis with new bounds. Distinct from resuming:
 * the review reads the checkpoint and the current limits, and reserves, reconciles and runs nothing.
 * The analysis is named by its node id, the identity the engine checks ownership against.
 */
function ReviewAction({ receipt, node }: { readonly receipt: ScanReceipt; readonly node?: WorkspaceNode }) {
  const composer = useContext(ComposeContext);
  const refusal = reviewRefusal(receipt, node);
  const target = composer ? freshName("bounds", composer.taken) : undefined;
  const command = !refusal && node && target ? reviewCommand(node.id, target) : undefined;
  const why = refusal ?? (!composer ? "reviewing continuation is prepared in the session" : !command ? "no unused result name is available" : undefined);
  return <div className="management-actions" role="group" aria-label="Analysis continuation review">
    <button type="button" className="cell-action" disabled={why !== undefined}
      onClick={() => { if (!why && composer && command) composer.compose(command); }}>Review continuation…</button>
    <MonoLine segments={[{ text: why ?? "reads the checkpoint and current limits with new bounds; nothing is reserved or run, and nothing runs until you submit it", role: "mono-faint" }]} className="value-line" />
  </div>;
}
