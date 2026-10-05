import {expect,it} from 'vitest';
import {read} from '../surface/commands';
import {promptCompletion} from '../surface/prompt-complete';
import {emptyWorkspace} from '../workspace';
import {applyPaneCommand} from '../surface/pane-command';
import {oneP,SESSION_PANE} from '../surface/split-model';
import {openValueTab,paneTabs} from '../surface/workspace-tabs';
import {restoreSplit} from '../surface/split-storage';
it('opens the dashboard UI without dispatching engine commands and completes boards only at dashboard/value destinations',()=>{
  expect(read('/dashboard')).toEqual({kind:'screen',screen:'dashboard'});
  expect(read('/dashboard $overview split')).toEqual({kind:'screen',screen:'dashboard',node:'overview',inPane:true});
  for(const text of ['/dashboard overview','/dashboard $x extra'])expect(read(text).kind).toBe('trouble');
  const base={catalogue:emptyWorkspace.catalogue,aliases:{},names:['count'],variables:['count'],dashboards:['overview']};
  const completion=(line:string)=>promptCompletion({...base,line,caret:line.length}).items.map(item=>item.text);
  expect(completion('/dashboard $ov')).toEqual(['$overview']);expect(completion('/tab $ov')).toEqual(['$overview']);expect(completion('/split related $ov')).toEqual([]);expect(completion('/goto $ov')).toEqual([]);
});
it('uses the existing value tab/pane lifecycle for board descriptors and rejects related projections of UI layouts',()=>{
  const board={node:'board-id',generation:'ui-dashboard',label:'overview',dashboard:true as const};
  const tab=openValueTab(oneP(SESSION_PANE),'p1','work',board,true);
  expect(paneTabs(restoreSplit(JSON.parse(JSON.stringify(tab))).panes[0]!)).toHaveLength(2);
  expect(restoreSplit(tab).panes[0]!.value).toEqual(board);
  const split=applyPaneCommand(tab,'/split $overview','p1',shown=>shown,false,()=>board);
  expect(split.panes[1]!.value).toEqual(board);
  expect(()=>applyPaneCommand(tab,'/split related $overview','p1',shown=>shown,false,()=>board)).toThrow('UI layouts');
});
