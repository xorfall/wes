import type {ValueViewModule} from "./contract";
import type {SharedInteraction} from "./interactive";
import {protocolIdentity} from "./definition";
import {stringifyExactJson} from "../exact-json";

/** Ephemeral fields belong to one rendered coordinator, never a workspace-wide bus.
 * Committed fields and events retain their engine transport and revision checks. */
export class LocalCoordinators {
  private groups=new Map<string,{protocol:string;fields:Record<string,unknown>;signature:string;members:Set<SharedInteraction>}>();
  attach(owner:string,module:ValueViewModule,controller:SharedInteraction):()=>void {
    const definition=module.definition!,interaction=definition.interaction;
    if(!interaction)return ()=>{};
    const protocol=protocolIdentity(definition);
    const names=Object.keys(definition.contracts[interaction.state]!.fields!).filter(name=>!interaction.sharedFields.includes(name));
    const read=()=>Object.fromEntries(names.map(name=>[name,(controller.committed() as Record<string,unknown>)[name]]));
    let group=this.groups.get(owner);
    if(!group){const fields=read();group={protocol,fields,signature:stringifyExactJson(fields),members:new Set()};this.groups.set(owner,group);}
    if(group.protocol!==protocol)throw new Error("Coordinator protocol mismatch");
    const selected=group;
    controller.adopt(selected.fields);
    selected.members.add(controller);
    const stop=controller.subscribe(()=>{
      const next=read();
      const signature=stringifyExactJson(next);
      if(selected.signature===signature)return;
      selected.signature=signature;
      selected.fields=next;
      for(const other of selected.members)if(other!==controller)other.adopt(next);
    });
    return ()=>{stop();selected.members.delete(controller);if(!selected.members.size)this.groups.delete(owner);};
  }
}
