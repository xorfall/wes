import type { WorkspaceNode } from "./workspace";
import type { Event } from "./protocol";

/** UTF-16 storage units: at most 512 KiB of text per node and 16 MiB across this client workspace. */
export const NODE_TRANSCRIPT = 256 * 1024;
export const WORKSPACE_TRANSCRIPTS = 8 * 1024 * 1024;
function tail(text: string, units: number): string {
  let from = Math.max(0, text.length - units);
  if (from > 0 && from < text.length && text.charCodeAt(from) >= 0xdc00 && text.charCodeAt(from) <= 0xdfff
      && text.charCodeAt(from - 1) >= 0xd800 && text.charCodeAt(from - 1) <= 0xdbff) from++;
  return text.slice(from);
}
export function appendTranscript(nodes: readonly WorkspaceNode[], event: Extract<Event, { event: "output" }>): readonly WorkspaceNode[] {
  const target = nodes.findIndex(node => node.id === event.node && node.run === event.run && node.interactive);
  if (target < 0) return nodes;
  const updated = [...nodes];
  const node = nodes[target];
  if (!node) return nodes;
  const text = (node.wrote ?? "") + event.text;
  const wrote = tail(text, NODE_TRANSCRIPT);
  updated[target] = { ...node, wrote, outputLost: node.outputLost || event.omittedBytes !== "0",
    wroteTrimmed: node.wroteTrimmed || wrote.length < text.length };
  let excess = updated.reduce((total, node) => total + (node.wrote?.length ?? 0), 0) - WORKSPACE_TRANSCRIPTS;
  // Prefer the currently arriving conversation. Older transcripts can be fetched through the final
  // result while it exists; browser display does not silently accumulate unbounded historical text.
  for (let i = 0; i < updated.length && excess > 0; i++) {
    const candidate = updated[i];
    if (i === target || !candidate?.wrote) continue;
    const previous = candidate.wrote;
    const remaining = tail(previous, Math.max(0, previous.length - excess));
    excess -= previous.length - remaining.length;
    updated[i] = { ...candidate, wrote: remaining, wroteTrimmed: true };
  }
  return updated;
}
