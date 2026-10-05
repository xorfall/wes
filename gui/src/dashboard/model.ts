import { budget } from "../limits/policy";
import type { ViewTier } from '../../../packages/view-sdk/contract';

export type WidthPolicy = 'auto' | 'fill' | 'preferred';
export type Alignment = 'auto' | 'start' | 'center' | 'end';
export interface DashboardMember {
  readonly id: string;
  readonly node: string;
  readonly generation: string;
  readonly label: string;
}
export type DashboardNode = {
  readonly kind: 'member'; readonly id: string; readonly member: string;
  readonly width: WidthPolicy; readonly align: Alignment; readonly weight: number;
} | {
  readonly kind: 'row' | 'column'; readonly id: string;
  readonly children: readonly DashboardNode[];
  /** Comfortable width for adjacent groups, in display columns; not a hard minimum. */
  readonly basis?: number;
  readonly weight: number;
};
export interface Dashboard {
  readonly version: 1; readonly id: string; readonly name: string; readonly title: string;
  readonly revision: number; readonly members: readonly DashboardMember[]; readonly layout: DashboardNode;
}
export interface DashboardSource extends DashboardMember {
  readonly sizing: ViewTier;
  readonly width: 'fill' | 'preferred';
  readonly align: Exclude<Alignment,'auto'>;
  /** Live publications cannot change their allocated viewport height. */
  readonly live?: boolean;
}
export function max_dashboard_members():number { return budget("ui.dashboard.members"); }
export function max_dashboards():number { return budget("ui.dashboards"); }
export function max_dashboard_bytes():number { return budget("ui.dashboard.bytes"); }

const record = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const plain = (value: unknown, max = 512): value is string => typeof value === 'string' && value.length > 0 && value.length <= max && !/[\u0000-\u001f\u007f]/.test(value);
export const dashboardName = (value: unknown): value is string => plain(value, 96) && !/[\s$/\\]/.test(value);
const keys = (value: Record<string, unknown>, allowed: readonly string[]) => Object.keys(value).every(key => allowed.includes(key));
/** Persisted/imported layouts grant no authority. Every reference is checked again by the host. */
export function readDashboard(value: unknown): Dashboard | undefined {
  if (!record(value) || !keys(value, ['version', 'id', 'name', 'title', 'revision', 'members', 'layout']) || value.version !== 1 || !plain(value.id)
    || !dashboardName(value.name) || !plain(value.title, 2048) || !Number.isSafeInteger(value.revision) || Number(value.revision) < 0
    || !Array.isArray(value.members) || value.members.length > max_dashboard_members()) return;
  const members: DashboardMember[] = [], seen = new Set<string>(), refs = new Set<string>();
  for (const item of value.members) {
    if (!record(item) || !keys(item, ['id', 'node', 'generation', 'label']) || !plain(item.id) || !plain(item.node) || !plain(item.generation) || !plain(item.label)) return;
    const reference = JSON.stringify([item.node, item.generation]);
    if (seen.has(item.id) || refs.has(reference)) return;
    seen.add(item.id); refs.add(reference); members.push({id: item.id, node: item.node, generation: item.generation, label: item.label});
  }
  const nodes = new Set<string>(), leaves = new Set<string>();
  let count = 0;
  function node(raw: unknown, depth: number): DashboardNode | undefined {
    if (++count > budget("ui.dashboard.nodes") || depth > 8 || !record(raw) || !plain(raw.id) || nodes.has(raw.id)
      || !Number.isInteger(raw.weight) || Number(raw.weight) < 1 || Number(raw.weight) > 16) return;
    nodes.add(raw.id);
    if (raw.kind === 'member') {
      if (!keys(raw, ['kind', 'id', 'member', 'width', 'align', 'weight']) || !plain(raw.member) || !seen.has(raw.member) || leaves.has(raw.member)
        || !['auto','fill','preferred'].includes(String(raw.width)) || !['auto','start','center','end'].includes(String(raw.align))) return;
      leaves.add(raw.member);
      return {kind:'member',id:raw.id,member:raw.member,width:raw.width as WidthPolicy,align:raw.align as Alignment,weight:Number(raw.weight)};
    }
    if (!keys(raw, ['kind','id','children','basis','weight']) || !['row','column'].includes(String(raw.kind)) || !Array.isArray(raw.children) || raw.children.length > max_dashboard_members()
      || raw.basis !== undefined && (!Number.isInteger(raw.basis) || Number(raw.basis) < 1 || Number(raw.basis) > 512)) return;
    const children: DashboardNode[] = [];
    for (const child of raw.children) {const parsed = node(child, depth + 1); if (!parsed) return; children.push(parsed);}
    return {kind:raw.kind as 'row'|'column',id:raw.id,children,weight:Number(raw.weight),...(raw.basis === undefined ? {} : {basis:Number(raw.basis)})};
  }
  const layout = node(value.layout, 0);
  if (!layout || layout.kind === 'member' || leaves.size !== members.length) return;
  const result:Dashboard={version:1,id:value.id,name:value.name,title:value.title,revision:Number(value.revision),members,layout};
  if(new TextEncoder().encode(JSON.stringify(result,null,2)).length>max_dashboard_bytes())return;
  return result;
}
export function newDashboard(): Dashboard {
  return {version:1,id:crypto.randomUUID(),name:'board',title:'Dashboard',revision:0,members:[],layout:{kind:'column',id:crypto.randomUUID(),weight:1,children:[]}};
}

export function dashboardNodes(node: DashboardNode): readonly DashboardNode[] {
  return [node, ...(node.kind === 'member' ? [] : node.children.flatMap(dashboardNodes))];
}
export function changeDashboardNode(root: DashboardNode, id: string, change: (node: DashboardNode) => DashboardNode): DashboardNode {
  if (root.id === id) return change(root);
  return root.kind === 'member' ? root : { ...root, children: root.children.map(child => changeDashboardNode(child, id, change)) };
}
export function removeDashboardMember(board: Dashboard, member: string): Dashboard {
  const remove = (node: DashboardNode): DashboardNode | undefined => node.kind === 'member' ? (node.member === member ? undefined : node)
    : { ...node, children: node.children.flatMap(child => { const next = remove(child); return next ? [next] : []; }) };
  return { ...board, members: board.members.filter(item => item.id !== member), layout: remove(board.layout) ?? { kind: 'column', id: board.layout.id, children: [], weight: 1 } };
}
export function addDashboardMember(board: Dashboard, member: DashboardMember, target = board.layout.id): Dashboard {
  if (!dashboardNodes(board.layout).some(node=>node.id===target) || board.members.length >= max_dashboard_members() || board.members.some(item => item.id===member.id || item.node === member.node && item.generation === member.generation)) return board;
  const leaf: DashboardNode = { kind: 'member', id: crypto.randomUUID(), member: member.id, width: 'auto', align: 'auto', weight: 1 };
  const layout = changeDashboardNode(board.layout, target, node => node.kind === 'member'
    ? { kind: 'column', id: crypto.randomUUID(), children: [node, leaf], weight: node.weight }
    : { ...node, children: [...node.children, leaf] });
  return { ...board, members: [...board.members, member], layout };
}
export function moveDashboardNode(root: DashboardNode, id: string, delta: -1 | 1): DashboardNode {
  if (root.kind === 'member') return root;
  const index = root.children.findIndex(child => child.id === id), target = index + delta;
  if (index >= 0 && target >= 0 && target < root.children.length) {
    const children = [...root.children]; [children[index], children[target]] = [children[target]!, children[index]!];
    return { ...root, children };
  }
  return { ...root, children: root.children.map(child => moveDashboardNode(child, id, delta)) };
}
