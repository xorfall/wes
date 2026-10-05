import { afterEach, describe, expect, it, vi } from "vitest";
import { describeSourceKind, libraryAction, parseCredentialReferences, readableIdentity, readableOrigin, revisionKey } from "./api-library";

afterEach(()=>vi.unstubAllGlobals());
it("uses the shared backend with JSON and never browser preference storage", async()=>{
  const fetch=vi.fn().mockResolvedValue({ok:true,json:async()=>({settings:null,revision:null})});
  vi.stubGlobal("fetch",fetch);
  expect(await libraryAction({action:"status"})).toEqual({settings:null,revision:null});
  expect(fetch).toHaveBeenCalledWith("/api-library",{method:"POST",headers:{"Content-Type":"application/json"},body:'{"action":"status"}'});
});
it("surfaces repository and conflict errors without retrying ingestion",async()=>{
  const fetch=vi.fn().mockResolvedValue({ok:false,status:400,text:async()=>"repository unavailable"});vi.stubGlobal("fetch",fetch);
  await expect(libraryAction({action:"resolve",key:{service:"a",apiVersion:"v1",scope:"all"}})).rejects.toThrow("repository unavailable");
  expect(fetch).toHaveBeenCalledTimes(1);
});
it("distinguishes scopes and exact revisions in catalog identities",()=>{
  const base={key:{service:"a",apiVersion:"v1",scope:"all"},revision:"a".repeat(64),accepted:false,origin:"fixture",sourceDigest:null};
  expect(revisionKey(base)).not.toBe(revisionKey({...base,key:{...base.key,scope:"public"}}));
  expect(revisionKey(base)).not.toBe(revisionKey({...base,revision:"b".repeat(64)}));
});
it("accepts credential references and refuses non-object/non-string bindings",()=>{
  expect(parseCredentialReferences('{"token":"team/dev/api"}')).toEqual({token:"team/dev/api"});
  expect(parseCredentialReferences("")).toEqual({});
  for(const value of ['[]','null','{"token":23}']) expect(()=>parseCredentialReferences(value)).toThrow();
});
describe("readable library identity", () => {
  it("should_HideSyntheticIdentity_When_TheKeyCameFromDescribe", () => {
    expect(readableIdentity({service:"a",apiVersion:"describe",scope:"f".repeat(64)})).toBeUndefined();
    expect(readableIdentity({service:"a",apiVersion:"v1",scope:"public"})).toEqual({version:"v1",scope:"public"});
  });
  it("should_StateTheSourceOnce_When_DescribedAndDiscoveredLocationsMatch", () => {
    expect(readableOrigin("described:https://d.invalid/g; source:https://d.invalid/g")).toEqual({source:"https://d.invalid/g"});
    expect(readableOrigin("described:https://d.invalid/; source:https://d.invalid/o.json")).toEqual({source:"https://d.invalid/",via:"https://d.invalid/o.json"});
    expect(readableOrigin("github:o/r@abc/specs")).toEqual({source:"github:o/r@abc/specs"});
    expect(readableOrigin("  ")).toBeUndefined();
  });
  it("should_SubmitUrlOnlyForSchemeLocations_When_DescribingDocumentation", () => {
    expect(describeSourceKind("https://d.invalid/guide")).toBe("url");
    expect(describeSourceKind(" HTTP://d.invalid")).toBe("url");
    expect(describeSourceKind("/tmp/openapi.json")).toBe("file");
    expect(describeSourceKind("docs/https-notes.md")).toBe("file");
  });
});
