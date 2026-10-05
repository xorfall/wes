import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";

export function displayIdentity(value:StoredValue,generation:string|undefined):string {
  const data=value.data;
  if(typeof data!=="object"||data===null||!("id" in data)||!("instance" in data))return JSON.stringify([generation,data]);
  return JSON.stringify([generation,data.id,data.instance]);
}

export interface DisplaySummary { readonly definition?:string; readonly members?:number }
export interface DisplaySnapshot extends DisplaySummary { readonly owner?:symbol; readonly label?:string }
interface Participant { eligible:boolean; label:string; target:()=>HTMLElement|null; changed:(snapshot:DisplaySnapshot)=>void }
interface Entry { participants:Map<symbol,Participant>; snapshot:DisplaySnapshot }
const EMPTY:DisplaySnapshot={};

/** Transcript placement is shared by identity, independently of backend lease ownership. */
export class ViewDisplays {
  private readonly entries=new Map<string,Entry>();
  join(key:string,slot:symbol,participant:Participant):()=>void {
    let entry=this.entries.get(key);
    if(!entry){entry={participants:new Map(),snapshot:EMPTY};this.entries.set(key,entry);}
    entry.participants.set(slot,participant);this.choose(entry);participant.changed(entry.snapshot);
    return ()=>{
      entry.participants.delete(slot);
      if(!entry.participants.size)this.entries.delete(key);
      else this.choose(entry);
    };
  }
  configure(key:string,slot:symbol,eligible:boolean,label:string){
    const entry=this.entries.get(key),participant=entry?.participants.get(slot);
    if(!entry||!participant)return;
    participant.eligible=eligible;participant.label=label;this.choose(entry);
  }
  describe(key:string,slot:symbol,summary:DisplaySummary){
    const entry=this.entries.get(key);
    if(!entry||entry.snapshot.owner!==slot)return;
    this.publish(entry,{...entry.snapshot,...summary});
  }
  navigate(key:string):boolean {
    const entry=this.entries.get(key),owner=entry?.snapshot.owner;
    const target=owner&&entry?.participants.get(owner)?.target();
    if(!target)return false;
    target.scrollIntoView?.({block:"center",inline:"nearest"});target.focus?.({preventScroll:true});return true;
  }
  private choose(entry:Entry){
    let owner=entry.snapshot.owner;
    if(!owner||!entry.participants.get(owner)?.eligible)owner=[...entry.participants].find(([,p])=>p.eligible)?.[0];
    this.publish(entry,{...entry.snapshot,owner,label:owner?entry.participants.get(owner)!.label:undefined});
  }
  private publish(entry:Entry,next:DisplaySnapshot){
    const old=entry.snapshot;
    if(old.owner===next.owner&&old.label===next.label&&old.definition===next.definition&&old.members===next.members)return;
    entry.snapshot=next;for(const participant of entry.participants.values())participant.changed(next);
  }
}
const engines=new WeakMap<Engine,ViewDisplays>();
export function viewDisplays(engine:Engine):ViewDisplays {
  let displays=engines.get(engine);if(!displays){displays=new ViewDisplays();engines.set(engine,displays);}return displays;
}
