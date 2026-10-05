import authoring from "@wes/view-sdk/authoring.json";
/** Drawing acknowledgments and geometry must not consume the interaction allowance. */
export class MessageRate {
  private readonly lanes=new Map<string,{tokens:number;last:number}>();
  take(kind:unknown,now:number){
    const key=kind==="event"?"interaction":"control",rate=authoring.runtimeLimits.eventsPerSecond;
    const lane=this.lanes.get(key)??{tokens:rate,last:now};
    lane.tokens=Math.min(rate,lane.tokens+Math.max(0,now-lane.last)*rate/1000);lane.last=now;
    this.lanes.set(key,lane);
    if(--lane.tokens<0)throw new Error("View message rate exceeded");
  }
}
