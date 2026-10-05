import type { DashboardNode, DashboardSource } from './model';
export interface DashboardBox { readonly id:string;readonly member?:string;readonly x:number;readonly y:number;readonly width:number;readonly height:number;readonly slotWidth:number;readonly stacked?:boolean }
/** Resolve width before height. Responsive reflow changes rectangles, never the stored tree. */
export function layoutDashboard(layout:DashboardNode,width:number,sources:ReadonlyMap<string,DashboardSource>,heights:ReadonlyMap<string,number>,advance:number,gap=12):{height:number;boxes:readonly DashboardBox[]} {
  const unit=Number.isFinite(advance)&&advance>0?advance:7.8;
  const available=Number.isFinite(width)&&width>0?width:780;
  const boxes:DashboardBox[]=[];
  const chrome=26, padding=12, heading=24;
  function minimum(node:DashboardNode,root=false):number {
    if(node.kind==='member')return (sources.get(node.member)?.sizing.min.columns??24)*unit+chrome;
    return Math.max(0,...node.children.map(child=>minimum(child)))+(root?0:padding*2);
  }
  function need(node:DashboardNode):number {return Math.max(minimum(node),(node.kind==='member'?0:node.basis??0)*unit);}
  function place(node:DashboardNode,x:number,y:number,slotWidth:number,root=false):number {
    if(node.kind==='member') {
      const source=sources.get(node.member),sizing=source?.sizing;
      const min=(sizing?.min.columns??24)*unit+chrome, max=(sizing?.max.columns??320)*unit+chrome;
      const policy=node.width==='auto'?source?.width??'fill':node.width;
      const preferred=(sizing?.preferred.columns??80)*unit+chrome;
      const rendered=Math.max(min,Math.min(slotWidth,max,policy==='preferred'?preferred:Infinity));
      const align=node.align==='auto'?source?.align??'start':node.align;
      const offset=Math.max(0,slotWidth-rendered)*(align==='end'?1:align==='center'?.5:0);
      const natural=heights.get(node.member),height=natural!==undefined&&Number.isFinite(natural)&&natural>0?Math.min(20000,natural):120;
      boxes.push({id:node.id,member:node.member,x:x+offset,y,width:rendered,height,slotWidth});return height;
    }
    const inset=root?0:padding, top=root?0:heading;
    const inner=Math.max(minimum(node,root)-inset*2,slotWidth-inset*2);
    const weight=node.children.reduce((sum,child)=>sum+child.weight,0),free=Math.max(0,inner-gap*Math.max(0,node.children.length-1));
    const stacked=node.kind==='row' && node.children.some(child=>free*child.weight/Math.max(1,weight)<need(child));
    let cursor=0,height=0;
    for(const child of node.children) {
      const childWidth=node.kind==='row'&&!stacked?free*child.weight/Math.max(1,weight):inner;
      const childHeight=place(child,x+inset+(node.kind==='row'&&!stacked?cursor:0),y+top+(node.kind==='column'||stacked?cursor:0),childWidth);
      if(node.kind==='row'&&!stacked){cursor+=childWidth+gap;height=Math.max(height,childHeight);}
      else{cursor+=childHeight+gap;height=cursor-gap;}
    }
    const total=height+top+(root?0:padding);
    boxes.push({id:node.id,x,y,width:Math.max(slotWidth,minimum(node,root)),height:total,slotWidth,stacked});return total;
  }
  const height=place(layout,0,0,Math.max(available,minimum(layout,true)),true);
  return {height,boxes};
}
