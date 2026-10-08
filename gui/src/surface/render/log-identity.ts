import { isNumeric, numericText } from "../../exact-json";
import type { StoredValue } from "../../protocol";
import type { LogMapping, Registry } from "../../presentation/registry";
/**
 * Declared item identity, read once for every consumer: the log view's rows and a live window's item
 * keys. Identity comes only from a builtin contract or a validated presentation mapping; field names
 * never imply it, and a row without its declared identity has none (never its position).
 */
const DOCKER_KEYED=["DockerStatsSample","DockerLogEvent","DockerContainerEvent"];
export function isLogValue(value:StoredValue):boolean {
  return value.type?.kind==="list" && value.type.element.kind==="record" && value.type.element.name==="DockerLogEvent" && Array.isArray(value.data);
}
/** How a value reads as a log: Docker's own shape (no mapping), the mapping its entry declares, or not at all. */
export function logShape(value:StoredValue,registry:Pick<Registry,"logMapping">):{readonly mapping?:LogMapping}|undefined {
  if(isLogValue(value))return {};
  const mapping=registry.logMapping(value.type,value.data);
  return mapping && Array.isArray(value.data) ? {mapping} : undefined;
}
export function numericBigInt(value:unknown):bigint|undefined {
  if(!isNumeric(value))return;
  const text=numericText(value);if(!/^-?\d{1,30}$/.test(text))return;
  return BigInt(text);
}
/** An Option arrives as `{kind:"some",value}` or `{kind:"none"}`; anything else is the value itself. */
function open(value:unknown):unknown {
  while(value && typeof value==="object" && !Array.isArray(value) && ((value as {kind?:unknown}).kind==="some" || (value as {kind?:unknown}).kind==="none"))
    value=(value as {kind:string;value?:unknown}).kind==="some"?(value as {value?:unknown}).value:undefined;
  return value;
}
/** A declared field of a row; a dotted name reads a field of a record field: `artifact.digest`. */
export function field(row:Record<string,unknown>,name:string|undefined):unknown {
  if(name===undefined)return undefined;
  let value:unknown=row;
  for(const part of name.split(".")){
    value=open(value);
    if(!value || typeof value!=="object" || Array.isArray(value))return undefined;
    value=(value as Record<string,unknown>)[part];
  }
  return open(value);
}
/** A scalar as exact text: a number keeps its wire digits. Records, lists and none have no label. */
export function label(value:unknown):string|undefined {
  if(typeof value==="string")return value;
  if(typeof value==="boolean")return String(value);
  if(isNumeric(value))return numericText(value);
  return undefined;
}
function record(raw:unknown):Record<string,unknown>|undefined {
  return raw && typeof raw==="object" && !Array.isArray(raw) ? raw as Record<string,unknown> : undefined;
}
/** Docker's `(container, sequence)`: a text container and a non-negative integer sequence, normalized. */
export function dockerIdentity(raw:unknown):{key:string;container:string;sequence:string}|undefined {
  const row=record(raw);if(!row || typeof row.container!=="string")return;
  const sequence=numericBigInt(row.sequence);if(sequence===undefined || sequence<0n)return;
  return {key:JSON.stringify([row.container,sequence.toString()]),container:row.container,sequence:sequence.toString()};
}
/** The mapping's key fields in order, each a scalar label; any missing or non-scalar part means no identity. */
export function mappedIdentity(raw:unknown,mapping:LogMapping):{key:string;parts:readonly string[]}|undefined {
  const row=record(raw);if(!row)return;
  const parts=mapping.key.map(name=>label(field(row,name)));
  if(parts.some(part=>part===undefined))return;
  return {key:JSON.stringify(parts),parts:parts as string[]};
}
/**
 * Every item's declared key by position, or nothing: a list with an item lacking its identity, or two
 * items sharing one, has no item identity at all. `mapping` is the validated log mapping of the value.
 */
export function declaredItemKeys(value:StoredValue,mapping?:LogMapping):ReadonlyMap<number,string>|undefined {
  if(value.type?.kind!=="list" || value.type.element.kind!=="record" || !Array.isArray(value.data))return;
  const docker=DOCKER_KEYED.includes(value.type.element.name??"");
  if(!docker && !mapping)return;
  const keys=new Map<number,string>(),seen=new Set<string>();
  for(let index=0;index<value.data.length;index++) {
    const key=(docker ? dockerIdentity(value.data[index]) : mappedIdentity(value.data[index],mapping!))?.key;
    if(key===undefined || seen.has(key))return;
    seen.add(key);keys.set(index,key);
  }
  return keys;
}
