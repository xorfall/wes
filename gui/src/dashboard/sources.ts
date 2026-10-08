import type { StoredValue } from '../protocol';
import type { WorkspaceNode } from '../workspace';
import { valueViewModules,viewsOfValue } from '../value-views/registry';
import { layoutTier } from '../value-views/layout';
import { preparedValues } from '../surface/render/ValueBlock';
import type { DashboardMember, DashboardSource } from './model';
export function readableDashboardNode(node:WorkspaceNode|undefined):node is WorkspaceNode {
  return !!node && !node.private && !node.evidence && !node.doubt
    && (!!node.handle || node.publication?.state==='pending') && node.publication?.state!=='unavailable';
}
export function dashboardSource(node:WorkspaceNode,generation:string,value?:StoredValue):DashboardSource {
  let definition;
  if(value) {
    if(value.type.kind==='meta'&&value.type.name==='ViewInstance'&&value.data&&typeof value.data==='object'&&'definition' in value.data) {
      // Management values carry the contract name and digest, not the frame's package id.
      const metadata=value.data as {definition:unknown;digest?:unknown};
      definition=(viewsOfValue(value)??valueViewModules.get()).find(module=>module.definition && module.definition.name===metadata.definition && module.definition.digest===metadata.digest)?.definition;
    }
    else {const prepared=preparedValues.read(`dashboard:${generation}:${node.id}`,value);definition=valueViewModules.find(prepared.type,prepared.data,undefined,prepared.viewModules)?.definition;}
  }
  const sizing=definition?layoutTier(definition.layout,'expanded'):{min:{columns:24,rows:1},preferred:{columns:48,rows:12},max:{columns:320,rows:40}};
  return {id:JSON.stringify([generation,node.id]),node:node.id,generation,label:`$${node.name??node.id}`,sizing,width:sizing.placement?.width??'fill',align:sizing.placement?.align??'start',live:node.streamOutput};
}
export function memberProblem(member:DashboardMember,generation:string|undefined,node:WorkspaceNode|undefined):string|undefined {
  if(member.generation!==generation)return 'This result belongs to a previous workspace session. Replace it with a current result in the editor.';
  if(!node)return 'This result no longer exists. Its source command was not rerun.';
  if(node.private)return 'This result is private. Inspect it in the workspace.';
  if(node.doubt)return 'The outcome of this command is unconfirmed. Inspect it in the workspace.';
  if(node.evidence?.kind==='stopped_stream')return 'This source was stopped. Its command was not restarted.';
  if(node.evidence?.kind==='incomplete')return 'This analysis stopped before completing. Inspect its partial result in the workspace.';
  // Publishing replaces the readable handle, not the public source reference. The
  // shared observation layer can retain its last display until the next commit.
  if((!node.handle&&node.publication?.state!=='pending')||node.publication?.state==='unavailable')return node.failure??'No readable result is available. Its source command was not rerun.';
}
