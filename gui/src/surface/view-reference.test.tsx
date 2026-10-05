import {Children,isValidElement,type ReactNode} from "react";
import {expect,it} from "vitest";
import type {Engine} from "../engine";
import type {StoredValue} from "../protocol";
import {emptyWorkspace,type WorkspaceNode} from "../workspace";
import {newCell} from "../cells";
import {cellBlocks} from "./cell-output";
import {readSession} from "./session-model";
import {ValueBlock,type ValueBlockProps} from "./render/ValueBlock";
function propsOf(content:ReactNode):ValueBlockProps|undefined {
  if(!isValidElement(content))return;
  if(content.type===ValueBlock)return content.props as ValueBlockProps;
  for(const child of Children.toArray((content.props as {children?:ReactNode}).children)){const props=propsOf(child);if(props)return props;}
}
it.each(["connect","disconnect","bind"])("shows %s results as references to the original alias without changing the stored handle",verb=>{
  const value:StoredValue={type:{kind:"meta",name:"ViewInstance"},data:{id:"original",instance:"identity",definition:"Dashboard"},provenance:{}};
  const base={dependsOn:[],state:"ready" as const,kept:false,provenance:{},cautions:[]};
  const original:WorkspaceNode={...base,id:"original",name:"overview",command:":view create Dashboard",handle:"original-handle"};
  const edit:WorkspaceNode={...base,id:"edit",name:"linkResult",command:`:view ${verb} $member to:$overview`,handle:"edit-handle"};
  const workspace={...emptyWorkspace,nodes:[original,edit]},held=new Map([["edit-handle",value]]);
  const cell=readSession({workspace,held,cells:[{...newCell(edit.command),state:"answered",nodes:["edit"]}],context:{workspace:"synthetic",connection:"connected"}}).cells[0]!;
  const blocks=cellBlocks({cell,workspace,held,reads:new Map(),retryRead(){},engine:{} as Engine,generation:"synthetic"});
  const props=propsOf(blocks[0]!.content)!;
  expect(props.instanceDisplay).toEqual({label:"$overview",referenceOnly:true});expect(props.value).toBe(value);expect(props.cacheKey).toBe("synthetic:edit-handle");
});
