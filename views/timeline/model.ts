import {drawingNumber,nanos,range,type TimeRange} from "@wes/view-sdk";
import type {Input} from "./contract";
export const TIMELINE_LIMITS={members:8,records:20_000,series:8,detailRows:200,text:2048} as const;
export interface Sample {readonly id:string;readonly at:string;readonly t:bigint;readonly value:number|null}
export interface EventRecord {readonly id:string;readonly at:string;readonly t:bigint;readonly label:string;readonly detail:string}
export interface Series {readonly id:string;readonly label:string;readonly unit:string;readonly samples:readonly Sample[]}
export interface TimelineModel {readonly id:string;readonly title:string;readonly range:TimeRange;readonly coverage:TimeRange;readonly series:readonly Series[];readonly events:readonly EventRecord[];readonly omitted:number;readonly sourceError:string;readonly preview:boolean}
function check(condition:unknown,message:string):asserts condition {if(!condition)throw new Error(message);}
function unique(items:readonly {id:string}[]){check(new Set(items.map(i=>i.id)).size===items.length,"Timeline IDs must be unique");}
function sorted(items:readonly {t:bigint}[]){for(let i=1;i<items.length;i++)check(items[i-1]!.t<=items[i]!.t,"Timeline records must be sorted by Instant");}
export function prepareTimeline(input:Input,preview=false,identity?:string|null):TimelineModel {
  const extent=range(input.range),coverage=range(input.coverage);
  check(extent&&coverage,"Expected Interval");
  const start=nanos(extent.start)!,end=nanos(extent.end)!;
  check(nanos(coverage.start)!>=start&&nanos(coverage.end)!<=end,"Coverage must lie inside the query range");
  check(input.series.length<=8,"Timeline series limit exceeded");
  check(input.events.length+input.series.reduce((n,s)=>n+s.samples.length,0)<=TIMELINE_LIMITS.records,"Timeline record limit exceeded");
  const omitted=drawingNumber(input.omitted);check(omitted!==undefined&&Number.isSafeInteger(omitted)&&omitted>=0,"Invalid omission count");
  const series=input.series.map(s=>{
    const samples=s.samples.map(p=>{
      const t=nanos(p.at),value=drawingNumber(p.value);
      check(t!==undefined&&value!==undefined,"Sample cannot be drawn accurately; explicitly scale numeric values before drawing");
      check(t>=start&&t<end,"Sample outside query range");
      return {id:p.id,at:p.at,t,value:p.gap?null:value};
    });
    sorted(samples);unique(samples);return {id:s.id,label:s.label,unit:s.unit,samples};
  });
  unique(series);check(new Set(series.map(s=>s.unit)).size<=1,"Series on one timeline must use the same unit");
  const events=input.events.map(e=>{const t=nanos(e.at);check(t!==undefined&&t>=start&&t<end,"Event outside query range");return {...e,t};});
  sorted(events);unique(events);
  return {id:identity??input.id,title:input.title,range:extent,coverage,series,events,omitted,sourceError:input.sourceError,preview};
}
export type ItemRef={readonly source:string;readonly series:string;readonly id:string;readonly at:string;readonly value:string};
export const sampleRef=(model:TimelineModel,series:Series,sample:Sample):ItemRef=>({source:model.id,series:series.id,id:sample.id,at:sample.at,value:String(sample.value)});
export const eventRef=(model:TimelineModel,event:EventRecord):ItemRef=>({source:model.id,series:"",id:event.id,at:event.at,value:JSON.stringify([event.label,event.detail])});
export const sameItem=(a:ItemRef|null,b:ItemRef)=>a!==null&&a.source===b.source&&a.series===b.series&&a.id===b.id&&a.at===b.at;
