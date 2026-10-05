import {defineView,drawingNumber,numericText} from "@wes/view-sdk";
import {definition} from "./contract";
import "./view.css";
export default defineView(definition,{Component:({input})=>{
  const counts=input.bins.map(bin=>drawingNumber(bin.count));
  const drawable=counts.every((n):n is number=>n!==undefined);
  const max=drawable?Math.max(1,...counts):1;
  return <figure className="histogram">
    <figcaption className="screen-title">Distribution <span className="table-value">{numericText(input.total)} samples</span></figcaption>
    {!input.bins.length?<p className="screen-label">No samples to plot.</p>:drawable?<svg viewBox="0 0 1000 280" role="img" aria-label={`Distribution across ${input.bins.length} bins`}>
      {input.bins.map((bin,i)=><g key={i} role="img" aria-label={`${numericText(bin.lower)} – ${numericText(bin.upper)}: ${numericText(bin.count)} samples`}><rect x={40+i*920/input.bins.length+1} y={240-200*counts[i]!/max} width={Math.max(1,920/input.bins.length-2)} height={200*counts[i]!/max}/></g>)}
      <text x="40" y="268">{numericText(input.bins[0]!.lower)}</text><text x="960" y="268" textAnchor="end">{numericText(input.bins[input.bins.length-1]!.upper)}</text>
    </svg>:<p className="status-warn">Counts exceed the exact drawing range. Inspect the table below.</p>}
    <details><summary className="screen-label">Bin counts</summary><table><thead><tr><th>Lower</th><th>Upper</th><th>Count</th></tr></thead><tbody>{input.bins.map((bin,i)=><tr key={i}><td className="table-value">{numericText(bin.lower)}</td><td className="table-value">{numericText(bin.upper)}</td><td className="table-value">{numericText(bin.count)}</td></tr>)}</tbody></table></details>
  </figure>;
}});
