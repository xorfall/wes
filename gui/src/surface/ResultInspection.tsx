import { stringifyExactJson } from "../exact-json";
/**
 * Complete, passive inspection shared by screens, panes and result windows.
 *
 * What is drawn for which result is `result-views.tsx`'s question; these are the drawings
 * themselves, and they decide nothing about when they are used.
 */
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import type { StoredValue } from "../protocol";

function passive(event: KeyboardEvent) {
  event.stopPropagation();
  if ((event.metaKey || event.ctrlKey) && (event.key === "Enter" || event.key.toLowerCase() === "r")) event.preventDefault();
}

type JsonReading = {text:string;problem?:never} | {text?:never;problem:string};
function serialize(value:StoredValue):JsonReading {
  try{return {text:stringifyExactJson(value.data,2)};}catch(error){return {problem:error instanceof Error ? error.message : String(error)};}
}
export function ReadableJson({ value,active=true }: { readonly value: StoredValue;readonly active?:boolean }) {
  const [limit, setLimit] = useState(16_384);
  const cached=useRef<JsonReading>();
  const reading=useMemo(()=>{if(active)cached.current=serialize(value);return cached.current;},[value,active]);
  if(!reading)return null;
  if(reading.problem!==undefined)return <p className="mono-warn" role="alert">JSON display unavailable · {reading.problem} · inspect a smaller part of the value.</p>;
  const text=reading.text;
  return <div className="result-inspection" onKeyDown={passive}>
    <p className="mono-faint">stored JSON · exact numbers · Bytes as base64</p>
    <pre className="inspection-text open-json" tabIndex={0}>{text.slice(0,limit)}</pre>
    {text.length>limit && <button className="cell-action" onClick={()=>setLimit(n=>n+16_384)}>show more JSON · {text.length-limit} characters not shown</button>}
    {active && <Download data={text} name="result.json" mime="application/json" label="Download original data"/>}
  </div>;
}

export function EncodedData({ value }: { readonly value: StoredValue }) {
  const reading=useMemo(()=>serialize(value),[value]);
  return <div className="result-inspection" onKeyDown={passive}>
    {reading.problem!==undefined ? <p className="mono-warn" role="alert">JSON export unavailable · {reading.problem}</p> : <Download data={reading.text} name="result.json" mime="application/json" label="Download original data" />}
  </div>;
}


function Download({ data, name, mime, label, encoded = false }: { data: string; name: string; mime: string; label: string; encoded?: boolean }) {
  const [url, setUrl] = useState<string>();
  useEffect(() => {
    try {
      const bytes = encoded ? Uint8Array.from(atob(data), char => char.charCodeAt(0)) : data;
      const href = URL.createObjectURL(new Blob([bytes], { type: mime }));
      setUrl(href);
      return () => URL.revokeObjectURL(href);
    } catch { setUrl(undefined); }
  }, [data, mime, encoded]);
  return url ? <a href={url} download={name}>{label}</a> : <span className="mono-faint">Download unavailable</span>;
}
