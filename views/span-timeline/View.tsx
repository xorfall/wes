import {useEffect,useState} from "react";
import {defineView,nanos,range} from "@wes/view-sdk";
import {definition,type Input} from "./contract";
import "./view.css";

type Status=Input["lanes"][number]["status"];
/** Colour comes from the declared status vocabulary, never from a label's text. */
const ROLE:Record<Status,string>={success:"status-ok",failure:"status-bad",cancelled:"status-warn",running:"table-value",skipped:"screen-label",unknown:"screen-label"};
const WORD:Record<Status,string>={success:"succeeded",failure:"failed",cancelled:"cancelled",running:"still running",skipped:"skipped",unknown:"status unknown"};

const SECOND=1_000_000_000n,MINUTE=60n*SECOND,HOUR=60n*MINUTE,DAY=24n*HOUR;
const STEPS=[...[1n,2n,5n,10n,15n,30n].map(s=>s*SECOND),...[1n,2n,5n,10n,15n,30n].map(m=>m*MINUTE),...[1n,2n,3n,6n,12n].map(h=>h*HOUR)];
const UNITS:[bigint,string][]=[[DAY,"d"],[HOUR,"h"],[MINUTE,"m"],[SECOND,"s"]];

function duration(ns:bigint):string {
  const s=Number(ns/1_000_000_000n),m=Math.floor(s/60);
  return m?`${m}m ${String(s%60).padStart(2,"0")}s`:`${s}s`;
}
function check(ok:boolean,message:string):asserts ok {if(!ok)throw new Error(message);}
/** The smallest step that divides the whole extent into at most eight parts: a fixed ladder up to
 *  half a day, then 1-2-5 multiples of a day, so any range gets at most nine ticks. */
function tickStep(span:bigint):bigint {
  const fit=STEPS.find(s=>s*8n>=span);
  if(fit!==undefined)return fit;
  const days=(span+8n*DAY-1n)/(8n*DAY);
  for(let p=1n;;p*=10n)for(const m of [1n,2n,5n])if(m*p>=days)return m*p*DAY;
}

/** Lanes of intervals over one range. A running span with no end runs to the range's end and says so; any
 *  other span without an end is marked at its start as having no recorded end, never drawn as a duration.
 *  A lane with a note and no spans says why nothing ran. Limits are reference markers with their source named. */
export function prepare(input:Input){
  const extent=range(input.range);check(extent!==undefined,"range must be an Interval");
  const start=nanos(extent.start)!,end=nanos(extent.end)!;check(end>start,"range must not be empty");
  const span=end-start;
  const at=(t:bigint)=>Math.max(0,Math.min(100,Number((t-start)*10000n/span)/100));
  const lanes=input.lanes.map(lane=>({
    ...lane,
    spans:lane.spans.map(s=>{
      const s0=nanos(s.start),s1=s.end===null?undefined:nanos(s.end);
      check(s0!==undefined,`span ${s.id} needs a start`);
      check(s1===undefined||s1>=s0,`span ${s.id} ends before it starts`);
      const open=s1===undefined&&s.status==="running",unended=s1===undefined&&!open;
      return {...s,left:at(s0),width:unended?0:Math.max(0,at(s1??end)-at(s0)),open,unended,length:s1===undefined?undefined:s1-s0};
    }),
    limits:lane.limits.map(l=>{const t=nanos(l.at)!;return {...l,left:at(t),inside:t>=start&&t<=end};}),
  }));
  const step=tickStep(span),[unit,suffix]=UNITS.find(([u])=>step%u===0n)??[SECOND,"s"];
  const ticks:{left:number;text:string}[]=[];
  for(let t=0n;t<=span;t+=step)ticks.push({left:Number(t*10000n/span)/100,text:`${t/unit}${suffix}`});
  return {title:input.title,lanes,ticks,total:duration(span)};
}
/** How a span without a length reads: still running, or an end that was never recorded. */
const unmeasured=(s:{open:boolean})=>s.open?"no end yet":"end not recorded";

const clock=(t:string|null)=>t===null?"—":/T(\d\d:\d\d:\d\d)/.exec(t)?.[1]??t;

export default defineView(definition,{Component:({input,context})=>{
  const model=prepare(input);
  /** The span being read. Local to this view: choosing it changes nothing outside. */
  const [chosen,setChosen]=useState<{lane:string;span:string}>();
  /** Choosing is idempotent and clearing is explicit (the clear control or Escape); focus outside the
   *  View keeps the choice and only reads it as inactive. */
  const active=context.active!==false;
  /** Escape is heard anywhere in this View's frame: WebKit does not focus a clicked button. */
  useEffect(()=>{
    if(!chosen||typeof window==="undefined")return;
    const key=(event:KeyboardEvent)=>{if(event.key==="Escape"){event.preventDefault();setChosen(undefined);}};
    window.addEventListener("keydown",key);return()=>window.removeEventListener("keydown",key);
  },[chosen]);
  const picked=chosen&&model.lanes.find(l=>l.id===chosen.lane)?.spans.find(s=>s.id===chosen.span);
  const lanes=context.mode==="preview"?model.lanes.slice(0,4):model.lanes;
  return <section className="span-timeline" aria-label={model.title} data-active={active}>
    <h3 className="span-timeline-title">{model.title} <small className="screen-label">· {model.total}</small></h3>
    {lanes.length===0?<p className="span-timeline-empty screen-label">No lanes.</p>:
    <div className="span-timeline-scroll"><div className="span-timeline-grid" role="list">
      <div className="span-timeline-axis" aria-hidden="true">{model.ticks.map(t=><span key={t.text}className="span-timeline-tick screen-label" style={{left:`${t.left}%`}}>{t.text}</span>)}</div>
      {lanes.map(lane=><div key={lane.id} className="span-timeline-lane" role="listitem">
        <div className="span-timeline-label"><span title={lane.label}>{lane.label}</span><small className={ROLE[lane.status]}>{WORD[lane.status]}</small></div>
        <div className="span-timeline-track">
          {lane.spans.map(s=><button type="button" key={s.id} className={`span-timeline-span ${ROLE[s.status]}`} data-status={s.status} {...(s.open?{"data-open":""}:{})} {...(s.unended?{"data-unended":""}:{})}
            style={{left:`${s.left}%`,width:`${s.width}%`}} {...(chosen?.lane===lane.id&&chosen.span===s.id?{"aria-current":"true" as const}:{})}
            onClick={()=>setChosen({lane:lane.id,span:s.id})}
            aria-label={`${s.label}: ${WORD[s.status]}, ${s.length!==undefined?duration(s.length):unmeasured(s)}`}
            title={`${s.label} · ${WORD[s.status]} · ${s.length!==undefined?duration(s.length):unmeasured(s)}`}/>)}
          {lane.limits.filter(l=>l.inside).map(l=><span key={l.label+l.left} className="span-timeline-limit status-bad" style={{left:`${l.left}%`}} title={`${l.label} (${l.source})`}><span>{l.label}</span></span>)}
          {lane.note!==null && <span className="span-timeline-note screen-label">{lane.note}</span>}
        </div>
      </div>)}
    </div></div>}
    {picked&&<p className="span-timeline-detail" role="status"><strong>{picked.label}</strong> · <span className={ROLE[picked.status]}>{WORD[picked.status]}</span> · {clock(picked.start)} → {picked.length===undefined?unmeasured(picked):clock(picked.end)}{picked.length!==undefined&&<> · {duration(picked.length)}</>}{" "}<button type="button" className="span-timeline-clear" aria-label="Clear the chosen span" title="Clear (Escape)" onClick={()=>setChosen(undefined)}>×</button></p>}
    {context.mode==="preview" && model.lanes.length>lanes.length && <small className="screen-label">+{model.lanes.length-lanes.length} lanes</small>}
  </section>;
}});
