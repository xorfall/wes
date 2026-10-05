import {defineView} from "@wes/view-sdk";
import {definition} from "./contract";
import "./view.css";
export default defineView(definition,{
  initial:input=>({value:input.options[0]?.value??""}),
  reduce:(_state,event)=>event,
  outputs:state=>({value:state.value}),
  Component:({input,state,emit})=>{
    if(new Set(input.options.map(o=>o.value)).size!==input.options.length)throw new Error("Choice requires distinct options");
    return <label className="choice"><span className="screen-label">{input.title}</span><select aria-label={input.title} value={state.value} onChange={event=>emit({value:event.target.value})}>
      {!input.options.some(o=>o.value===state.value)&&<option value={state.value} disabled>Selection unavailable</option>}
      {input.options.map(o=><option key={o.value} value={o.value}>{o.label}</option>)}
    </select></label>;
  },
});
