/**
 * The local reconciliation the engine says this node's original run accepts, as one action shared by
 * the cell (whatever its size, with or without a readable value), `/open` and the inspector.
 *
 * Reconciliation checks the local store's own writes once the original run has joined. It never
 * reruns the source, resumes analysis or turns the run successful, so no source Start/Cancel is ever
 * offered here. The action only writes the exact command into the session prompt for review; opening,
 * drawing or reading never submits anything. The command names the node by its id and the exact run
 * the engine stated the control for; the engine checks ownership, scope and that run again when it is
 * submitted. Only the engine's typed control is read: nothing is inferred from the command text,
 * progress, the result's shape or a copy of it.
 */
import { useContext } from "react";
import type { ReconciliationControl } from "../protocol";
import type { WorkspaceNode } from "../workspace";
import { CANONICAL_RUN, ComposeContext, freshName, REFERABLE_NODE } from "./dataset-management";
import { MonoLine } from "./MonoLine";
import "./dataset-management.css";

export interface LocalReconciliation {
  readonly command: ReconciliationControl["command"];
  readonly available: boolean;
  readonly run: string;
}

/** What may be offered for this node now, for the exact run the engine stated it for, or nothing. */
export function localReconciliation(node: WorkspaceNode | undefined): LocalReconciliation | undefined {
  const control = node?.reconciliationControl;
  if (!node || !control || node.run === undefined || control.run !== node.run || node.accessWithdrawn) return undefined;
  if (!REFERABLE_NODE.test(node.id) || !CANONICAL_RUN.test(control.run)) return undefined;
  // The node's own state can only withhold: a run seen running or waiting has not joined, whatever was said earlier.
  const joined = node.state !== "running" && node.state !== "pending";
  return { command: control.command, available: control.available && joined, run: control.run };
}

/* The run is always carried: the engine refuses the command once it no longer names the original run. */
export const reconcileCommand = (command: ReconciliationControl["command"], node: string, run: string, name: string) =>
  `:${command} reconcile $${node} run:"${run}" > ${name}`;

export const RECONCILE_NOTE = "Checks local disk writes; does not rerun the source or resume analysis. Nothing runs until you submit.";
export const RECONCILE_WAITING = "Offered once this run has fully stopped; it checks local disk writes only.";
const UNHOSTED = "local write reconciliation is prepared in the session";

export function LocalReconciliationControls({ node }: { readonly node: WorkspaceNode | undefined }) {
  const composer = useContext(ComposeContext);
  const control = localReconciliation(node);
  if (!node || !control) return null;
  const receipt = composer && control.available ? freshName("receipt", composer.taken) : undefined;
  const note = !composer ? UNHOSTED : control.available ? RECONCILE_NOTE : RECONCILE_WAITING;
  return <div className="management-actions reconciliation-controls" role="group" aria-label="Local write reconciliation">
    {composer && <button type="button" className="cell-action" disabled={!receipt} aria-description={note}
      onClick={() => receipt && composer.compose(reconcileCommand(control.command, node.id, control.run, receipt))}>reconcile local write…</button>}
    <MonoLine segments={[{ text: note, role: "mono-faint" }]} className="value-line" description={note} />
  </div>;
}
