import { useClipboard } from "../useClipboard";
import { useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { isNumeric } from "../../exact-json";
import { composing } from "../../platform-keys";
import type { StoredValue } from "../../protocol";
import { byteSize, readBytes, DecodedBytes } from "../../presentation/bytes";
import { max_decoded_body } from "../../views/http-response";
import { useColumns } from "./measure";
import { width, ellipsizeEnd } from "../../presentation/columns";

export function isProcessOutput(value: StoredValue): boolean {
  if(value.type?.kind!=="record" || value.type.name!=="ProcessOutput" || !value.data || typeof value.data!=="object" || Array.isArray(value.data))return false;
  const data=value.data as Record<string,unknown>;
  return isNumeric(data.exitCode) && [data.stdout,data.stderr].some(it=>typeof it==="string") && [data.stdout,data.stderr].every(it=>it===undefined || typeof it==="string");
}
const size=(bytes:number)=>bytes<1024 ? `${bytes} B` : bytes<1024*1024 ? `${(bytes/1024).toFixed(1)} KiB` : `${(bytes/1024/1024).toFixed(1)} MiB`;
const linesOf=(text:string)=>text==="" ? [] : text.replace(/\r\n/g,"\n").replace(/\r/g,"\n").replace(/\n$/,"").split("\n");
export function previewLines(lines:readonly string[], columns:number, rows:number): {lines:{number:number;text:string}[];remaining:number;cut:boolean} {
  const drawn:{number:number;text:string}[]=[];
  let budget=Math.max(0,rows), cut=false;
  for(let at=0;at<lines.length && budget>0;at++) {
    const text=lines[at]!, cost=Math.max(1,Math.ceil(width(text.replace(/\t/g,"        "),columns*Math.max(1,rows))/Math.max(1,columns)));
    if(cost>budget){drawn.push({number:at+1,text:ellipsizeEnd(text.replace(/\t/g,"        "),columns*budget)});cut=true;break;}
    drawn.push({number:at+1,text});budget-=cost;
  }
  return {lines:drawn,remaining:Math.max(0,lines.length-drawn.length),cut};
}

export async function saveBytes(encoded:string,name:string):Promise<void> {
  const parts:Uint8Array<ArrayBuffer>[]=[];
  for(let at=0;at<encoded.length;at+=262144) {
    parts.push(Uint8Array.from(atob(encoded.slice(at,at+262144)),char=>char.charCodeAt(0)));
    if(at && at%2097152===0)await new Promise(resolve=>setTimeout(resolve,0));
  }
  const url=URL.createObjectURL(new Blob(parts)),anchor=document.createElement("a");
  anchor.href=url;anchor.download=name;anchor.click();setTimeout(()=>URL.revokeObjectURL(url),1000);
}
/** Process stdout and stderr are independent Bytes. Looking never executes a command. */
export function ProcessOutput({value,mode,collapsed=false,whole=true,preparedData}:{value:StoredValue;mode:string;collapsed?:boolean;whole?:boolean;preparedData?:unknown}) {
  const clipboard=useClipboard();
  const data=value.data as Record<string,unknown>;
  const scroll=useRef<HTMLDivElement>(null);
  const root=useRef<HTMLDivElement>(null), lineRefs=useRef(new Map<number,HTMLElement>());
  const columns=useColumns(root);
  const streams=useMemo(()=>Object.fromEntries(["stdout","stderr"].map(name=>{
    const encoded=typeof data[name]==="string" ? data[name] as string : undefined;
    const prepared=preparedData && typeof preparedData==="object" ? (preparedData as Record<string,unknown>)[name] : undefined;
    const read=prepared instanceof DecodedBytes ? prepared : encoded===undefined ? undefined : readBytes(encoded);
    return [name,{encoded,read,lines:read?.text===undefined ? [] : linesOf(read.text)}];
  })),[data,preparedData]) as Record<string,{encoded?:string;read?:ReturnType<typeof readBytes>;lines:string[]}>;
  const [tab,setTab]=useState(()=>streams.stderr!.read?.size && Number(data.exitCode)!==0 ? "stderr" : "stdout");
  const [representation,setRepresentation]=useState<Record<string,string>>({});
  const [queries,setQueries]=useState<Record<string,string>>({});
  const [limits,setLimits]=useState<Record<string,number>>({});
  const [match,setMatch]=useState(0);
  const stream=streams[tab]!;
  const query=queries[tab] ?? "";
  const deferredQuery=useDeferredValue(query);
  const matches=useMemo(()=>deferredQuery ? stream.lines.flatMap((text,at)=>text.toLocaleLowerCase().includes(deferredQuery.toLocaleLowerCase()) ? [at] : []) : [],[stream.lines,deferredQuery]);
  const [download,setDownload]=useState<string>();
  const [saveProblem,setSaveProblem]=useState<string>();
  useEffect(()=>{
    setDownload(undefined);if(mode==="preview" || collapsed || stream.encoded===undefined || byteSize(stream.encoded)>max_decoded_body())return;
    try{const url=URL.createObjectURL(new Blob([Uint8Array.from(atob(stream.encoded),char=>char.charCodeAt(0))]));setDownload(url);return()=>URL.revokeObjectURL(url);}catch{return;}
  },[stream.encoded,mode,collapsed]);
  const hex=useMemo(()=>{if(stream.read?.text!==undefined && representation[tab]!=="hex" || stream.encoded===undefined || byteSize(stream.encoded)>max_decoded_body())return;try{return atob(stream.encoded);}catch{return;}},[stream.encoded,stream.read,representation,tab]);
  const limit=limits[tab] ?? 400;
  const move=(by:number)=>{
    if(!matches.length)return;
    const next=(match+by+matches.length)%matches.length, at=matches[next]!;
    setMatch(next);setLimits(was=>({...was,[tab]:Math.max(limit,Math.ceil((at+1)/400)*400)}));
  };
  useEffect(()=>{const at=matches[match],line=at===undefined ? undefined : lineRefs.current.get(at),box=scroll.current;if(line && box){const target=line.getBoundingClientRect(),viewport=box.getBoundingClientRect();box.scrollTop+=target.top-viewport.top-(box.clientHeight-target.height)/2;}},[match,matches,limit,tab]);
  const facts=<div className="process-facts"><span className={Number(data.exitCode)===0 ? "mono-ok" : "mono-bad"}>exit {String(data.exitCode ?? "missing")}</span>{["stdout","stderr"].map(name=>{const s=streams[name]!;return <span key={name}> · {name} {s.read ? s.read.problem==="invalid base64" ? s.read.problem : s.read.size===0 ? "empty" : `${whole ? "" : "≥ "}${size(s.read.size)}${s.read.text!==undefined ? ` · ${whole ? "" : "≥ "}${s.lines.length} ${s.lines.length===1 ? "line" : "lines"}` : ` · ${s.read.problem}`}` : "not read"}</span>;})}</div>;
  if(collapsed)return <div className="process-output" ref={root}>{facts}</div>;
  if(mode==="preview") {
    const order=streams.stderr!.read?.size ? ["stderr","stdout"] : ["stdout","stderr"];
    const active=order.filter(name=>streams[name]!.read?.size);
    let remainingBudget=4;
    const tails:string[]=[];
    return <div className="process-output process-preview" ref={root}>{facts}<div className="process-preview-lines">{active.map((name,at)=>{
      const s=streams[name]!, budget=active.length>1 && at===0 ? 2 : remainingBudget;
      const drawn=previewLines(s.lines,Math.max(4,columns-14),budget);remainingBudget-=Math.min(budget,drawn.lines.reduce((n,line)=>n+Math.max(1,Math.ceil(width(line.text.replace(/\t/g,"        "),columns*budget)/Math.max(4,columns-14))),0));
      if(drawn.remaining)tails.push(`+${whole ? "" : "≥"}${drawn.remaining} ${name} lines`);
      if(drawn.cut)tails.push(`${name} line ${drawn.lines.at(-1)?.number} continues`);
      return <div className="process-preview-stream" key={name}><span className="process-label">{name}</span><div>{s.read?.text===undefined ? <p className="mono-faint">{s.read?.problem ?? "not read"}</p> : drawn.lines.map(line=><div className="process-line" key={line.number}><span className="process-number">{line.number}</span><span className="process-text">{line.text}</span></div>)}</div></div>;
    })}</div><div className="process-tail">{tails.join(" · ")}{!whole && " · partial read"}</div></div>;
  }
  const text=stream.read?.text!==undefined && representation[tab]!=="hex";
  const perRow=columns>=80 ? 16 : columns>=45 ? 8 : 4;
  return <div className="process-output process-full" ref={root}>
    {facts}
    <div className="process-toolbar"><div role="tablist" aria-label="Process streams">{["stdout","stderr"].map(name=><button role="tab" aria-selected={tab===name} className="cell-action" key={name} onClick={()=>{setTab(name);setMatch(0);}}>{name} · {streams[name]!.read?.text!==undefined ? `${streams[name]!.lines.length} ${streams[name]!.lines.length===1 ? "line" : "lines"}` : streams[name]!.read?.problem ?? "not read"}</button>)}</div><div className="process-representation">{stream.read?.text!==undefined && <button className="cell-action" aria-pressed={text} onClick={()=>setRepresentation(was=>({...was,[tab]:"text"}))}>text</button>}<button className="cell-action" aria-pressed={!text} onClick={()=>setRepresentation(was=>({...was,[tab]:"hex"}))}>hex</button></div></div>
    {text && (mode==="window" || stream.lines.length>20) && <div className="process-find"><input aria-label={`Find in ${tab}`} placeholder="find in stream" value={query} onChange={event=>{setQueries(was=>({...was,[tab]:event.target.value}));setMatch(0);}} onKeyDown={event=>{if(event.key==="Enter"&&!composing(event)){event.preventDefault();event.stopPropagation();move(event.shiftKey ? -1 : 1);}}}/><button className="cell-action" aria-label="Previous match" disabled={!matches.length} onClick={()=>move(-1)}>‹</button><button className="cell-action" aria-label="Next match" disabled={!matches.length} onClick={()=>move(1)}>›</button><span>{query ? `${matches.length ? match+1 : 0} of ${matches.length} matching lines${whole ? "" : " in the read part"}` : ""}</span></div>}
    <div ref={scroll} className="process-scroll" tabIndex={0} aria-label={`${tab} content`}>
      {stream.read===undefined ? <p className="mono-faint">{tab} not read</p> : stream.read.problem==="invalid base64" ? <p className="mono-warn">invalid base64</p> : stream.read.size===0 ? <p className="mono-faint">{tab} empty</p> : text ? stream.lines.slice(0,limit).map((line,at)=><div className={`process-line${matches[match]===at ? " process-current" : ""}`} key={at} ref={element=>{if(element)lineRefs.current.set(at,element);else lineRefs.current.delete(at);}}><span className="process-number">{at+1}</span><span className="process-text">{line}</span></div>) : hex===undefined ? <p className="mono-faint">{stream.read.problem}</p> : Array.from({length:Math.min(Math.ceil(hex.length/perRow),limit)},(_,at)=>{const part=hex.slice(at*perRow,(at+1)*perRow);return <div className="process-hex" key={at}><span className="process-number">{(at*perRow).toString(16).padStart(8,"0")}</span><span>{[...part].map(c=>c.charCodeAt(0).toString(16).padStart(2,"0")).join(" ")}</span><span className="mono-dim">{[...part].map(c=>/[ -~]/.test(c)?c:".").join("")}</span></div>;})}
    </div>
    {text && stream.lines.length>limit && <button className="cell-action" onClick={()=>setLimits(was=>({...was,[tab]:limit+400}))}>show 400 more lines · {stream.lines.length-limit} left</button>}
    {!text && hex && Math.ceil(hex.length/perRow)>limit && <button className="cell-action" onClick={()=>setLimits(was=>({...was,[tab]:limit+400}))}>show more bytes</button>}
    <footer className="process-byte-actions">{stream.encoded!==undefined && byteSize(stream.encoded)<=max_decoded_body() && <button className="cell-action" onClick={()=>void clipboard.copy(stream.encoded!)}>copy base64</button>}{download ? <a href={download} download={`${tab}.bin`}>save bytes…</a> : stream.encoded!==undefined && <button className="cell-action" onClick={()=>void saveBytes(stream.encoded!,`${tab}.bin`).catch(error=>setSaveProblem(String(error)))}>save bytes…</button>}{clipboard.notice && <span role="status" className="mono-dim">{clipboard.notice}</span>}{saveProblem && <span role="alert" className="mono-warn">{saveProblem}</span>}{!whole && <span className="mono-warn">partial read · sizes are lower bounds</span>}</footer>
  </div>;
}
