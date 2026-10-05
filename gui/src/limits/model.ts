/** A Settings row combines the active runtime policy and the saved next-start policy. */
export interface LimitEntry {
  readonly id:string;readonly label:string;readonly description:string;readonly group:string;
  readonly unit:'count'|'bytes'|'milliseconds';readonly default:number;readonly active:number;readonly saved:number;
  readonly min:number;readonly max:number;readonly editable:boolean;readonly restart:boolean;readonly reason?:string;readonly source?:string;
}
