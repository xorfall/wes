import { max_dashboards, readDashboard, type Dashboard } from './model';
import { workspaceName } from '../workspace-binding';
export interface SavedDashboard {readonly workspace:string;readonly board:Dashboard}
/** Layouts are personal UI state. Labels never resolve imported references to new nodes. */
export function restoreDashboards(value:unknown):readonly SavedDashboard[] {
  if(!Array.isArray(value)||value.length>max_dashboards())return [];
  const seen=new Set<string>(),ids=new Set<string>(),result:SavedDashboard[]=[];
  for(const entry of value) {
    if(!entry||typeof entry!=='object'||Array.isArray(entry)||Object.keys(entry).some(key=>!['workspace','board'].includes(key))||!workspaceName(entry.workspace))return [];
    const board=readDashboard(entry.board),key=JSON.stringify([entry.workspace,board?.name]);
    if(!board||seen.has(key)||ids.has(board.id))return [];
    seen.add(key);ids.add(board.id);result.push({workspace:entry.workspace,board});
  }
  return result;
}
export function saveDashboard(saved:readonly SavedDashboard[],workspace:string,raw:Dashboard,nodeNames:readonly string[]):readonly SavedDashboard[] {
  const board=readDashboard(raw);if(!board||!workspaceName(workspace))throw new Error('Invalid dashboard layout. Check its name, members and grouping.');
  const previous=saved.find(entry=>entry.board.id===board.id);
  if(previous && (previous.workspace!==workspace||previous.board.revision!==board.revision))throw new Error('This dashboard changed while editing. Reopen its editor before saving.');
  if(nodeNames.includes(board.name)||saved.some(entry=>entry.workspace===workspace&&entry.board.id!==board.id&&entry.board.name===board.name))throw new Error(`$${board.name} already exists. Choose another dashboard name.`);
  if(!previous && saved.length>=max_dashboards())throw new Error('The saved dashboard budget is full. Remove a dashboard before creating another.');
  return [...saved.filter(entry=>entry.board.id!==board.id),{workspace,board:{...board,revision:board.revision+1}}];
}
export function exportDashboard(board:Dashboard):string {return JSON.stringify(board,null,2);}
/** An import is a new draft. Explicit source replacements are required across sessions. */
export function importDashboard(text:string):Dashboard {
  if(new TextEncoder().encode(text).length>128*1024)throw new Error('Dashboard definition exceeds its 128 KiB import limit.');
  let parsed:unknown;try{parsed=JSON.parse(text);}catch{throw new Error('Invalid dashboard JSON.');}
  const board=readDashboard(parsed);if(!board)throw new Error('Invalid dashboard definition.');
  return {...board,id:crypto.randomUUID(),revision:0};
}
