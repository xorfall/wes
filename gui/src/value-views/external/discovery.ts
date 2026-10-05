import type {StoredValue} from "../../protocol";
import {isExactNumber} from "../../exact-json";
import {decodeContract,type ViewDefinition} from "../definition";
import {builtinValueViews,valueViewModules} from "../registry";
import {loadPackages} from "./load";
import type {ViewAsset} from "./document";

/** Only marker-bearing records are candidates; neither metadata nor a marker executes code. */
function markedValues(value:StoredValue){
  const found:unknown[]=[];let left=200_000;
  const visit=(data:unknown,depth:number)=>{
    if(--left<0||depth>64||!data||typeof data!=="object"||isExactNumber(data))return;
    if(Array.isArray(data)){for(const child of data){if(left<0)break;visit(child,depth+1);}return;}
    const record=data as Record<string,unknown>;
    if(typeof record.view==="string"&&found.length<1024)found.push(record);
    for(const child of Object.values(record)){if(left<0)break;visit(child,depth+1);}
  };
  visit(value.data,0);return found;
}
const digest=(v:unknown):v is string=>typeof v==="string"&&/^[a-f0-9]{64}$/.test(v);
/** One catalogue per observation epoch; only renderers required by a value are fetched. */
export class ValuePackages {
  private catalogue?:Promise<readonly ViewDefinition[]>;
  constructor(private readonly readCatalogue:()=>Promise<unknown>,private readonly readAsset:(digest:string)=>Promise<ViewAsset>){}
  invalidate(){this.catalogue=undefined;}
  async modules(value:StoredValue){
    const candidates=markedValues(value);
    if(!candidates.length)return builtinValueViews;
    if(!this.catalogue){
      const reading=this.readCatalogue().then(raw=>{
        if(!Array.isArray(raw)||raw.length>4096)throw new Error("Invalid View catalogue.");
        return raw.filter((entry):entry is ViewDefinition=>!!entry&&typeof entry.id==="string"&&digest(entry.digest)&&digest(entry.artifact));
      });
      this.catalogue=reading;
      void reading.catch(()=>{if(this.catalogue===reading)this.catalogue=undefined;});
    }
    const reading=this.catalogue;
    const installed=await reading;
    if(this.catalogue!==reading)throw new Error("View catalogue changed during discovery.");
    let validations=128;
    const matches=installed.filter(definition=>candidates.some(data=>{
      try{
        const schema=definition.contracts[definition.input],marker=schema?.fields?.view;
        if(!marker||!definition.contracts[marker.type]?.constraints.enum.includes((data as {view:string}).view))return false;
        if(--validations<0)return false;
        decodeContract(definition,definition.input,data,true);return true;
      }catch{return false;}
    }));
    await loadPackages(matches.map(d=>({definition:d.id,digest:d.digest,artifact:d.artifact})),this.readAsset);
    return [...builtinValueViews,...matches.flatMap(d=>{
      const module=valueViewModules.named(d.id,d.artifact)!;
      return builtinValueViews.includes(module)?[]:[module];
    })];
  }
}
