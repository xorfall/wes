import { useClipboard } from "../useClipboard";
import { useRef, useState } from "react";
import { isExactNumber, stringifyExactJson } from "../../exact-json";
import type { TypeShape } from "../../protocol";
import { DecodedBytes } from "../../presentation/bytes";
import { formatWire } from "../../presentation/format";
import { datasetSelect, datasetSummary, decodeDatasetData } from "../../presentation/dataset";
import { DatasetRegion } from "./DatasetBrowser";
import { useColumns } from "./measure";
import { declarationPath, declaredTone, fieldMeta, type Tone } from "../../value-meta";

/** Declared tones in the roles the rest of the client draws them with. */
const TONE_ROLE: Record<Tone, string> = { ok: "mono-ok", warn: "mono-warn", bad: "mono-bad", dim: "mono-dim", meta: "mono-meta", ink: "mono-ink" };

const structure = (value: unknown) => value !== null && typeof value === "object" && !isExactNumber(value) && !(value instanceof DecodedBytes);
export const pointerPart = (key: string) => key.replace(/~/g,"~0").replace(/\//g,"~1");
export function jsonSummary(value: unknown): string {
  if (value instanceof DecodedBytes) return `${value.size} B · ${value.text === undefined ? value.problem ?? "not text" : "utf-8"}`;
  if (value === undefined) return "missing";
  if (value === null) return "null";
  if (Array.isArray(value)) return value.length ? `List · ${value.length} items` : "no items";
  if (structure(value)) { const keys=Object.keys(value as object);return keys.length ? `{ ${keys.slice(0,3).join(", ")}${keys.length>3 ? ", …" : ""} } · ${keys.length} keys` : "{}"; }
  return stringifyExactJson(value) ?? String(value);
}
/** Text sent as-is by `formatWire`; the locale only applies to numeric times, never read here. */
const EXACT_TEXT = { locale: "en-GB", timeZone: "UTC" } as const;
/**
 * A summary that knows the value's declared type: a temporal primitive's canonical text (`Instant`,
 * `Duration`, `Interval`) is shown exactly as sent, unquoted. Text that merely looks like a time and
 * every other value keep the generic `jsonSummary`; raw JSON and copy are unaffected.
 */
export function typedJsonSummary(value: unknown, type?: TypeShape): string {
  if (type?.kind === "dataset" && value !== undefined && value !== null) return datasetSummary(type, value);
  if (type?.kind !== "primitive" || typeof value !== "string") return jsonSummary(value);
  return formatWire(type.name, value, EXACT_TEXT.locale, EXACT_TEXT.timeZone) ?? jsonSummary(value);
}
function unwrap(value:unknown,type?:TypeShape):{value:unknown;type?:TypeShape;none?:boolean} {
  if(type?.kind==="option" && value!==null && typeof value==="object" && !Array.isArray(value)) {
    const option=value as {kind?:unknown;value?:unknown};
    if(option.kind==="none")return {value:null,type:type.element,none:true};
    if(option.kind==="some")return unwrap(option.value,type.element);
  }
  return {value,type};
}
/** A Dataset descriptor that does not validate opens to nothing: its summary already says so. */
const invalidDataset=(value:unknown,type?:TypeShape)=>type?.kind==="dataset" && !decodeDatasetData(value);
const keysOf=(value:unknown,type?:TypeShape):string[]=>{
  if(!structure(value) || Array.isArray(value) || invalidDataset(value,type))return [];
  const declared=type?.kind==="record" ? type.fields.map(field=>field.name) : [];
  const seen=new Set(declared);
  return [...declared,...Object.keys(value as object).filter(key=>!seen.has(key))];
};
const countOf=(value:unknown,type?:TypeShape)=>Array.isArray(value) && !invalidDataset(value,type) ? value.length : keysOf(value,type).length;
const entryOf=(value:unknown,type:TypeShape|undefined,key:string):[string,unknown,TypeShape|undefined]=>[key,(value as Record<string,unknown>)[key],type?.kind==="list" ? type.element : type?.kind==="record" ? type.fields.find(field=>field.name===key)?.type : undefined];
const entries=(value:unknown,type:TypeShape|undefined,limit:number):[string,unknown,TypeShape|undefined][]=>
  Array.isArray(value) ? value.slice(0,limit).map((item,at)=>[String(at),item,type?.kind==="list"?type.element:undefined])
    : keysOf(value,type).slice(0,limit).map(key=>entryOf(value,type,key));


/** Exact data in a bounded, path-addressable tree; narrow boxes use one level at a time. */
export function JsonTree({ data, type, mode="window", collapsed=false, declared, datasets }: {data:unknown;type?:TypeShape;mode?:string;collapsed?:boolean;declared?:import("../../presentation/types").Declared;datasets?:{readonly root:TypeShape;readonly at:string}}) {
  /** A scalar's declared tone at its pointer, when the tree carries metadata for its declaration. */
  const toned=(address:string,item:unknown)=>{
    if(!declared||!type)return undefined;
    const relative=declarationPath(type,address);
    const tone=relative===undefined ? undefined : declaredTone(fieldMeta(declared.meta,declared.at+relative),item);
    return tone ? TONE_ROLE[tone] : undefined;
  };
  const clipboard=useClipboard();
  const root=useRef<HTMLDivElement>(null);
  const columns=useColumns(root);
  const unwrapped=unwrap(data,type);
  const [opened,setOpened]=useState<ReadonlySet<string>>(new Set());
  const [shown,setShown]=useState<ReadonlyMap<string,number>>(new Map());
  const [path,setPath]=useState<{key:string}[]>([]);
  const [selected,setSelected]=useState("");
  const [whole,setWhole]=useState<ReadonlySet<string>>(new Set());
  // Datasets whose records were opened by hand; nothing is read for one until then.
  const [reading,setReading]=useState<ReadonlySet<string>>(new Set());
  const narrow=columns<40 && mode!=="preview";
  // A breadcrumb stores addresses, never an old live payload.
  let current=unwrap(data,type);
  for(const part of narrow ? path : []) {
    const entry=structure(current.value) && Object.hasOwn(current.value as object,part.key) ? entryOf(current.value,current.type,part.key) : undefined;
    if(!entry)break;
    current=unwrap(entry[1],entry[2]);
  }
  const toggle=(key:string)=>setOpened(was=>{const next=new Set(was);if(next.has(key))next.delete(key);else next.add(key);return next;});
  let visited=0;
  const branch=(value:unknown,shape:TypeShape|undefined,at:string,depth:number): React.ReactNode => {
    if(depth>64 || ++visited>2000)return <p className="mono-faint">Tree limit reached · open a smaller branch</p>;
    const limit=Math.min(2000,shown.get(at) ?? (mode === "preview" ? 4 : 50));
    const total=countOf(value,shape);
    const list=entries(value,shape,Math.min(limit, Math.max(0,2000-visited)));
    return <div role="group" className="json-branch">
      {list.slice(0,limit).map(([key,item,itemType])=>{
        if (++visited>2000)return null;
        const address=`${at}/${pointerPart(key)}`;
        const openedItem=unwrap(item,itemType);
        item=openedItem.value;itemType=openedItem.type;
        const children=countOf(item,itemType)>0;
        const summary=openedItem.none ? "none" : typedJsonSummary(item,itemType);
        const cut=typeof item === "string" && summary.length>80 && !whole.has(address);
        return <div key={address} className="json-entry">
          <div className={`json-row${selected===address ? " json-selected" : ""}`} onClick={()=>setSelected(address)}>
            {children ? <button className="json-toggle" aria-label={`${opened.has(address)?"Close":"Open"} ${address}`} aria-expanded={opened.has(address)} onClick={()=>narrow ? setPath(was=>[...was,{key}]) : toggle(address)}>{opened.has(address)&&!narrow ? "▾" : "▸"}</button> : <span className="json-toggle"/>}
            <span className="json-pair"><span className="json-key">{key}</span>
            <span className={`json-value ${structure(item) ? "json-shape" : item==null ? "mono-faint" : toned(address, item) ?? (typeof item === "string" ? "mono-literal" : typeof item === "boolean" ? "mono-ref" : "mono-meta")}`}>{cut ? `${summary.slice(0,80)}…` : summary}{cut && <button className="cell-action json-chars" onClick={()=>setWhole(was=>new Set(was).add(address))}>+{summary.length-80} chars</button>}</span></span>
          </div>
          {!narrow && children && opened.has(address) && <div className="json-indent">{branch(item,itemType,address,depth+1)}</div>}
          {datasetRecords(address,item,itemType)}
        </div>;
      })}
      {(limit>=2000 || visited>=2000) && total>list.length ? <p className="mono-faint">Tree limit reached · {total-list.length} items not shown · select a smaller branch in JSON</p> : total>limit && <button className="cell-action json-more" onClick={()=>setShown(was=>new Map(was).set(at,limit+50))}>show {Math.min(50,total-limit)} more · {total-limit} not shown</button>}
    </div>;
  };
  /** A valid Dataset's explicit records action and, once used, its page reader under the entry. */
  const datasetRecords=(address:string,item:unknown,itemType:TypeShape|undefined)=>{
    const reference=itemType?.kind==="dataset" && datasets ? decodeDatasetData(item) : undefined;
    if(!reference || itemType?.kind!=="dataset" || !datasets)return null;
    const select=datasetSelect(datasets.root,`${datasets.at}${address}`);
    const open=reading.has(address);
    return <div className="json-indent">
      <button type="button" className="cell-action" aria-expanded={open} aria-label={`${open?"Close":"Read"} records at ${address}`}
        onClick={()=>setReading(was=>{const next=new Set(was);if(next.has(address))next.delete(address);else next.add(address);return next;})}>{open?"close records":"read records"}</button>
      {open && <DatasetRegion anchor={{reference,type:itemType,...(select===undefined?{}:{select})}}>{null}</DatasetRegion>}
    </div>;
  };
  const previewGrown=mode==="preview" && (opened.size>0 || shown.size>0 || whole.size>0 || reading.size>0);
  return <div ref={root} className={`json-tree json-${mode}${columns<48 ? " json-stacked" : ""}${previewGrown ? " json-preview-grown" : ""}`}>
    {collapsed ? <p className="json-facts">{unwrapped.none ? "none" : typedJsonSummary(unwrapped.value,unwrapped.type)}</p> : <>
      {narrow && <nav className="json-breadcrumb" aria-label="JSON location"><button className="cell-action" onClick={()=>setPath([])}>root</button>{path.map((part,at)=><button key={at} className="cell-action" onClick={()=>setPath(was=>was.slice(0,at+1))}>/ {part.key}</button>)}</nav>}
      {structure(current.value) && !invalidDataset(current.value,current.type) ? branch(current.value,current.type,narrow ? `/${path.map(part=>pointerPart(part.key)).join("/")}`.replace(/\/$/,"") : "",0) : <p className="json-facts">{current.none ? "none" : typedJsonSummary(current.value,current.type)}</p>}
      {clipboard.notice && <p className="mono-dim" role="status">{clipboard.notice}</p>}
      {mode === "window" && selected && <footer className="json-location"><span>{selected}</span><button className="cell-action" onClick={()=>void clipboard.copy(selected)}>copy pointer</button></footer>}
    </>}
  </div>;
}
