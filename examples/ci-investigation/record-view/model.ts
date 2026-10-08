/**
 * What the CiRecords View reads from a CI log analysis result, and how it says it.
 *
 * The input is the analysis's own ScanResult: its last scan state, its committed outputs as a
 * read-only Dataset of log records, and its receipt. Every count and position is shown exactly as
 * the engine wrote it; nothing here estimates, totals or invents a status.
 */
import {numericText,type DatasetPage,type DatasetRef,type NumericValue} from "@wes/view-sdk";

export interface CiArtifact {readonly run:string;readonly attempt:NumericValue;readonly job:string;readonly origin:string;readonly digest:string}
/** One committed log record, as the analysis wrote it. */
export interface CiLogLine {
  readonly artifact:CiArtifact;readonly ordinal:NumericValue;readonly job:string;readonly jobName:string|null;
  readonly step:string|null;readonly stepName:string|null;readonly time:string|null;readonly raw:string;readonly text:string;
  readonly level:string|null;readonly group:NumericValue|null;
  readonly byteStart:NumericValue;readonly byteEnd:NumericValue;readonly delimiterEnd:NumericValue;
}
export interface CiLogState {readonly group:NumericValue|null;readonly job:string;readonly step:string}
/** The receipt fields this View reads; the engine's receipt carries more. */
export interface CiRecordsReceipt {
  readonly status:string;readonly position:NumericValue;readonly readPosition:NumericValue;readonly extent:NumericValue;
  readonly positionUnit:string;readonly inputRecords:NumericValue;readonly outputRecords:NumericValue;readonly finishApplied:boolean;
  readonly sourceComplete:boolean|null;readonly failureCode:string|null;readonly failureMessage:string|null;readonly exhausted:string|null;
}
export interface CiRecordsInput {readonly state:CiLogState;readonly outputs:DatasetRef<CiLogLine>;readonly receipt:CiRecordsReceipt}

export type Mode="preview"|"expanded"|"window";
/** Rows one page asks for per tier; the row area shows exactly this many lines before it scrolls. */
export const PAGE_ROWS:Readonly<Record<Mode,number>>={preview:5,expanded:20,window:50};
const DASH="–";

/** `12 345 678`: exact decimal grouping that never passes through a float. */
export const grouped=(value:NumericValue|string)=>(typeof value==="string"?value:numericText(value)).replace(/\B(?=(\d{3})+$)/g," ");

/** `15:01:20.268` from the record's own timestamp, or nothing when the record has none. */
export function clock(time:string|null):string {
  if(time===null)return "";
  return /T(\d\d:\d\d:\d\d(?:\.\d{1,3})?)/.exec(time)?.[1]??"";
}

/** Where the previous page starts: one page back, never below zero. Exact at any size. */
export function previousFrom(first:string,limit:number):string {
  const start=BigInt(first)-BigInt(limit);
  return (start<0n?0n:start).toString();
}

/** `bytes 1 024–1 088`: the record's original span in the log. */
export const byteSpan=(line:CiLogLine)=>`${grouped(line.byteStart)}${DASH}${grouped(line.byteEnd)}`;

/** The job and step a record belongs to, by the names the record itself carries. */
export function scope(line:CiLogLine):string {
  const job=line.jobName??line.job,step=line.stepName??line.step;
  return step===null?job:`${job} · ${step}`;
}

/** What the receipt says about the run of the analysis, line by line, in its own words and numbers. */
export function receiptLines(receipt:CiRecordsReceipt):{text:string;tone?:"warn"|"bad"}[] {
  const lines:{text:string;tone?:"warn"|"bad"}[]=[];
  lines.push({text:`${receipt.status} · committed through ${grouped(receipt.position)} of ${grouped(receipt.extent)} ${receipt.positionUnit} · read through ${grouped(receipt.readPosition)}`});
  lines.push({text:`${grouped(receipt.inputRecords)} input records · ${grouped(receipt.outputRecords)} output records${receipt.finishApplied?" · finish applied":""}`});
  lines.push({text:receipt.sourceComplete===null?"source completion unknown":receipt.sourceComplete?"source complete":"source incomplete",
    ...(receipt.sourceComplete===false?{tone:"warn" as const}:{})});
  if(receipt.exhausted!==null)lines.push({text:`stopped at its ${receipt.exhausted} bound`,tone:"warn"});
  if(receipt.failureCode!==null||receipt.failureMessage!==null)
    lines.push({text:[receipt.failureCode,receipt.failureMessage].filter(part=>part!==null).join(" · "),tone:"bad"});
  return lines;
}

/** `records 40–59 of 1 234`, with the records before and after the page said exactly. */
export function pageRange(page:DatasetPage<CiLogLine>):string {
  if(page.rows.length===0)return page.records==="0"?"no committed records":`no records from ${grouped(page.first)}`;
  const last=page.rows[page.rows.length-1]!.ordinal;
  const after=BigInt(page.records)-BigInt(page.next);
  return `records ${grouped(page.first)}${DASH}${grouped(last)} of ${grouped(page.records)}`
    +(page.first!=="0"?` · ${grouped(page.first)} before`:"")+(after>0n?` · ${grouped(after.toString())} after`:"");
}

/** The end of what is committed is not the end of the producer. */
export function endText(page:DatasetPage<CiLogLine>):string|undefined {
  if(!page.extentExhausted)return undefined;
  return page.lifecycle==="sealed"?"end of the records":`end of the committed records · ${page.lifecycle==="open"?"the analysis may still commit more":page.lifecycle}`;
}

export type ReadProblem="unavailable"|"busy"|"changed"|"withdrawn"|"limit"|"invalid"|"failed";
/** What a refused read means for the reader, and whether reading again is worth offering. */
export const PROBLEMS:Readonly<Record<ReadProblem,{text:string;retry:boolean}>>={
  unavailable:{text:"Records cannot be read here",retry:false},
  busy:{text:"Readers are busy",retry:true},
  changed:{text:"The input changed; reading its current records",retry:false},
  withdrawn:{text:"Access withdrawn",retry:false},
  limit:{text:"This page is too large to show",retry:true},
  invalid:{text:"These records could not be read as CI log records",retry:false},
  failed:{text:"Reading the records failed",retry:true},
};
