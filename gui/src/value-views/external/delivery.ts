import {stringifyExactJson} from "../../exact-json";
import authoring from "@wes/view-sdk/authoring.json";
/** Payload-independent reason a delivery stopped; never carries input or renderer data. */
export type DeliveryFailure="draw_timeout"|"delivery_failed";
/**
 * Optional lifecycle observer. `label` is read at dispatch together with the snapshot, kept with
 * that flight and reported back unchanged, so later updates never relabel an older delivery.
 */
export interface DeliveryObserver {
  label():string|null;
  sent(sequence:number,label:string|null):void;
  drawn(sequence:number,label:string|null):void;
}
/** One replaceable input, one acknowledged render in flight. Serialization happens at dispatch. */
export class LatestDelivery {
  private pending?:()=>unknown;
  private timer?:ReturnType<typeof setTimeout>;
  private deadline?:ReturnType<typeof setTimeout>;
  private flight=0;private flightLabel:string|null=null;private sequence=0;private closed=false;
  constructor(private readonly send:(text:string)=>void,private readonly fail:(message:string,reason:DeliveryFailure)=>void,private readonly period=1000/authoring.runtimeLimits.drawingsPerSecond,private readonly observer?:DeliveryObserver){}
  update(snapshot:()=>unknown){if(this.closed)return;this.pending=snapshot;this.schedule();}
  /** Only the acknowledgement of the current flight counts; bogus, late or post-close ones are ignored. */
  ack(sequence:number){
    if(this.closed||!this.flight||sequence!==this.flight)return;
    clearTimeout(this.deadline);this.flight=0;this.observer?.drawn(sequence,this.flightLabel);this.schedule();
  }
  private schedule(){if(this.closed||this.timer!==undefined||this.flight||!this.pending)return;this.timer=setTimeout(()=>{this.timer=undefined;this.flush();},this.period);}
  private flush(){
    if(this.closed||this.flight||!this.pending)return;
    const snapshot=this.pending;this.pending=undefined;
    try{
      const sequence=++this.sequence,label=this.observer?.label()??null,text=stringifyExactJson({kind:"render",sequence,...snapshot() as object});
      if(new TextEncoder().encode(text).length>authoring.runtimeLimits.inputBytes)throw new Error("View input exceeds the 1 MiB drawing budget. Bind a smaller projection or stream window.");
      this.flight=sequence;this.flightLabel=label;this.send(text);
      this.observer?.sent(sequence,label);
      this.deadline=setTimeout(()=>this.stop("View did not acknowledge drawing within 5 seconds. Close and reopen to retry.","draw_timeout"),authoring.runtimeLimits.drawAckSeconds*1000);
    }catch(error){this.stop(error instanceof Error?error.message:"View delivery failed.","delivery_failed");}
  }
  private stop(problem:string,reason:DeliveryFailure){this.close();this.fail(problem,reason);}
  close(){this.closed=true;this.pending=undefined;clearTimeout(this.timer);clearTimeout(this.deadline);}
}
