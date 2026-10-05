import {defineView,numericText} from "@wes/view-sdk";
import {definition} from "./contract";
import "./view.css";
export default defineView(definition,{Component:({input})=>{
  const status=(input.status??"").toLowerCase();
  const role=["healthy","ok","ready","success"].includes(status)?"status-ok":["degraded","slow","warning","warn"].includes(status)?"status-warn":["failed","error","critical","down"].includes(status)?"status-bad":"table-value";
  return <section className="metric" aria-label={input.label??"Metric"}>
    <span className="screen-label">{input.label??"Value"}</span>
    <div><strong className={role}>{numericText(input.value)}</strong>{input.unit&&<span className="table-value metric-unit">{input.unit}</span>}</div>
    {input.status&&<span className={role}>{input.status}</span>}
  </section>;
}});
