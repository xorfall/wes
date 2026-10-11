import { stringifyExactJson } from "../exact-json";
import type { ValueViewModule } from "./contract";
import type { SharedInteraction } from "./interactive";
import { valueViewModules } from "./registry";
import { decodeContract, protocolIdentity } from "./definition";
import type { FrameSample } from "./instances";

export interface SharedState {
  readonly owner:string; readonly identity:string; readonly definitionRevision:string; readonly revision:string;
  readonly definition:string; readonly digest:string; readonly artifact?:string|null;
  readonly fields:Readonly<Record<string,unknown>>; readonly outputs:Readonly<Record<string,unknown>>;
}
export type SharedCommit = Omit<SharedState,"definition"|"digest"> & {readonly events?:readonly {port:string;value:unknown}[]};
export interface SharedCommitResult {
  readonly state:SharedState;
  readonly outcome:"committed"|"rejected";
  readonly problem?:string;
}
export interface StateTransport {
  activate?(state: SharedState): void;
  settled?(ready:boolean):void;
  watch(listener:(sample:FrameSample<SharedState>)=>void):()=>void;
  commit(edit:SharedCommit,signal:AbortSignal):Promise<SharedCommitResult>;
}
/** Latest state is replaceable, events are not. One write and one pending snapshot per mount.
 * Conflicts adopt the owner's state without replay. Opening only initializes empty state. */
export function attachSharedState(module:ValueViewModule, controller:SharedInteraction, transport:StateTransport):()=>void {
  const definition=module.definition!, fields=definition.interaction?.sharedFields??[];
  if(!fields.length)return ()=>{};
  const signal=new AbortController();
  type Pending=Pick<SharedCommit,"fields"|"outputs"|"events"> & { activation?: boolean };
  let current:SharedState|undefined, pending:Pending|undefined, busy=false, blocked:string|undefined;
  const events:Pending[]=[];
  const report=()=>transport.settled?.(!!current && !busy && !pending && !events.length && !blocked && !signal.signal.aborted);
  report();
  let timer:ReturnType<typeof setTimeout>|undefined;
  const snapshot=(state:unknown)=>{
    const values=state as Record<string,unknown>, all=module.outputSnapshot!(state) as Record<string,unknown>;
    return {fields:Object.fromEntries(fields.map(name=>[name,values[name]])),outputs:Object.fromEntries(Object.entries(definition.outputs).filter(([,port])=>port.shared && port.mode==="state").map(([name])=>[name,all[name]]))};
  };
  const adopt=(state:SharedState)=>{
    if(!/^\d+$/.test(state.revision)||!/^\d+$/.test(state.definitionRevision))throw new Error("Invalid shared state revision");
    if(current?.identity===state.identity && (BigInt(state.revision)<BigInt(current.revision) || BigInt(state.definitionRevision)<BigInt(current.definitionRevision)))return;
    const owner=valueViewModules.named(state.definition,state.artifact)?.definition;
    if(!owner || owner.digest!==state.digest || protocolIdentity(owner)!==protocolIdentity(definition))throw new Error("Coordinator protocol changed");
    if(Object.keys(state.fields).length!==fields.length||fields.some(name=>!Object.hasOwn(state.fields,name)))throw new Error("Invalid shared state fields");
    const shape=definition.contracts[definition.interaction!.state]!.fields!;
    for(const name of fields)decodeContract(definition,shape[name]!.type,state.fields[name]);
    if(!controller.adopt(state.fields))throw new Error("Shared state rejected");
    current=state;
    if(blocked)controller.problem(blocked);
    report();
  };
  const flush=()=>{
    timer=undefined;
    if(busy||(!pending&&!events.length)||!current||signal.signal.aborted)return;
    const submitted=events.shift()??pending!;if(!events.length && submitted===pending)pending=undefined;busy=true;report();
    const { activation, ...edit } = submitted;
    void transport.commit({...current,...edit},signal.signal).then(result=>{
      if(signal.signal.aborted)return;
      if(result.outcome==="rejected"){
        const lost=!!submitted.events?.length || events.length>0;
        events.length=0;pending=undefined;
        const problem=result.problem??"Selection changed in another view; current selection restored.";
        if(result.state.revision==="0"){
          // Revision zero is an uninitialized owner, not a valid empty state.
          // Keep the local draft visible, but never retry initialization on polling.
          current=undefined;blocked=`Shared view initialization was rejected. ${problem}`;
          controller.problem(blocked);
        }else{
          adopt(result.state);
          controller.problem(`${problem}${lost?" View events were rejected and were not replayed.":""}`);
        }
      }
      else {
        if(!pending&&!events.length)adopt(result.state); else current=result.state;
        if(activation) transport.activate?.(result.state);
      }
    }).catch(()=>{
      if(signal.signal.aborted)return;
      events.length=0;pending=undefined;current=undefined;blocked="View update was not confirmed and was not replayed. Reopen the view to reconnect.";controller.problem(blocked);
    }).finally(()=>{busy=false;report();if((pending||events.length)&&!signal.signal.aborted)timer=setTimeout(flush,events.length?0:50);});
  };
  const stopIntercept=controller.intercept((previous,next,event,activation)=>{
    const emitted=module.outputEvents?.(previous,event,next)??[];
    if(!activation && !emitted.length && fields.every(name=>Object.is((previous as Record<string,unknown>)[name],(next as Record<string,unknown>)[name])))return true;
    if(blocked){controller.problem(blocked);return false;}
    if(!current){controller.problem("Connecting shared selection; try again shortly.");return false;}
    if(emitted.length || activation){
      if(events.length>=16){blocked="View event queue is full; this event was rejected. Reopen the view to reconnect.";report();controller.problem(blocked);return false;}
      const item={...snapshot(next),events:emitted,activation};
      if(new TextEncoder().encode(stringifyExactJson(item)).length>12000){controller.problem("View event exceeds its message budget; this event was rejected.");return false;}
      events.push(item);pending=undefined;
    }else pending=snapshot(next);
    report();
    if(!busy&&timer===undefined)timer=setTimeout(flush,emitted.length?0:50);return true;
  });
  const stopWatch=transport.watch(sample=>{
    if(signal.signal.aborted||busy||pending||events.length)return;
    if(sample.problem){current=undefined;report();controller.problem(sample.problem);return;}
    if(!sample.frame)return;
    if(blocked){controller.problem(blocked);return;}
    try {
      if(sample.frame.revision==="0"){
        current=sample.frame;pending=snapshot(controller.committed());flush();
      } else adopt(sample.frame);
    } catch { current=undefined;report();controller.problem("Shared view state does not match its contract."); }
  });
  return ()=>{signal.abort();clearTimeout(timer);events.length=0;pending=undefined;stopWatch();stopIntercept();};
}
