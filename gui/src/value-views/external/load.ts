import type {ViewFrame} from "../instances";
import type {ViewAsset} from "./document";
import {externalModule} from "./module";
import {valueViewModules} from "../registry";
interface PackageIdentity {definition:string;digest:string;artifact?:string|null}
/** Assets stay isolated; both frame and ordinary-value discovery verify immutable identity. */
export async function loadPackages(entries:readonly PackageIdentity[],read:(digest:string)=>Promise<ViewAsset>){
  const packages=new Map(entries.filter(i=>i.artifact).map(i=>[i.artifact!,i]));
  // Bound simultaneous read admission even for a group with many distinct packages.
  const queue=[...packages];
  const worker=async()=>{
    for(let next=queue.shift();next;next=queue.shift()){
      const [digest,entry]=next;
      if(valueViewModules.named(entry.definition,digest))continue;
      const asset=await read(digest);
      if(asset.digest!==digest||asset.definition.artifact!==digest||asset.definition.id!==entry.definition||asset.definition.digest!==entry.digest||typeof asset.javascript!=="string"||typeof asset.css!=="string")throw new Error("View package identity does not match its instance.");
      if(!valueViewModules.named(entry.definition,digest))valueViewModules.register(externalModule(asset));
    }
  };
  await Promise.all([worker(),worker()]);
  for(const entry of entries.filter(i=>i.artifact)){const module=valueViewModules.named(entry.definition,entry.artifact);if(!module||module.definition?.digest!==entry.digest)throw new Error("View package is unavailable or mismatched.");}
}
export async function loadFramePackages(frame:ViewFrame,read:(digest:string)=>Promise<ViewAsset>){
  await loadPackages(frame.instances,read);
}
