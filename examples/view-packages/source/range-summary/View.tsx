import {defineView} from "@wes/view-sdk";
import {definition} from "./contract";
export default defineView(definition,{Component:({input})=><section><strong className="screen-title">{input.title}</strong><p className="table-time">{input.selection??"No selected interval"}</p></section>});
