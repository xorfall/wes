import type {ViewAsset} from "./document";
/** Shipped and fetched artifacts consume the same bounded renderer cache. */
export class AssetBudget {
  private readonly retained=new Set<string>();private bytes=0;
  retain(asset:ViewAsset){
    if(this.retained.has(asset.digest))return;
    const encoder=new TextEncoder(),javascript=encoder.encode(asset.javascript).length,css=encoder.encode(asset.css).length;
    if(this.retained.size>=64)throw new Error("Loaded View package limit reached (64). Reopen the application to clear unused packages.");
    if(javascript>4*1024*1024||css>256*1024)throw new Error("View assets exceed the package size limit.");
    const charge=javascript+css;
    if(this.bytes+charge>32*1024*1024)throw new Error("Loaded View assets exceed the 32 MiB budget.");
    this.retained.add(asset.digest);this.bytes+=charge;
  }
}
const budget=new AssetBudget();
export const retainAsset=(asset:ViewAsset)=>budget.retain(asset);
