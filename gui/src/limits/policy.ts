import catalogue from '../../../packages/budgets/catalog.json';
import type {LimitEntry} from './model';
export interface BudgetSnapshot {readonly revision:number;readonly entries:readonly LimitEntry[];readonly scope:'application';}
const definitions=new Map(catalogue.map(entry=>[entry.id,entry]));
let active:ReadonlyMap<string,number>=new Map();
/** The startup snapshot is immutable. Saving/reloading Settings never reconfigures live readers. */
export function budget(id:string):number {
  const definition=definitions.get(id);if(!definition)throw new Error(`Unknown operating budget: ${id}`);
  return active.get(id)??definition.default;
}
export function decodeBudgets(value:unknown):BudgetSnapshot {
  if(!value||typeof value!=='object')throw new Error('Invalid operating budget response.');
  const raw=value as Partial<BudgetSnapshot>;
  if(!Number.isSafeInteger(raw.revision)||raw.revision!<0||raw.scope!=='application'||!Array.isArray(raw.entries)||raw.entries.length!==catalogue.length)throw new Error('Invalid operating budget response.');
  const seen=new Set<string>();
  for(const entry of raw.entries) {
    const definition=definitions.get(entry?.id);
    if(!definition||seen.has(entry.id)||entry.default!==definition.default||entry.min!==definition.min||entry.max!==definition.max||entry.unit!==definition.unit
      ||typeof entry.label!=='string'||typeof entry.description!=='string'||typeof entry.group!=='string'||entry.editable!==true||entry.restart!==true
      ||![entry.active,entry.saved].every(value=>Number.isSafeInteger(value)&&value>=definition.min&&value<=definition.max))throw new Error('Invalid operating budget response.');
    seen.add(entry.id);
  }
  return raw as BudgetSnapshot;
}
const timeout=()=>AbortSignal.timeout(5000);
/** What a host without the settings service (the CLI, `wes --serve`) means for the reader. */
export const UNSUPPORTED_HOST_NOTICE='Operating budget settings require the WesDesk desktop app. The CLI and wes --serve use their startup/default budgets and cannot apply changes here.';
const NOT_IMPLEMENTED=501;
export async function readBudgets(signal?:AbortSignal):Promise<BudgetSnapshot> {
  const response=await fetch('/operating-budgets',{cache:'no-store',signal:signal?AbortSignal.any([signal,timeout()]):timeout()});
  if(!response.ok)throw new Error(response.status===NOT_IMPLEMENTED?UNSUPPORTED_HOST_NOTICE:`Could not load operating budgets (HTTP ${response.status}).`);
  const text=await response.text();if(text.length>128*1024)throw new Error('Operating budget response is too large.');
  return decodeBudgets(JSON.parse(text));
}
export async function initializeBudgets():Promise<void> {
  const response=await fetch('/operating-budgets',{cache:'no-store',signal:timeout()});
  // A standalone design/embedded host may intentionally omit this service.
  if(response.status===404||response.status===501)return;
  if(!response.ok)throw new Error('Could not load the startup operating budget policy.');
  const text=await response.text();if(text.length>128*1024)throw new Error('Operating budget response is too large.');
  const snapshot=decodeBudgets(JSON.parse(text));active=new Map(snapshot.entries.map(entry=>[entry.id,entry.active]));
}
export async function saveBudgets(revision:number,values:Readonly<Record<string,number>>):Promise<BudgetSnapshot> {
  for(const [id,value]of Object.entries(values)) {
    const definition=definitions.get(id);if(!definition||!Number.isSafeInteger(value)||value<definition.min||value>definition.max)throw new Error('Invalid operating budget value.');
  }
  const response=await fetch('/operating-budgets',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({revision,values}),signal:timeout()});
  if(!response.ok)throw new Error((await response.text()).slice(0,512)||`Could not save budgets (HTTP ${response.status}).`);
  const text=await response.text();if(text.length>128*1024)throw new Error('Operating budget response is too large.');
  return decodeBudgets(JSON.parse(text));
}
