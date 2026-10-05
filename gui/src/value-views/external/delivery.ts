import {stringifyExactJson} from "../../exact-json";
import authoring from "@wes/view-sdk/authoring.json";
/** One replaceable input, one acknowledged render in flight. Serialization happens at dispatch. */
export class LatestDelivery {
  private pending?:()=>unknown;
  private timer?:ReturnType<typeof setTimeout>;
  private deadline?:ReturnType<typeof setTimeout>;
  private flight=0;private sequence=0;private closed=false;
  constructor(private readonly send:(text:string)=>void,private readonly fail:(message:string)=>void,private readonly period=1000/authoring.runtimeLimits.drawingsPerSecond){}
  update(snapshot:()=>unknown){if(this.closed)return;this.pending=snapshot;this.schedule();}
  ack(sequence:number){if(!this.flight||sequence!==this.flight)return;clearTimeout(this.deadline);this.flight=0;this.schedule();}
  private schedule(){if(this.closed||this.timer!==undefined||this.flight||!this.pending)return;this.timer=setTimeout(()=>{this.timer=undefined;this.flush();},this.period);}
  private flush(){
    if(this.closed||this.flight||!this.pending)return;
    const snapshot=this.pending;this.pending=undefined;
    try{
      const sequence=++this.sequence,text=stringifyExactJson({kind:"render",sequence,...snapshot() as object});
      if(new TextEncoder().encode(text).length>authoring.runtimeLimits.inputBytes)throw new Error("View input exceeds the 1 MiB drawing budget. Bind a smaller projection or stream window.");
      this.flight=sequence;this.send(text);
      this.deadline=setTimeout(()=>this.stop("View did not acknowledge drawing within 5 seconds. Close and reopen to retry."),authoring.runtimeLimits.drawAckSeconds*1000);
    }catch(error){this.stop(error instanceof Error?error.message:"View delivery failed.");}
  }
  private stop(problem:string){this.close();this.fail(problem);}
  close(){this.closed=true;this.pending=undefined;clearTimeout(this.timer);clearTimeout(this.deadline);}
}
