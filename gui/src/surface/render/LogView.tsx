import { useLayoutEffect, useMemo, useRef, useState } from "react";
import type { StoredValue } from "../../protocol";
import { useColumns } from "./measure";
import type { LogMapping } from "../../presentation/registry";
import { dockerIdentity, field, isLogValue, label, mappedIdentity, numericBigInt } from "./log-identity";

export { isLogValue };
export interface LogRow {key:string;sequence:string;time:string;received:boolean;exactTime?:string;stream:string;text:string;flags:string[];warn?:boolean;scope?:string;group?:string}
/**
 * Rows of a log. Docker's events read through their own declared shape; any other list reads only
 * through the mapping its presentation entry declares. Identity comes from the declared key fields,
 * never from row position or message text; a row missing its key or text is counted as unreadable.
 */
export function logRows(value:StoredValue,mapping?:LogMapping):LogRow[] {
  return readLog(value,mapping).rows;
}
/** The rows, and how many readable events repeated a key already read: a repeat is not unreadable. */
function readLog(value:StoredValue,mapping?:LogMapping):{rows:LogRow[];repeated:number} {
  if(mapping && !isLogValue(value))return mappedRows(value,mapping);
  if(!isLogValue(value))return {rows:[],repeated:0};
  const seen=new Set<string>();let repeated=0;
  const rows=(value.data as unknown[]).flatMap(raw=>{
    const identity=dockerIdentity(raw);if(!identity)return [];
    const row=raw as Record<string,unknown>,stamp=numericBigInt(row.timestamp_ns),received=numericBigInt(row.received_at_ns);
    if(typeof row.text!=="string" || typeof row.stream!=="string")return [];
    const key=identity.key;if(seen.has(key)){repeated++;return [];}seen.add(key);
    const ns=stamp??received;
    const ms=ns===undefined?undefined:Number(ns/1_000_000n);
    const date=ms===undefined?undefined:new Date(ms);
    const time=date && Number.isFinite(date.getTime()) ? date.toISOString().slice(11,23) : "—";
    return [{key,sequence:identity.sequence,time,received:stamp===undefined,...(ns!==undefined?{exactTime:`${ns.toString()} ns`}:{}),stream:row.stream,text:row.text,flags:["partial","lossy","line_truncated"].filter(flag=>row[flag]===true).map(flag=>flag==="line_truncated"?"truncated":flag)}];
  });
  return {rows,repeated};
}
const UNIT_NS={ns:1n,us:1_000n,ms:1_000_000n,s:1_000_000_000n} as const;
/** Clock time to the millisecond and the exact source text: an Instant as written, or an epoch count. */
function clock(value:unknown,unit:LogMapping["timeUnit"]):{time:string;exact?:string} {
  if(typeof value==="string"){const m=/T(\d\d:\d\d:\d\d)(\.\d{1,3})?/.exec(value);return m?{time:`${m[1]}${(m[2]??".000").padEnd(4,"0")}`,exact:value}:{time:"—",exact:value};}
  const count=numericBigInt(value);if(count===undefined)return {time:"—"};
  const ns=count*UNIT_NS[unit??"ns"],date=new Date(Number(ns/1_000_000n));
  return {time:Number.isFinite(date.getTime())?date.toISOString().slice(11,23):"—",exact:`${ns.toString()} ns`};
}
function mappedRows(value:StoredValue,mapping:LogMapping):{rows:LogRow[];repeated:number} {
  if(!Array.isArray(value.data))return {rows:[],repeated:0};
  const seen=new Set<string>();let repeated=0;
  const rows=(value.data as unknown[]).flatMap(raw=>{
    const identity=mappedIdentity(raw,mapping);if(!identity)return [];
    const row=raw as Record<string,unknown>,parts=identity.parts,text=field(row,mapping.text);
    if(typeof text!=="string")return [];
    const key=identity.key;if(seen.has(key)){repeated++;return [];}seen.add(key);
    const when=mapping.time===undefined?{time:"—"}:clock(field(row,mapping.time),mapping.timeUnit);
    const level=label(field(row,mapping.level)),stream=label(field(row,mapping.stream))??level??"";
    const scope=label(field(row,mapping.scope)),group=label(field(row,mapping.group));
    return [{key,sequence:parts[parts.length-1]!,time:when.time,received:false,...(when.exact?{exactTime:when.exact}:{}),stream,text,
      flags:mapping.flags.filter(flag=>field(row,flag)===true),warn:stream==="stderr" || level==="error" || level==="warning",...(scope!==undefined?{scope}:{}),...(group!==undefined?{group}:{})}];
  });
  return {rows,repeated};
}
/**
 * Folds of a mapped log. A group's first row stays as its handle; the rest hide when folded. A group
 * starts folded unless it holds a warning, so errors are never folded away by default; the reader's
 * own toggle wins. Filtering or finding reads through folds instead of hiding what matched.
 */
export function foldRows(rows:readonly LogRow[],toggled:ReadonlyMap<string,boolean>,open:boolean){
  const first=new Map<string,string>(),size=new Map<string,number>(),warn=new Set<string>();
  for(const row of rows){
    if(row.group===undefined)continue;
    if(!first.has(row.group))first.set(row.group,row.key);
    size.set(row.group,(size.get(row.group)??0)+1);
    if(row.warn)warn.add(row.group);
  }
  const folded=(group:string)=>!open && (toggled.get(group) ?? !warn.has(group));
  const shown=rows.filter(row=>row.group===undefined || first.get(row.group)===row.key || !folded(row.group));
  return {shown,first,size,folded,hidden:rows.length-shown.length};
}
type ScrollMode="follow"|"reading";
interface Anchor {key:string;offset:number}
const SCROLL_KEYS=new Set(["ArrowUp","ArrowDown","PageUp","PageDown","Home","End"," "]);
const UPWARD_KEYS=new Set(["ArrowUp","PageUp","Home"]);
/** How long a wheel, key, touch or pointer gesture claims the scroll events that follow it. */
const GESTURE_MS=250;
/** Distance from the end that still counts as the tail, absorbing sub-pixel rounding. */
const TAIL_SLACK_PX=2;
const EVICTED_NOTICE="The anchored row left this source window; showing the oldest available row.";
/**
 * Two reading positions over one live window. `follow` keeps the newest row in the log's own
 * scroller. `reading` keeps the row the reader chose at its pixel offset across samples, wrapping and
 * resizes. Only reader gestures (wheel, scrollbar, touch, scroll keys, find) leave follow or move the
 * anchor; scroll events this view or the layout caused are never recaptured. Reading is a position,
 * not a hold: the stream keeps sampling, and leaving the bounded window is said, never hidden.
 */
export function LogView({value,mode,identity,collapsed=false,mapping}:{value:StoredValue;mode:string;identity?:string;collapsed?:boolean;mapping?:LogMapping}) {
  const root=useRef<HTMLDivElement>(null),viewport=useRef<HTMLDivElement>(null),lines=useRef<HTMLDivElement>(null);
  const columns=useColumns(root),read=useMemo(()=>readLog(value,mapping),[value,mapping]),rows=read.rows;
  /** Docker's channels are known; a mapped log offers the channels its rows actually carry. */
  const channels=useMemo(()=>mapping && !isLogValue(value)?[...new Set(rows.map(row=>row.stream).filter(Boolean))]:["stdout","stderr","tty"],[rows,mapping,value]);
  const [filter,setFilter]=useState(""),[find,setFind]=useState(""),[channel,setChannel]=useState("all"),[match,setMatch]=useState(0);
  const [scrollMode,setScrollMode]=useState<ScrollMode>("follow"),[notice,setNotice]=useState<string>();
  const modeRef=useRef<ScrollMode>("follow"),anchor=useRef<Anchor>(),placed=useRef<number>(),gesture=useRef(Number.NEGATIVE_INFINITY),pointer=useRef(false),elements=useRef(new Map<string,HTMLDivElement>());
  const [toggled,setToggled]=useState<ReadonlyMap<string,boolean>>(new Map());
  const filtered=useMemo(()=>rows.filter(row=>(channel==="all" || channel===row.stream) && (!filter || row.text.toLocaleLowerCase().includes(filter.toLocaleLowerCase()))),[rows,filter,channel]);
  const folds=useMemo(()=>foldRows(filtered,toggled,Boolean(filter || find)),[filtered,toggled,filter,find]);
  const shown=folds.shown;
  const matches=useMemo(()=>find?shown.filter(row=>row.text.toLocaleLowerCase().includes(find.toLocaleLowerCase())):[],[shown,find]);
  const toggle=(group:string)=>{setToggled(previous=>new Map(previous).set(group,!folds.folded(group)));};
  const order=useMemo(()=>new Map(rows.map((row,at)=>[row.key,at])),[rows]);
  const unreadable=Array.isArray(value.data)?value.data.length-rows.length-read.repeated:0;
  const enter=(next:ScrollMode)=>{modeRef.current=next;setScrollMode(next);};
  /** Every position this view sets is remembered, so its own scroll event is not taken for the reader's. */
  const place=(box:HTMLDivElement,top:number)=>{box.scrollTop=top;placed.current=box.scrollTop;};
  const firstVisible=():Anchor|undefined=>{
    const box=viewport.current;if(!box)return;
    const top=box.getBoundingClientRect().top;
    for(const row of shown){const rect=elements.current.get(row.key)?.getBoundingClientRect();if(rect && rect.bottom>top)return {key:row.key,offset:rect.top-top};}
  };
  const toReading=()=>{enter("reading");anchor.current=firstVisible();};
  const toFollow=()=>{enter("follow");anchor.current=undefined;setNotice(undefined);const box=viewport.current;if(box)place(box,box.scrollHeight);};
  const restore=()=>{
    const box=viewport.current;if(!box)return;
    if(modeRef.current==="follow"){place(box,box.scrollHeight);return;}
    let at=anchor.current;if(!at)return;
    if(!elements.current.has(at.key)){
      const index=order.get(at.key);
      if(index===undefined){place(box,0);anchor.current=firstVisible();setNotice(EVICTED_NOTICE);return;}
      // Hidden by filter or stream, not evicted: keep the place at the next row still shown.
      const next=shown.find(row=>(order.get(row.key)??-1)>=index) ?? shown[shown.length-1];
      if(!next){anchor.current=undefined;return;}
      at=anchor.current={key:next.key,offset:at.offset};
    }
    const el=elements.current.get(at.key);
    if(el)place(box,box.scrollTop+el.getBoundingClientRect().top-box.getBoundingClientRect().top-at.offset);
  };
  const restoreLatest=useRef(restore);restoreLatest.current=restore;
  useLayoutEffect(()=>{anchor.current=undefined;placed.current=undefined;gesture.current=Number.NEGATIVE_INFINITY;enter("follow");setMatch(0);setNotice(undefined);setToggled(new Map());},[identity]);
  useLayoutEffect(()=>restore(),[shown,mode,columns,identity]);
  useLayoutEffect(()=>{
    const box=viewport.current;if(!box || typeof ResizeObserver==="undefined")return;
    const observer=new ResizeObserver(()=>restoreLatest.current());observer.observe(box);if(lines.current)observer.observe(lines.current);
    return()=>observer.disconnect();
  },[collapsed]);
  const scrollable=()=>{const box=viewport.current;return Boolean(box && box.scrollHeight>box.clientHeight);};
  const claim=(time:number)=>{gesture.current=Math.max(gesture.current,time+GESTURE_MS);};
  const onScroll=(event:{timeStamp:number})=>{
    const box=viewport.current;if(!box)return;
    const own=placed.current!==undefined && Math.abs(box.scrollTop-placed.current)<1;
    if(own || !(pointer.current || event.timeStamp<=gesture.current))return;
    placed.current=undefined;claim(event.timeStamp);
    if(modeRef.current==="follow"){if(box.scrollTop+box.clientHeight>=box.scrollHeight-TAIL_SLACK_PX)return;enter("reading");}
    anchor.current=firstVisible();
  };
  const release=(time:number)=>{pointer.current=false;claim(time);};
  const onPointerDown=(event:{button:number;timeStamp:number;currentTarget?:EventTarget|null})=>{
    if(event.button!==0)return;
    pointer.current=true;
    const doc=(event.currentTarget as HTMLElement|null|undefined)?.ownerDocument;
    if(doc){const up=(next:PointerEvent)=>{release(next.timeStamp);doc.removeEventListener("pointerup",up,true);doc.removeEventListener("pointercancel",up,true);};doc.addEventListener("pointerup",up,true);doc.addEventListener("pointercancel",up,true);}
  };
  /** Moving towards older rows starts reading before the scroll lands, so a sample cannot pull it back. */
  const leaveTail=()=>{if(modeRef.current==="follow" && scrollable())toReading();};
  const reveal=(key:string)=>{
    const box=viewport.current,el=elements.current.get(key);if(!box || !el)return;
    const top=el.getBoundingClientRect().top-box.getBoundingClientRect().top,bottom=el.getBoundingClientRect().bottom-box.getBoundingClientRect().top;
    if(top<0)place(box,box.scrollTop+top);else if(bottom>box.clientHeight)place(box,box.scrollTop+bottom-box.clientHeight);
    anchor.current={key,offset:el.getBoundingClientRect().top-box.getBoundingClientRect().top};
  };
  const seek=(direction:number)=>{if(!matches.length)return;const next=(match+direction+matches.length)%matches.length;setMatch(next);enter("reading");reveal(matches[next]!.key);};
  if(collapsed)return <div className="value-summary">Log · {rows.length} events</div>;
  const following=scrollMode==="follow";
  const modes=<div className="log-mode" role="group" aria-label="Log scrolling">
    <button type="button" className="cell-action" aria-pressed={following} title="Follow: keep the newest event in view" onClick={toFollow}>follow</button>
    <button type="button" className="cell-action" aria-pressed={!following} title="Reading: stay at the row being read while new events arrive. The source keeps running." onClick={()=>{if(modeRef.current!=="reading")toReading();}}>reading</button>
  </div>;
  return <div className="log-view" ref={root} data-mode={mode} data-scroll={scrollMode}>
    {mode==="preview" ? <div className="log-tools log-tools-compact">{modes}</div> : <div className="log-tools">
      <input aria-label="Filter log messages" placeholder="filter" value={filter} onChange={event=>setFilter(event.target.value)}/>
      {/* A chosen channel stays listed after its rows leave the window, so the select never shows `all` while filtering. */}
      {(channels.length>0 || channel!=="all") && <select aria-label="Log stream" value={channel} onChange={event=>setChannel(event.target.value)}>{[...new Set(["all",...channels,channel])].map(stream=><option key={stream}>{stream}</option>)}</select>}
      <input aria-label="Find in log" placeholder="find" value={find} onChange={event=>{setFind(event.target.value);setMatch(0);}}/>
      <span className="mono-dim log-match-count">{matches.length} matches</span>
      <button className="cell-action" aria-label="Previous log match" disabled={!matches.length} onClick={()=>seek(-1)}>↑</button><button className="cell-action" aria-label="Next log match" disabled={!matches.length} onClick={()=>seek(1)}>↓</button>
      {modes}
    </div>}
    {unreadable>0 && <p className="mono-warn log-notice" role="status">{unreadable} unreadable log {unreadable===1?"event":"events"}</p>}
    {read.repeated>0 && <p className="mono-warn log-notice" role="status">{read.repeated} log {read.repeated===1?"event repeats a key":"events repeat a key"} already shown</p>}
    {notice && <p className="mono-warn log-notice" role="status">{notice}</p>}
    <div className="log-scroll" ref={viewport} role="log" aria-live="off" aria-label="Log events" tabIndex={0}
      onPointerDown={onPointerDown} onPointerUp={event=>release(event.timeStamp)} onPointerCancel={event=>release(event.timeStamp)}
      onTouchStart={event=>{pointer.current=true;claim(event.timeStamp);}} onTouchEnd={event=>release(event.timeStamp)} onTouchCancel={event=>release(event.timeStamp)}
      onKeyDown={event=>{if(!SCROLL_KEYS.has(event.key))return;claim(event.timeStamp);if(UPWARD_KEYS.has(event.key) || (event.key===" " && event.shiftKey))leaveTail();}}
      onWheel={event=>{if(!event.deltaY)return;claim(event.timeStamp);if(event.deltaY<0)leaveTail();}} onScroll={onScroll}>
      <div className="log-lines" ref={lines}>
      {shown.length===0 && <p className="mono-dim">{rows.length?"No matching log events.":"No log events yet."}</p>}
      {shown.map((row,at)=>[row.scope!==undefined && row.scope!==shown[at-1]?.scope ? <div key={`scope:${row.key}`} className="log-scope mono-dim" role="separator">{row.scope}</div> : null,<div key={row.key} ref={el=>{if(el)elements.current.set(row.key,el);else elements.current.delete(row.key);}} className={`log-row${matches[match]?.key===row.key?" log-match":""}`} data-item-key={row.key}>
        {row.group!==undefined && folds.first.get(row.group)===row.key && (folds.size.get(row.group)??0)>1
          ? <button type="button" className="log-fold cell-action" aria-expanded={!folds.folded(row.group)} disabled={Boolean(filter || find)}
              title={filter || find?"Filter and find read through folds":`${folds.size.get(row.group)} lines in this group`} onClick={()=>toggle(row.group!)}>{folds.folded(row.group)?"▸":"▾"}</button>
          : mapping?.group!==undefined && <span className="log-fold" aria-hidden="true"/>}
        <span className="log-sequence mono-faint">{row.sequence}</span>
        {columns>=48 && <time className="mono-faint" title={row.exactTime===undefined?"time unavailable":`${row.exactTime} · ${row.received?"received time; source timestamp unavailable":"source timestamp"}`}>{row.time}{row.received && <sup>ʳ</sup>}</time>}
        <span className={row.warn || row.stream==="stderr"?"mono-warn":"mono-dim"} title={`sequence ${row.sequence}`}>{row.stream}</span><span className="log-message" dir="auto">{row.text}{row.group!==undefined && folds.first.get(row.group)===row.key && folds.folded(row.group) && (folds.size.get(row.group)??0)>1 && <span className="log-folded mono-faint"> · {(folds.size.get(row.group)??1)-1} more lines</span>}{row.flags.length>0 && <span className="log-flags mono-warn"> · {row.flags.join(" · ")}</span>}</span>
      </div>])}
      </div>
    </div>
    {shown.length!==rows.length && <div className="mono-dim log-counts">showing {shown.length} of {rows.length} events{folds.hidden>0 && ` · ${folds.hidden} folded`}</div>}
  </div>;
}
