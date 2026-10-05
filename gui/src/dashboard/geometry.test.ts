import {expect,it} from 'vitest';
import {layoutDashboard} from './geometry';
import type {DashboardNode,DashboardSource} from './model';
const leaf=(id:string):Extract<DashboardNode,{kind:'member'}>=>({kind:'member',id,member:id,width:id==='b'?'preferred':'fill',align:'center',weight:1});
const group=(id:string,children:DashboardNode[],basis?:number):DashboardNode=>({kind:'column',id,weight:1,children,...(basis?{basis}:{})});
const pair=(id:string,a:string,b:string):DashboardNode=>({kind:'row',id,weight:1,children:[leaf(a),leaf(b)]});
const root:DashboardNode={kind:'row',id:'root',weight:1,children:[group('g1',[pair('r1','a','b'),leaf('c')],56),group('g2',[pair('r2','d','e'),leaf('f')],56)]};
const sources=new Map('abcdef'.split('').map(id=>[id,{id,node:id,generation:'session',label:id,width:'fill',align:'start',sizing:{min:{columns:id==='c'||id==='f'?30:20,rows:1},preferred:{columns:25,rows:8},max:{columns:100,rows:40}}} satisfies DashboardSource]));
const heights=new Map('abcdef'.split('').map(id=>[id,100]));
it('stacks outer groups before inner pairs using allocation width, retaining the same tree and leaf identities',()=>{
  const initial=JSON.stringify(root),wide=layoutDashboard(root,1280,sources,heights,7.8),middle=layoutDashboard(root,720,sources,heights,7.8),narrow=layoutDashboard(root,360,sources,heights,7.8);
  for(const [layout,outer,pair] of [[wide,false,false],[middle,true,false],[narrow,true,true]] as const){
    expect(layout.boxes.find(b=>b.id==='root')?.stacked).toBe(outer);expect(layout.boxes.find(b=>b.id==='r1')?.stacked).toBe(pair);
    expect(layout.boxes.filter(b=>b.member).map(b=>b.member)).toEqual(['a','b','c','d','e','f']);
  }
  expect(JSON.stringify(root)).toBe(initial);
  expect(wide.boxes.find(b=>b.member==='b')?.width).toBe(25*7.8+26);
  expect(middle.boxes.find(b=>b.member==='c')!.width).toBeGreaterThan(wide.boxes.find(b=>b.member==='c')!.width);
});
it('distributes weights as slot shares, checks each share against its minimum, caps frames and applies alignment',()=>{
  const node:DashboardNode={kind:'row',id:'weighted',weight:1,children:[leaf('a'),{...leaf('b'),weight:2,width:'fill',align:'end'}]};
  const result=layoutDashboard(node,1500,sources,heights,7.8);
  const a=result.boxes.find(b=>b.member==='a')!,b=result.boxes.find(b=>b.member==='b')!;
  expect(b.slotWidth/a.slotWidth).toBe(2);expect(b.width).toBe(806);expect(b.x+b.width).toBeCloseTo(1500);
  const tooNarrow=layoutDashboard(node,360,sources,heights,7.8);expect(tooNarrow.boxes.find(b=>b.id==='weighted')?.stacked).toBe(true);
  const subMinimum=layoutDashboard(leaf('c'),100,sources,heights,7.8);expect(subMinimum.boxes[0]!.width).toBeGreaterThan(100);
});
