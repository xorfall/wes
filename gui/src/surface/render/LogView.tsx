import { useLayoutEffect, useMemo, useRef, useState } from "react";
import type { StoredValue } from "../../protocol";
import { isNumeric, numericText } from "../../exact-json";
import { useColumns } from "./measure";

export function isLogValue(value:StoredValue):boolean {
  return value.type?.kind==="list" && value.type.element.kind==="record" && value.type.element.name==="DockerLogEvent" && Array.isArray(value.data);
}
function numericBigInt(value:unknown):bigint|undefined {
  if(!isNumeric(value))return;
  const text=numericText(value);if(!/^-?\d{1,30}$/.test(text))return;
  return BigInt(text);
}
export interface LogRow {key:string;sequence:string;time:string;received:boolean;exactTime?:string;stream:string;text:string;flags:string[]}
/** Identity comes from Docker's declared sequence, never from row position or message text. */
export function logRows(value:StoredValue):LogRow[] {
  if(!isLogValue(value))return [];
  const seen=new Set<string>();
  return (value.data as unknown[]).flatMap(raw=>{
    if(!raw || typeof raw!=="object")return [];
    const row=raw as Record<string,unknown>,sequence=numericBigInt(row.sequence),stamp=numericBigInt(row.timestamp_ns),received=numericBigInt(row.received_at_ns);
    if(sequence===undefined || typeof row.container!=="string" || typeof row.text!=="string" || typeof row.stream!=="string")return [];
    const key=JSON.stringify([row.container,sequence.toString()]);if(seen.has(key))return [];seen.add(key);
    const ns=stamp??received;
    const ms=ns===undefined?undefined:Number(ns/1_000_000n);
    const date=ms===undefined?undefined:new Date(ms);
    const time=date && Number.isFinite(date.getTime()) ? date.toISOString().slice(11,23) : "—";
    return [{key,sequence:sequence.toString(),time,received:stamp===undefined,exactTime:ns?.toString(),stream:row.stream,text:row.text,flags:["partial","lossy","line_truncated"].filter(flag=>row[flag]===true).map(flag=>flag==="line_truncated"?"truncated":flag)}];
  });
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
export function LogView({value,mode,identity,collapsed=false}:{value:StoredValue;mode:string;identity?:string;collapsed?:boolean}) {
  const root=useRef<HTMLDivElement>(null),viewport=useRef<HTMLDivElement>(null),lines=useRef<HTMLDivElement>(null);
  const columns=useColumns(root),rows=useMemo(()=>logRows(value),[value]);
  const [filter,setFilter]=useState(""),[find,setFind]=useState(""),[channel,setChannel]=useState("all"),[match,setMatch]=useState(0);
  const [scrollMode,setScrollMode]=useState<ScrollMode>("follow"),[notice,setNotice]=useState<string>();
  const modeRef=useRef<ScrollMode>("follow"),anchor=useRef<Anchor>(),placed=useRef<number>(),gesture=useRef(Number.NEGATIVE_INFINITY),pointer=useRef(false),elements=useRef(new Map<string,HTMLDivElement>());
  const shown=useMemo(()=>rows.filter(row=>(channel==="all" || channel===row.stream) && (!filter || row.text.toLocaleLowerCase().includes(filter.toLocaleLowerCase()))),[rows,filter,channel]);
  const matches=useMemo(()=>find?shown.filter(row=>row.text.toLocaleLowerCase().includes(find.toLocaleLowerCase())):[],[shown,find]);
  const order=useMemo(()=>new Map(rows.map((row,at)=>[row.key,at])),[rows]);
  const unreadable=Array.isArray(value.data)?value.data.length-rows.length:0;
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
  useLayoutEffect(()=>{anchor.current=undefined;placed.current=undefined;gesture.current=Number.NEGATIVE_INFINITY;enter("follow");setMatch(0);setNotice(undefined);},[identity]);
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
      <select aria-label="Log stream" value={channel} onChange={event=>setChannel(event.target.value)}>{["all","stdout","stderr","tty"].map(stream=><option key={stream}>{stream}</option>)}</select>
      <input aria-label="Find in log" placeholder="find" value={find} onChange={event=>{setFind(event.target.value);setMatch(0);}}/>
      <span className="mono-dim log-match-count">{matches.length} matches</span>
      <button className="cell-action" aria-label="Previous log match" disabled={!matches.length} onClick={()=>seek(-1)}>↑</button><button className="cell-action" aria-label="Next log match" disabled={!matches.length} onClick={()=>seek(1)}>↓</button>
      {modes}
    </div>}
    {unreadable>0 && <p className="mono-warn log-notice" role="status">{unreadable} unreadable log {unreadable===1?"event":"events"}</p>}
    {notice && <p className="mono-warn log-notice" role="status">{notice}</p>}
    <div className="log-scroll" ref={viewport} role="log" aria-live="off" aria-label="Log events" tabIndex={0}
      onPointerDown={onPointerDown} onPointerUp={event=>release(event.timeStamp)} onPointerCancel={event=>release(event.timeStamp)}
      onTouchStart={event=>{pointer.current=true;claim(event.timeStamp);}} onTouchEnd={event=>release(event.timeStamp)} onTouchCancel={event=>release(event.timeStamp)}
      onKeyDown={event=>{if(!SCROLL_KEYS.has(event.key))return;claim(event.timeStamp);if(UPWARD_KEYS.has(event.key) || (event.key===" " && event.shiftKey))leaveTail();}}
      onWheel={event=>{if(!event.deltaY)return;claim(event.timeStamp);if(event.deltaY<0)leaveTail();}} onScroll={onScroll}>
      <div className="log-lines" ref={lines}>
      {shown.length===0 && <p className="mono-dim">{rows.length?"No matching log events.":"No log events yet."}</p>}
      {shown.map(row=><div key={row.key} ref={el=>{if(el)elements.current.set(row.key,el);else elements.current.delete(row.key);}} className={`log-row${matches[match]?.key===row.key?" log-match":""}`} data-item-key={row.key}>
        <span className="log-sequence mono-faint">{row.sequence}</span>
        {columns>=48 && <time className="mono-faint" title={`${row.exactTime??"unavailable"} ns · ${row.received?"received time; source timestamp unavailable":"source timestamp"}`}>{row.time}{row.received && <sup>ʳ</sup>}</time>}
        <span className={row.stream==="stderr"?"mono-warn":"mono-dim"} title={`sequence ${row.sequence}`}>{row.stream}</span><span className="log-message" dir="auto">{row.text}{row.flags.length>0 && <span className="log-flags mono-warn"> · {row.flags.join(" · ")}</span>}</span>
      </div>)}
      </div>
    </div>
    {shown.length!==rows.length && <div className="mono-dim log-counts">showing {shown.length} of {rows.length} events</div>}
  </div>;
}
