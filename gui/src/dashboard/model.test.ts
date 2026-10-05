import {expect,it} from 'vitest';
import {addDashboardMember,changeDashboardNode,newDashboard,readDashboard,removeDashboardMember,type DashboardNode} from './model';
import {exportDashboard,importDashboard,restoreDashboards,saveDashboard} from './storage';
const populated=()=>addDashboardMember(newDashboard(),{id:'source',node:'id1001',generation:'session-a',label:'$count'});
it('validates bounded complete trees without alias resolution, duplicate sources or unknown fields',()=>{
  const board=populated();expect(readDashboard(board)).toEqual(board);
  expect(readDashboard({...board,name:'$bad'})).toBeUndefined();expect(readDashboard({...board,command:':run'})).toBeUndefined();
  expect(readDashboard({...board,members:[...board.members,...board.members]})).toBeUndefined();
  expect(readDashboard({...board,layout:{kind:'column',id:'root',weight:1,children:[]}})).toBeUndefined();
  const leaf=board.layout.kind!=='member'?board.layout.children[0]!:board.layout;
  expect(readDashboard({...board,layout:{kind:'row',id:'root',weight:1,children:[leaf,{...leaf,id:'other'}]}})).toBeUndefined();
  expect(readDashboard({...board,layout:{kind:'column',id:'root',weight:Infinity,children:[leaf]}})).toBeUndefined();
  let nested:DashboardNode=leaf;for(let i=0;i<10;i++)nested={kind:'column',id:`group-${i}`,weight:1,children:[nested]};
  expect(readDashboard({...board,layout:nested})).toBeUndefined();
});
it('keeps exactly one member reference through add/remove and refuses an invalid insertion target',()=>{
  const board=populated();expect(addDashboardMember(board,{...board.members[0]!,id:'duplicate'})).toBe(board);
  expect(addDashboardMember(board,{id:'b',node:'id1002',generation:'session-a',label:'$other'},'missing')).toBe(board);
  expect(readDashboard(removeDashboardMember(board,'source'))?.members).toEqual([]);
  const modified={...board,layout:changeDashboardNode(board.layout,board.layout.id,node=>({...node,weight:2}))};expect(readDashboard(modified)).toEqual(modified);
});
it('scopes saves, rejects stale revisions and name collisions, and exports no result data',()=>{
  const board=populated(),saved=saveDashboard([],'work',board,[]),committed=saved[0]!.board;
  expect(committed.revision).toBe(1);expect(()=>saveDashboard(saved,'work',board,[])).toThrow('changed');
  expect(()=>saveDashboard(saved,'other',committed,[])).toThrow('changed');
  expect(()=>saveDashboard(saved,'work',{...newDashboard(),name:board.name},[])).toThrow('already exists');
  expect(()=>saveDashboard([],'work',board,[board.name])).toThrow('already exists');
  expect(restoreDashboards(JSON.parse(JSON.stringify(saved)))).toEqual(saved);
  expect(restoreDashboards([...saved,...saved])).toEqual([]);
  const imported=importDashboard(exportDashboard(committed));expect(imported.id).not.toBe(committed.id);expect(imported.revision).toBe(0);expect(imported.members).toEqual(committed.members);
  expect(()=>importDashboard('{"version":1,"command":"run"}')).toThrow('Invalid');
  expect(()=>importDashboard(' '.repeat(128*1024+1))).toThrow('limit');
});
