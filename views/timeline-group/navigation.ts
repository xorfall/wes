import {range} from "@wes/view-sdk";
import type {Input,State,Event} from "./contract";
export const initial=(input:Pick<Input,"range">):State=>({viewport:range(input.range)!,cursor:null,selection:null,draft:null,selectedItem:null});
export const reduce=(state:State,event:Event):State=>{
  switch(event.kind){
    case "cursor":return {...state,cursor:event.at};
    case "viewport":return {...state,viewport:event.range};
    case "selection":return {...state,selection:event.range,draft:null};
    case "selection-preview":return {...state,draft:event.range};
    case "item":return {...state,selectedItem:event.item,cursor:event.item?.at??state.cursor};
  }
};
export const outputs=(state:State)=>({cursor:state.cursor,selection:state.selection?`${state.selection.start}/${state.selection.end}`:null,selectedItem:state.selectedItem});
export const eventOutputs=(_previous:State,_next:State,event:Event)=>event.kind==="item"&&event.item?[{port:"picked" as const,value:event.item}]:[];
export const displayViewport=(state:State)=>state.viewport;
