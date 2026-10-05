import { observationStale, type WorkspaceNode } from "../workspace";

/** Private finite values are readable in the authorized memory data plane.
 * Observation of private or uncertain live sources is never implicit. */
export function resultAccess(node:WorkspaceNode|undefined) {
  const current=Boolean(node && !node.doubt && node.state!=="failed" && node.state!=="skipped" && (node.state!=="cancelled" || node.stopped));
  const live=Boolean(node && !node.doubt && !node.private && node.streamOutput && node.failureRecord?.code!=="ENV020"
    && (node.state==="failed" || node.state==="cancelled" && node.stopped || node.state==="ready" || node.state==="running" || node.state==="pending" || observationStale(node)));
  return {current,live};
}
