import {defineView,numericText} from "@wes/view-sdk";
import {definition} from "./contract";
export default defineView(definition,{Component:({input})=><p className="table-value">{numericText(input.milliseconds)} ms</p>});
