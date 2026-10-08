import { useContext } from "react";
import type { StoredValue } from "../protocol";
import type { WorkspaceNode } from "../workspace";
import { ComposeContext, freshName, resumeCommand } from "./dataset-management";
import { lineText, MonoLine } from "./MonoLine";
import { receiptHeadline, receiptRows, scanReceiptOf, type ScanReceipt } from "./scan-receipt";
import { lifetimeActive } from "./record-progress";
import "./record-progress.css";
import "./dataset-management.css";

/**
 * The analysis receipt under its own disclosure, beside — never instead of — the generic value view.
 * Rows wrap here: this is the place where everything the receipt says is readable whole.
 */
export function ScanReceiptDetails({ value, open = false, node }: { readonly value: StoredValue | undefined; readonly open?: boolean; readonly node?: WorkspaceNode }) {
  const receipt = scanReceiptOf(value);
  if (!receipt) return null;
  const headline = receiptHeadline(receipt);
  return (
    <details className="scan-receipt" open={open}>
      <summary aria-label={lineText(headline)}><MonoLine segments={headline} className="scan-receipt-headline" /></summary>
      {receiptRows(receipt).map((row, at) => <MonoLine key={at} segments={row} className="scan-receipt-row" />)}
      <ResumeAction receipt={receipt} {...(node ? { node } : {})} />
    </details>
  );
}

/**
 * Why this analysis cannot be resumed, or nothing when its own receipt permits it. Only the
 * engine's `durableResume` decides, and it is deliberately conservative: it offers resuming only a
 * cancelled analysis with a durable checkpoint, no applied finish, and headroom in duration and
 * every captured cumulative bound. A stopped analysis keeps its checkpoint but is not offered:
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
 * Resume this analysis from its own checkpoint, in a new cell and under its original bounds. The
 * command is written into the prompt; the original cell and its source are never refreshed.
 */
function ResumeAction({ receipt, node }: { readonly receipt: ScanReceipt; readonly node?: WorkspaceNode }) {
  const composer = useContext(ComposeContext);
  const refusal = resumeRefusal(receipt, node);
  const target = composer ? freshName("continuation", composer.taken) : undefined;
  const why = refusal ?? (!composer ? "resuming is prepared in the session" : !target ? "no unused result name is available" : undefined);
  return <div className="management-actions" role="group" aria-label="Analysis resume">
    <button type="button" className="cell-action" disabled={why !== undefined}
      onClick={() => { if (!why && composer && target && node?.name) composer.compose(resumeCommand(`$${node.name}`, target)); }}>resume analysis…</button>
    <MonoLine segments={[{ text: why ?? "a new cell resumes from the checkpoint under the original bounds; nothing runs until you submit it", role: "mono-faint" }]} className="value-line" />
  </div>;
}
