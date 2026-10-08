/**
 * CI log records, as a developer reads them during an investigation: what the analysis committed,
 * one bounded page at a time, each record with its original log line, byte span, time, level, job
 * and step. The receipt above the records says how far the analysis got, in its own numbers.
 *
 * Records are read only through `context.datasets`, by the `/outputs` pointer of this View's input.
 * Nothing here selects, records, refreshes, resumes or asks anything of a provider.
 */
import {useEffect,useRef,useState} from "react";
import {ViewDatasetError,type DatasetPage,type DatasetPosition,type ViewDatasets} from "@wes/view-sdk";
import {byteSpan,clock,endText,grouped,pageRange,PAGE_ROWS,previousFrom,PROBLEMS,receiptLines,scope,
  type CiLogLine,type CiRecordsInput,type Mode,type ReadProblem} from "./model";

/** Below this many columns the time and byte columns give way to the text. */
const NARROW_COLUMNS=72;

type Read={kind:"reading";shown?:DatasetPage<CiLogLine>}|{kind:"shown";page:DatasetPage<CiLogLine>}|{kind:"failed";problem:ReadProblem;shown?:DatasetPage<CiLogLine>};

export interface RecordsContext {readonly mode:Mode;readonly allocation?:{readonly columns:number};readonly datasets?:ViewDatasets}

export function CiRecordsView({input,context}:{readonly input:CiRecordsInput;readonly context:RecordsContext}) {
  const reads=context.datasets;
  const limit=PAGE_ROWS[context.mode];
  const reference=input.outputs.reference;
  // A new snapshot is a new set of records: reading starts again at its first record.
  const snapshot=`${reference.dataset}/${reference.generation}/${reference.records}`;
  const [position,setPosition]=useState<{snapshot:string;at:DatasetPosition}>({snapshot,at:{from:"0"}});
  const at=position.snapshot===snapshot?position.at:{from:"0"};
  const [attempt,setAttempt]=useState(0);
  const [read,setRead]=useState<Read>({kind:"reading"});
  const list=useRef<HTMLOListElement|null>(null);
  const atKey=JSON.stringify(at);

  useEffect(()=>{
    if(!reads){setRead({kind:"failed",problem:"unavailable"});return;}
    let live=true;
    setRead(was=>({kind:"reading",...(was.kind==="shown"?{shown:was.page}:was.shown?{shown:was.shown}:{})}));
    reads.page<CiLogLine>("/outputs",at,limit).then(page=>{
      if(!live)return;
      setRead({kind:"shown",page});
      // The reader stays on the control they used; only the record list returns to its top.
      if(list.current)list.current.scrollTop=0;
    },(error:unknown)=>{
      if(!live)return;
      const problem:ReadProblem=error instanceof ViewDatasetError?error.code:"failed";
      // Withdrawn records leave nothing behind; other refusals keep the last page, labelled.
      setRead(was=>({kind:"failed",problem,...(problem!=="withdrawn"&&problem!=="changed"&&was.kind!=="failed"&&(was.kind==="shown"?was.page:was.shown)
        ?{shown:was.kind==="shown"?was.page:was.shown!}:{})}));
    });
    return ()=>{live=false;};
  // eslint-disable-next-line react-hooks/exhaustive-deps
  },[reads,snapshot,atKey,limit,attempt]);

  const page=read.kind==="shown"?read.page:read.shown;
  const stale=read.kind!=="shown"&&page!==undefined;
  const narrow=(context.allocation?.columns??120)<NARROW_COLUMNS;
  const go=(next:DatasetPosition)=>setPosition({snapshot,at:next});
  const problem=read.kind==="failed"?PROBLEMS[read.problem]:undefined;

  return <section className="ci-records" data-mode={context.mode} aria-label="CI log records">
    <header className="ci-records-head">
      <h3 className="screen-title ci-records-title">CI log records</h3>
      <p className="screen-label ci-records-snapshot">snapshot generation {grouped(reference.generation)} · {grouped(reference.records)} committed records</p>
    </header>
    <div className="ci-records-receipt" aria-label="Analysis receipt">
      {receiptLines(input.receipt).map((line,index)=><p key={index} className={`screen-label${line.tone?` status-${line.tone}`:""}`}>{line.text}</p>)}
      <p className="screen-label">last scan state · {input.state.job} · {input.state.step}</p>
    </div>
    <div className="ci-records-toolbar" role="toolbar" aria-label="Record pages">
      <button type="button" disabled={!page||page.first==="0"||read.kind==="reading"} onClick={()=>page&&go({from:previousFrom(page.first,limit)})}>‹ previous</button>
      <button type="button" disabled={!page||page.cursor===null||read.kind==="reading"} onClick={()=>page?.cursor&&go({cursor:page.cursor})}>next ›</button>
      <span className="screen-label ci-records-range">{page?pageRange(page):"no page read"}</span>
    </div>
    <p className={`ci-records-status screen-label${problem?" status-warn":""}`} role="status">
      {problem?`${problem.text}${stale?" · previous page shown":""}`:read.kind==="reading"?(stale?"reading · previous page shown":"reading…"):(page&&endText(page))??""}
      {problem?.retry&&<button type="button" className="ci-records-again" onClick={()=>setAttempt(n=>n+1)}>read again</button>}
    </p>
    {read.kind==="failed"&&read.problem==="withdrawn"?null:
    <ol ref={list} className={`ci-records-rows${narrow?" ci-records-narrow":""}${stale?" ci-records-stale":""}`} style={{["--ci-records-rows" as string]:limit}} aria-label="Records" aria-busy={read.kind==="reading"}>
      {page?.rows.map(row=>{const line=row.value;const flagged=line.level==="error"||line.level==="warning";
        return <li key={row.ordinal} className={`ci-records-row${flagged?" status-bad":""}`}>
          <span className="ci-records-ordinal table-key" title="record ordinal">{grouped(row.ordinal)}</span>
          <span className="ci-records-line table-value" title="original log line">{grouped(line.ordinal)}</span>
          {!narrow&&<span className="ci-records-time table-time">{clock(line.time)}</span>}
          <span className="ci-records-level screen-label">{line.level??""}</span>
          <span className="ci-records-scope screen-label">{scope(line)}</span>
          {!narrow&&<span className="ci-records-bytes screen-label">{byteSpan(line)}</span>}
          <span className="ci-records-text table-value">{line.text}</span>
        </li>;})}
    </ol>}
  </section>;
}
