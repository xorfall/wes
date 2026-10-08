import {useEffect} from "react";
import {defineView,numericText} from "@wes/view-sdk";
import {definition,type Input} from "./contract";
import "./view.css";

type Failure=Input["failures"][number];
type Edge=Input["edges"][number];
type Kind=Failure["kind"];
type Selection={failure:string|null;evidence:Failure["evidence"]};

/** Tone comes from the declared failure kind, never from a title's text. */
const ROLE:Record<Kind,string>={test:"status-bad",build:"status-bad",dependency:"status-bad",invocation:"status-bad",infrastructure:"status-warn",timeout:"status-warn",cancellation:"status-warn",unknown:"screen-label"};
const VERB:Record<Edge["kind"],string>={wraps:"wrapped by",reports:"reported by",caused_by:"causes",blocked_by:"blocks"};
const PREVIEW_ORIGINS=3;

function check(ok:boolean,message:string):asserts ok {if(!ok)throw new Error(message);}

export interface Link {failure:Failure;edge:Edge;depth:number}

/** Each originating failure with the failures that carry it outward, following edges from the inner
 *  failure (`to`) to the one that wraps, reports or is caused by it (`from`). Failures no chain reaches
 *  are kept apart rather than dropped. Unknown or repeated ids fail instead of being guessed at. */
export function prepare(input:Input){
  const byId=new Map<string,Failure>();
  for(const f of input.failures){check(!byId.has(f.id),`failure ${f.id} is listed twice`);byId.set(f.id,f);}
  for(const e of input.edges)check(byId.has(e.from)&&byId.has(e.to),`edge ${e.from} → ${e.to} names an unknown failure`);
  for(const id of input.originating)check(byId.has(id),`originating failure ${id} is unknown`);
  const outward=(id:string)=>input.edges.filter(e=>e.to===id);
  const reached=new Set<string>();
  const origins=input.originating.map(id=>{
    const seen=new Set([id]),chain:Link[]=[];
    const walk=(at:string,depth:number)=>{for(const edge of outward(at)){if(seen.has(edge.from))continue;seen.add(edge.from);chain.push({failure:byId.get(edge.from)!,edge,depth});walk(edge.from,depth+1);}};
    walk(id,0);
    seen.forEach(s=>reached.add(s));
    return {failure:byId.get(id)!,chain};
  });
  const kinds=new Map<Kind,number>();
  for(const o of origins)kinds.set(o.failure.kind,(kinds.get(o.failure.kind)??0)+1);
  return {title:input.title,origins,unlinked:input.failures.filter(f=>!reached.has(f.id)),kinds:[...kinds],coverage:input.coverage};
}

/** Line numbers arrive as numbers or exact lexemes; their text is what is compared and shown. */
const lines=(e:NonNullable<Failure["evidence"]>)=>{const from=numericText(e.from),to=numericText(e.to);return from===to?`line ${from}`:`lines ${from}–${to}`;};
/** Unread logs grouped by reason, so one reason shared by many jobs is said once. */
function unread(rows:Input["coverage"]["notRead"]):string {
  const byReason=new Map<string,string[]>();
  for(const r of rows)byReason.set(r.reason,[...(byReason.get(r.reason)??[]),r.job]);
  return [...byReason].map(([reason,jobs])=>`${jobs.length===1?jobs[0]:`${jobs.length} jobs`} (${reason})`).join("; ");
}
const big=(v:Parameters<typeof numericText>[0])=>BigInt(numericText(v));
/** The selected failure's lines, evidence marked. Only what the summary carries is shown: lines that
 *  were not retained, or that fell past the excerpt bound, are said rather than filled in. */
function Excerpt({failure,close}:{failure:Failure;close:()=>void}){
  const e=failure.evidence;
  if(e===null)return null;
  const from=big(e.from),to=big(e.to);
  if(failure.excerpt.length===0)return <p className="failure-summary-excerpt-missing screen-label" role="status">These lines were not retained with the summary. <button type="button" className="failure-summary-close" aria-label={`Close ${lines(e)}`} onClick={close}>×</button></p>;
  const last=big(failure.excerpt[failure.excerpt.length-1]!.ordinal);
  return <div className="failure-summary-excerpt">
    <button type="button" className="failure-summary-close" aria-label={`Close ${lines(e)}`} title="Close (Escape)" onClick={close}>×</button>
    <ol aria-label={`${lines(e)} of ${e.artifact.origin}`}>
      {failure.excerpt.map(l=>{const at=big(l.ordinal),inside=at>=from&&at<=to;
        return <li key={numericText(l.ordinal)} className={`${inside?"failure-summary-focus":""}${l.level==="error"||l.level==="warning"?" status-bad":""}`} {...(inside?{"aria-current":"true" as const}:{})}>
          <span className="failure-summary-ordinal screen-label">{numericText(l.ordinal)}</span><span className="failure-summary-text">{l.text}</span></li>;})}
    </ol>
    {last<to&&<p className="failure-summary-excerpt-missing status-warn" role="status">Lines {String(last+1n)}–{numericText(e.to)} are past the excerpt limit.</p>}
  </div>;
}
const where=(f:Failure)=>[f.job,f.step].filter(Boolean).join(" · ");
/** The title of a failure `prepare` already checked exists. */
const titleOf=(input:Input,id:string)=>input.failures.find(f=>f.id===id)?.title??id;

function Evidence({failure,selected,emit}:{failure:Failure;selected:boolean;emit:(s:Selection)=>void}){
  const e=failure.evidence;
  if(e===null)return <span className="failure-summary-evidence screen-label">no evidence lines</span>;
  return <button type="button" className="failure-summary-evidence" aria-pressed={selected}
    title={`${e.artifact.origin} · ${e.artifact.job} · attempt ${numericText(e.artifact.attempt)}`}
    onClick={()=>{if(!selected)emit({failure:failure.id,evidence:e});}}>{lines(e)}</button>;
}

export default defineView(definition,{
  initial:()=>({failure:null,evidence:null}),
  reduce:(_state,event)=>event,
  outputs:state=>({evidence:state.evidence}),
  eventOutputs:(_previous,next,_event)=>next.evidence?[{port:"picked" as const,value:next.evidence}]:[],
  Component:({input,state,emit,context})=>{
    const model=prepare(input);
    /** Choosing a range is idempotent and closing it is explicit (its close control or Escape); focus
     *  outside the View keeps the selection and only reads it as inactive. */
    const active=context.active!==false;
    const preview=context.mode==="preview";
    const clear=()=>emit({failure:null,evidence:null});
    const open=state.failure!==null;
    /** Escape is heard anywhere in this View's frame: WebKit does not focus a clicked button. */
    useEffect(()=>{
      if(!open||typeof window==="undefined")return;
      const key=(event:KeyboardEvent)=>{if(event.key==="Escape"){event.preventDefault();emit({failure:null,evidence:null});}};
      window.addEventListener("keydown",key);return()=>window.removeEventListener("keydown",key);
    },[open,emit]);
    const origins=preview?model.origins.slice(0,PREVIEW_ORIGINS):model.origins;
    const notRead=model.coverage.notRead;
    /** With no originating failure named, the findings are still shown — as unranked observations,
     *  never promoted to roots and never hidden behind a collapsed list. */
    const unranked=model.origins.length===0&&model.unlinked.length>0;
    const shownUnranked=preview?model.unlinked.slice(0,PREVIEW_ORIGINS):model.unlinked;
    const nothingRead=model.coverage.read.length===0;
    return <section className="failure-summary" aria-label={model.title} data-active={active}>
      <h3 className="failure-summary-title">{model.title}</h3>
      <p className="failure-summary-counts screen-label">
        {nothingRead&&input.failures.length===0?"No log was read, so no failure is identified or ruled out."
          :unranked?`${model.unlinked.length} ${model.unlinked.length===1?"observation":"observations"} · none is marked as originating`
          :input.failures.length===0?"No failure observation was recognized in what was read. This is not a verdict that nothing failed."
          :`${model.origins.length} originating`}
        {model.kinds.map(([kind,n])=><span key={kind}> · <span className={ROLE[kind]}>{n} {kind}</span></span>)}
      </p>
      {unranked&&<ol className="failure-summary-origins" aria-label="Observations not ranked as originating">
        {shownUnranked.map(f=>{const carried=input.edges.filter(e=>e.to===f.id);return <li key={f.id} className="failure-summary-origin" data-kind={f.kind}>
          <div className="failure-summary-head">
            <span className={`failure-summary-kind ${ROLE[f.kind]}`}>{f.kind}</span>
            <strong className="failure-summary-name">{f.title}</strong>
            <Evidence failure={f} selected={state.failure===f.id} emit={emit}/>
          </div>
          <div className="failure-summary-where screen-label">{where(f)}{f.location!==null&&<> · <code>{f.location}</code></>}{f.confidence!=="observed"&&<> · {f.confidence}</>}</div>
          {carried.map(edge=><div key={edge.from} className="failure-summary-where screen-label">← {VERB[edge.kind]}{edge.confidence!=="observed"&&` (${edge.confidence})`}: {titleOf(input,edge.from)}</div>)}
          {state.failure===f.id&&<Excerpt failure={f} close={clear}/>}
          {!preview&&f.detail!==""&&state.failure!==f.id&&<pre className="failure-summary-detail">{f.detail}</pre>}
        </li>;})}
      </ol>}
      {unranked&&preview&&model.unlinked.length>shownUnranked.length&&<small className="screen-label">+{model.unlinked.length-shownUnranked.length} observations</small>}
      <ol className="failure-summary-origins">
        {origins.map(({failure,chain})=><li key={failure.id} className="failure-summary-origin" data-kind={failure.kind}>
          <div className="failure-summary-head">
            <span className={`failure-summary-kind ${ROLE[failure.kind]}`}>{failure.kind}</span>
            <strong className="failure-summary-name">{failure.title}</strong>
            <Evidence failure={failure} selected={state.failure===failure.id} emit={emit}/>
          </div>
          <div className="failure-summary-where screen-label">{where(failure)}{failure.location!==null&&<> · <code>{failure.location}</code></>}{failure.confidence!=="observed"&&<> · {failure.confidence}</>}</div>
          {state.failure===failure.id&&<Excerpt failure={failure} close={clear}/>}
          {!preview&&failure.detail!==""&&state.failure!==failure.id&&<pre className="failure-summary-detail">{failure.detail}</pre>}
          {chain.length>0&&<ol className="failure-summary-chain" aria-label={`What carries ${failure.title}`}>
            {chain.map(({failure:outer,edge,depth})=><li key={outer.id} style={{paddingLeft:`${depth*1.25}em`}}>
              <span className="screen-label">← {VERB[edge.kind]}{edge.confidence!=="observed"&&` (${edge.confidence})`}:</span>{" "}
              <span className={ROLE[outer.kind]}>{outer.title}</span>
              {" "}<Evidence failure={outer} selected={state.failure===outer.id} emit={emit}/>
              {state.failure===outer.id&&<Excerpt failure={outer} close={clear}/>}
            </li>)}
          </ol>}
        </li>)}
      </ol>
      {preview&&model.origins.length>origins.length&&<small className="screen-label">+{model.origins.length-origins.length} originating</small>}
      {!preview&&!unranked&&model.unlinked.length>0&&<details className="failure-summary-unlinked">
        <summary>{model.unlinked.length} not linked to an originating failure</summary>
        <ul>{model.unlinked.map(f=><li key={f.id}><span className={ROLE[f.kind]}>{f.kind}</span> {f.title} <Evidence failure={f} selected={state.failure===f.id} emit={emit}/>{state.failure===f.id&&<Excerpt failure={f} close={clear}/>}</li>)}</ul>
      </details>}
      <p className="failure-summary-coverage screen-label" role="note">
        Read {model.coverage.read.length} log {model.coverage.read.length===1?"range":"ranges"}
        {model.coverage.read.some(r=>!r.complete)&&", some partial"}
        {notRead.length>0?<> · not read: {unread(notRead)}</>:" · nothing listed as unread"}
      </p>
      {/* What each read range covered, in its producer's words: often the caveat that matters. With
          nothing recognized it is the whole answer, so it is said even in preview. */}
      {(!preview||input.failures.length===0)&&[...new Set(model.coverage.read.map(r=>r.scope).filter(s=>s!==""))].map(scope=>
        <p key={scope} className="failure-summary-coverage screen-label">{scope}</p>)}
    </section>;
  },
});
