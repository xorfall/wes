import { expect, it, vi } from "vitest";
import { complete } from "./complete";
import { emptyCatalogue, type Catalogue } from "./vocabulary";
import { suggestionLine, promptCompletion } from "./surface/prompt-complete";
const id="a".repeat(64);
const catalogue: Catalogue = { ...emptyCatalogue, providers: [{name:"dock",ready:true,credentials:[],capabilities:[{path:["inspect"],summary:"",result:"Any",safe:true,parameters:[{name:"container",type:"Text",required:true,allowed:[],content:"",resourceHint:"Run dock containers to load suggestions",resources:{node:"id1000",observedAtNs:"1000000000",items:[{value:id,label:"shop-api",detail:"running"}]}}]}]}] };
it("matches resource names but inserts exact identity, with freshness and source",()=>{
  vi.spyOn(Date,"now").mockReturnValue(6000);
  const line="dock inspect container:api";
  const result=complete(line,line.length,catalogue,[]);
  expect(result.items).toEqual([{text:`container:${id}`,label:"shop-api",kind:"resource",detail:expect.stringContaining("5s before this menu")}]);
  expect(suggestionLine(result.items[0]!,false).map(s=>s.text).join("")).toContain("shop-api");
  expect(promptCompletion({line,caret:line.length,catalogue,names:[],aliases:{}}).hint).toContain("dock containers");
  vi.restoreAllMocks();
});
it("withdraws suggestions with catalogue replacement and does not require remote completion",()=>{
  const line="dock inspect container:";
  expect(complete(line,line.length,emptyCatalogue,[]).items).toEqual([]);
  const missing=structuredClone(catalogue);
  delete (missing.providers[0]!.capabilities[0]!.parameters[0]! as {resources?:unknown}).resources;
  const result=complete(line,line.length,missing,[]);
  expect(result.items).toEqual([]); expect(result.hint).toContain("Run dock containers");
});
