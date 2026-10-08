import {useEffect,useRef} from "react";
import {defineView,numericText,type NumericValue} from "@wes/view-sdk";
import {definition,type Input} from "./contract";
import "./view.css";

type Row=Input["lines"][number];
export type Item={kind:"line";row:Row;focused:boolean}|{kind:"gap";count:number}|{kind:"scope";text:string};

function check(ok:boolean,message:string):asserts ok {if(!ok)throw new Error(message);}
const clock=(time:string|null)=>time===null?"":/T(\d\d:\d\d:\d\d)/.exec(time)?.[1]??"";
/** Ordinals arrive as numbers or exact lexemes; compare them as integers. */
const n=(value:NumericValue)=>BigInt(numericText(value));
const span=(from:NumericValue,to:NumericValue)=>n(from)===n(to)?`line ${numericText(from)}`:`lines ${numericText(from)}–${numericText(to)}`;
const WARN=new Set(["error","warning"]);
/** The frame has no definite height, so the line list carries its own bound in every tier. */
const TIER:Record<"preview"|"expanded"|"window",string>={preview:" log-excerpt-preview",expanded:"",window:" log-excerpt-window"};

/** Lines in ordinal order with the focus range marked. Skipped ordinals become a counted gap, a step
 *  change becomes a scope row, and focus lines the excerpt does not carry are counted, not invented. */
export function prepare(input:Input){
  const focus=input.focus;
  check(focus===null||n(focus.to)>=n(focus.from),"focus ends before it starts");
  const items:Item[]=[];
  let previous:Row|undefined;
  for(const row of input.lines){
    const at=n(row.ordinal),before=previous===undefined?undefined:n(previous.ordinal);
    check(before===undefined||at>before,`line ${numericText(row.ordinal)} is out of order or repeated`);
    if(before!==undefined&&at>before+1n)items.push({kind:"gap",count:Number(at-before-1n)});
    if(row.step!==null&&row.step!==previous?.step)items.push({kind:"scope",text:row.step});
    items.push({kind:"line",row,focused:focus!==null&&at>=n(focus.from)&&at<=n(focus.to)});
    previous=row;
  }
  const carried=items.filter(i=>i.kind==="line"&&i.focused).length;
  const missing=focus===null?0:Number(n(focus.to)-n(focus.from)+1n)-carried;
  return {items,missing};
}

export default defineView(definition,{Component:({input,context})=>{
  const model=prepare(input);
  const first=useRef<HTMLLIElement|null>(null),list=useRef<HTMLOListElement|null>(null);
  const target=input.focus===null?"":`${input.artifact.digest}:${numericText(input.focus.from)}-${numericText(input.focus.to)}`;
  /** Centre the focus inside this list only, and only when the focus moves: scrolling the element
   *  into view would also scroll the page that hosts this frame. */
  useEffect(()=>{const box=list.current,row=first.current;if(box&&row)box.scrollTop=Math.max(0,row.offsetTop-box.offsetTop-(box.clientHeight-row.offsetHeight)/2);},[target]);
  let marked=false;
  const a=input.artifact,focus=input.focus;
  return <section className="log-excerpt" aria-label={input.title}>
    <h3 className="log-excerpt-title">{input.title}</h3>
    <p className="log-excerpt-source screen-label">{a.origin} · {a.job} · run {a.run} attempt {numericText(a.attempt)}
      {focus!==null&&<> · focus {span(focus.from,focus.to)}</>}</p>
    {model.missing>0&&<p className="log-excerpt-missing status-warn" role="status">{model.missing} focus {model.missing===1?"line is":"lines are"} not in this excerpt</p>}
    {input.lines.length===0?<p className="log-excerpt-empty screen-label">No lines.</p>:
    <ol ref={list} className={`log-excerpt-lines${TIER[context.mode]}`}>
      {model.items.map((item,i)=>{
        if(item.kind==="gap")return <li key={`g${i}`} className="log-excerpt-gap screen-label">… {item.count} {item.count===1?"line":"lines"} not shown</li>;
        if(item.kind==="scope")return <li key={`s${i}`} className="log-excerpt-scope screen-label">{item.text}</li>;
        const r=item.row,ref=item.focused&&!marked?(marked=true,first):undefined;
        return <li key={numericText(r.ordinal)} ref={ref} className={`log-excerpt-line${item.focused?" log-excerpt-focus":""}${r.level!==null&&WARN.has(r.level)?" status-bad":""}`} {...(item.focused?{"aria-current":"true" as const}:{})}>
          <span className="log-excerpt-ordinal screen-label">{numericText(r.ordinal)}</span>
          <span className="log-excerpt-time screen-label">{clock(r.time)}</span>
          <span className="log-excerpt-text">{r.text}</span>
        </li>;
      })}
    </ol>}
  </section>;
}});
