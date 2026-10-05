import {createContext} from "react";
import {isNumeric,numericText} from "../../exact-json";
import type {StoredValue} from "../../protocol";
/** Known builtin identity contracts; arbitrary field names never imply identity. */
export const StreamItemsContext=createContext<ReadonlyMap<number,string>|undefined>(undefined);
export function streamItemKeys(value:StoredValue):ReadonlyMap<number,string>|undefined {
 if(value.type?.kind!=="list" || value.type.element.kind!=="record" || !["DockerStatsSample","DockerLogEvent","DockerContainerEvent"].includes(value.type.element.name??"") || !Array.isArray(value.data))return;
 const keys=new Map<number,string>(),seen=new Set<string>();
 for(let index=0;index<value.data.length;index++) {
  const row=value.data[index];if(!row || typeof row!=="object")return;
  const sequence=(row as Record<string,unknown>).sequence;if(!isNumeric(sequence) || !/^\d{1,30}$/.test(numericText(sequence)) || typeof (row as Record<string,unknown>).container!=="string")return;
  const key=JSON.stringify([(row as Record<string,unknown>).container??"",numericText(sequence)]);if(seen.has(key))return;
  seen.add(key);keys.set(index,key);
 }
 return keys;
}
