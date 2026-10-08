import { incompleteResult, observationStale, stoppedStream, type WorkspaceNode } from "../workspace";

/** Private finite values are readable in the authorized memory data plane.
 * Observation of private or uncertain live sources is never implicit.
 * An incomplete analysis's partial result is readable evidence; its node stays failed. */
export function resultAccess(node:WorkspaceNode|undefined) {
  const partial=Boolean(node && !node.doubt && incompleteResult(node));
  const current=partial || Boolean(node && !node.doubt && node.state!=="failed" && node.state!=="skipped" && (node.state!=="cancelled" || stoppedStream(node)));
  const live=Boolean(node && !node.doubt && !node.private && !partial && node.streamOutput && node.failureRecord?.code!=="ENV020"
    && (node.state==="failed" || node.state==="cancelled" && stoppedStream(node) || node.state==="ready" || node.state==="running" || node.state==="pending" || observationStale(node)));
  return {current,live,partial};
}
