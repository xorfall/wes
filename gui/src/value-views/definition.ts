import { isNumeric, isExactNumber, numericText, compareNumeric, type NumericValue } from "../exact-json";
import { nanos, range } from "./temporal";
import { ViewInputError } from "./contract";
import type { TypeShape } from "../protocol";
import { decodeDatasetData } from "../presentation/dataset";

import type { ViewDefinition, ContractSchema } from "../../../packages/view-sdk/contract";
export type { ViewDefinition, ContractSchema } from "../../../packages/view-sdk/contract";

const object = (v: unknown): v is Record<string, unknown> => v !== null && typeof v === "object" && !Array.isArray(v) && !isExactNumber(v);
function compare(a: NumericValue, b: string) { return compareNumeric(a,b); }
/** Presentation boundary; backend contracts remain authoritative for engine values. Native mode
 * is used for React interaction messages, wire mode decodes tagged Option values from the engine.
 */
export function decodeContract(definition: ViewDefinition, name: string, value: unknown, wire = false, shape?: TypeShape): unknown {
  let remaining = 500_000, bytes = 8 * 1024 * 1024;
  const visit = (name: string, v: unknown, depth: number, type?: TypeShape): unknown => {
    if (--remaining < 0 || depth > 64 || bytes < 0) throw new ViewInputError("View input exceeds validation budget");
    const s=definition.contracts[name]; if (!s) throw new Error("Missing view contract");
    const c=s.constraints;
    const fail=(): never => {throw new ViewInputError(`View input does not satisfy ${name}`);};
    if (s.kind === "option") {
      if(type && type.kind!=="option")return fail();
      if (wire) {
        if (!object(v) || (v.kind !== "some" && v.kind !== "none")) return fail();
        return v.kind === "none" ? null : visit(s.element!,v.value,depth+1,type?.kind==="option"?type.element:undefined);
      }
      return v === null ? null : visit(s.element!,v,depth+1,type?.kind==="option"?type.element:undefined);
    }
    if (s.kind === "dataset") {
      // Only engine input carries a Dataset, and only as the exact descriptor of one snapshot; a
      // View's own outputs, state and events never hold one. Its declared row contract must match
      // the captured element shape, so pages are read against what the View was compiled for.
      if (!wire || type && type.kind !== "dataset") return fail();
      const reference = decodeDatasetData(v);
      if (!reference) return fail();
      if (type && !conforms(definition, s.element!, type.element, 0)) return fail();
      return Object.freeze({ kind: "dataset", reference });
    }
    if (s.kind === "union") {
      // The native shape model erases a declared union to Unknown. Its explicit alternatives
      // still validate the complete value; an ordinary Unknown input is not a type proof.
      const proof = type?.kind === "unknown" ? undefined : type;
      for (const alternative of s.alternatives!) {try{return visit(alternative,v,depth+1,proof);}catch(error){if(!(error instanceof ViewInputError))throw error;}}
      return fail();
    }
    if (s.kind === "record") {
      if (!object(v) || type && type.kind!=="record") return fail();
      const result: Record<string,unknown> = Object.create(null);
      for (const [key,f] of Object.entries(s.fields!)) {
        if (!Object.hasOwn(v,key)) {if(f.optional)continue;return fail();}
        const childType=type?.kind==="record"?type.fields.find(f=>f.name===key)?.type:undefined;
        if(type && !childType)return fail();
        result[key]=visit(f.type,v[key],depth+1,childType);
      }
      return Object.freeze(result);
    }
    if (s.kind === "list") {
      if (!Array.isArray(v) || type && type.kind!=="list" || v.length > remaining || c.minItems !== null && v.length<c.minItems || c.maxItems !== null && v.length>c.maxItems) return fail();
      return Object.freeze(v.map(item=>visit(s.element!,item,depth+1,type?.kind==="list"?type.element:undefined)));
    }
    if (s.kind !== "scalar") return fail();
    if(type && (type.kind!=="primitive" || type.name!==s.primitive?.toUpperCase()))return fail();
    const valid = s.primitive === "Text" ? typeof v === "string" :
      s.primitive === "Bool" ? typeof v === "boolean" :
      s.primitive === "Int" ? isNumeric(v) && /^-?\d+$/.test(numericText(v)) && compare(v,"-9223372036854775808")>=0 && compare(v,"9223372036854775807")<=0 :
      s.primitive === "Decimal" ? isNumeric(v) :
      s.primitive === "Instant" ? nanos(v) !== undefined :
      s.primitive === "Interval" ? range(v) !== undefined :
      s.primitive === "Duration" ? typeof v === "string" && v.length < 128 && /^PT(?=-?\d)(?:-?\d+H)?(?:-?\d+M)?(?:-?\d+(?:\.\d{1,9})?S)?$/.test(v) :
      s.primitive === "Bytes" ? typeof v === "string" && /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(v) : false;
    if (!valid) return fail();
    if (typeof v === "string") {
      bytes-=v.length*2;
      if (bytes<0) return fail();
      if (c.minLength !== null || c.maxLength !== null) {
        // Wes Text lengths count Unicode code points, not UTF-16 code units.
        let length=0;
        for (const _ of v) length++;
        if (c.minLength !== null && length<c.minLength || c.maxLength !== null && length>c.maxLength) return fail();
      }
    } else bytes-=isExactNumber(v)?v.text.length*2:8;
    if(bytes<0)return fail();
    if (isNumeric(v) && (c.min!==null && compare(v,c.min)<0 || c.max!==null && compare(v,c.max)>0)) return fail();
    if(c.enum.length && !c.enum.some(e=>isNumeric(v) && (isNumeric(e) || typeof e === "string" && ["Int","Decimal"].includes(s.primitive!)) ? compare(v,String(e))===0 : e===v))return fail();
    return v;
  };
  return visit(name,value,0,shape);
}

/**
 * Whether a captured element shape is the declared contract's shape: same kinds, same primitives,
 * every declared record field present (required ones non-optional in shape terms is not knowable,
 * so presence is checked). Unknown alternatives of a union accept any one matching alternative.
 */
function conforms(definition: ViewDefinition, name: string, shape: TypeShape, depth: number): boolean {
  const s = definition.contracts[name];
  if (!s || depth > 32) return false;
  switch (s.kind) {
    case "scalar": return shape.kind === "primitive" && shape.name === s.primitive?.toUpperCase();
    case "list": return shape.kind === "list" && conforms(definition, s.element!, shape.element, depth + 1);
    case "option": return shape.kind === "option" && conforms(definition, s.element!, shape.element, depth + 1);
    case "union": return shape.kind === "unknown" || s.alternatives!.some(alternative => conforms(definition, alternative, shape, depth + 1));
    case "record": {
      if (shape.kind !== "record") return false;
      return Object.entries(s.fields!).every(([key, field]) => {
        const found = shape.fields.find(item => item.name === key);
        return found ? conforms(definition, field.type, found.type, depth + 1) : field.optional;
      });
    }
    default: return false;
  }
}

/** Canonical protocol identity includes its reachable contracts, not unrelated view input types. */
export function protocolIdentity(definition: ViewDefinition): string {
  const i=definition.interaction; if(!i)return "";
  const schemas: Record<string,ContractSchema>={};
  const add=(name:string)=>{if(schemas[name])return;const s=definition.contracts[name]!;schemas[name]=s;
    if(s.element)add(s.element);for(const a of s.alternatives??[])add(a);for(const f of Object.values(s.fields??{}))add(f.type);};
  add(i.state);add(i.event);
  const outputs=Object.fromEntries(Object.entries(definition.outputs).filter(([,p])=>p.shared));
  for(const output of Object.values(outputs))add(output.type);
  return JSON.stringify([i,outputs,Object.keys(schemas).sort().map(k=>[k,schemas[k]])]);
}
