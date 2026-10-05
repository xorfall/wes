import { max_active_view_roots } from "./limits";

interface Mount {
  node:string; identity:string; generation:string; users:number;
  active:boolean; finished:boolean; closed:boolean; token?:string;
  timer?:ReturnType<typeof setInterval>;
  problem?:Error;
  ready:Promise<string>; resolve:(token:string)=>void; reject:(error:unknown)=>void;
  failures:Set<(error:Error)=>void>;
}

/** One lease per engine/window/root. Resident roots stay leased until their final display closes. */
export class ViewMounts {
  private entries=new Map<string,Mount>();
  private active=0;
  constructor(private action:(node:string,identity:string,generation:string,action:string,token?:string)=>Promise<string|undefined>){}
  mount(node:string,identity:string,generation:string,failed?:(error:Error)=>void):{ready:Promise<string>;waiting:boolean;close:()=>void}{
    const key=`${generation}:${node}:${identity}`;
    let entry=this.entries.get(key);
    if(!entry){
      let resolve!:(token:string)=>void,reject!:(error:unknown)=>void;
      const ready=new Promise<string>((yes,no)=>{resolve=yes;reject=no;});
      entry={node,identity,generation,users:0,active:false,finished:false,closed:false,ready,resolve,reject,failures:new Set()};
      this.entries.set(key,entry);
    }
    entry.users++;if(failed)entry.failures.add(failed);
    if(entry.problem)failed?.(entry.problem);
    this.admit();
    const selected=entry;let closed=false;
    return {ready:selected.ready,waiting:!selected.active,close:()=>{
      if(closed)return;closed=true;selected.users--;
      if(failed)selected.failures.delete(failed);
      if(!selected.users){
        selected.closed=true;clearInterval(selected.timer);this.entries.delete(key);
        if(selected.token)this.closeLease(selected);
        else if(selected.finished)this.release(selected);
        else if(!selected.active)selected.reject(new Error("View closed"));
      }
    }};
  }
  private admit(){
    for(const entry of this.entries.values()){
      if(this.active>=max_active_view_roots())break;
      if(entry.active||entry.closed)continue;
      entry.active=true;this.active++;
      void this.action(entry.node,entry.identity,entry.generation,"open").then(token=>{
        if(!token)throw new Error("View lease was not created");
        entry.token=token;entry.finished=true;
        if(entry.closed){this.closeLease(entry);entry.reject(new Error("View closed"));return;}
        let busy=false;
        entry.timer=setInterval(()=>{
          if(busy||entry.closed)return;busy=true;
          void this.action(entry.node,entry.identity,entry.generation,"touch",token).catch(error=>{
            clearInterval(entry.timer); // A failed or uncertain lease is never automatically reopened.
            const problem=error instanceof Error?error:new Error("View display session is unavailable; reopen the view.");
            entry.problem=problem;
            for(const listener of entry.failures)listener(problem);
          }).finally(()=>{busy=false;});
        },1000);
        entry.resolve(token);
      }).catch(error=>{
        entry.finished=true;entry.reject(error);
        if(entry.closed)this.release(entry);
      });
    }
  }
  private closeLease(entry:Mount){
    // Keep the slot until cleanup settles, including opens completed after unmount.
    void this.action(entry.node,entry.identity,entry.generation,"close",entry.token).catch(()=>{}).finally(()=>this.release(entry));
  }
  private release(entry:Mount){
    if(!entry.active)return;
    entry.active=false;this.active--;this.admit();
  }
}
