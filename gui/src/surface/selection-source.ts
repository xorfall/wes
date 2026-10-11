import type {WorkspaceNode} from "../workspace";
import type {ResultObservation} from "./results";

/** Activation names the displayed publication, never a newer run still producing its result. */
export function selectionSource(node:WorkspaceNode|undefined, observation?:ResultObservation):{node:string;run:string}|undefined {
  if(!node || node.private || node.doubt || node.accessWithdrawn || node.evidence || node.streamSource)return undefined;
  const run=observation ? observation.run : node.publication?.state==="available" && node.publication.handle===node.handle ? node.publication.run : node.state==="ready" ? node.run : undefined;
  return run ? {node:node.id,run} : undefined;
}
