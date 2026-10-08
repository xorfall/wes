import {defineView} from "@wes/view-sdk";
import {definition} from "./contract";
import {CiRecordsView} from "./CiRecordsView";
import "./view.css";

/** A static View: it reads committed records by page and emits nothing. */
export default defineView(definition,{Component:({input,context})=><CiRecordsView input={input} context={context}/>});
