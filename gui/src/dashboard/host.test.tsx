import {act,create,type ReactTestRenderer} from 'react-test-renderer';
import {useEffect,useState} from 'react';
import {afterEach,expect,it,vi} from 'vitest';
import type {Engine} from '../engine';
import type {StoredValue} from '../protocol';
import {apply,emptyWorkspace} from '../workspace';
import {DashboardHost} from './DashboardHost';
import {DashboardEditor} from './DashboardEditor';
import {DashboardCanvas} from './DashboardCanvas';
import {addDashboardMember,newDashboard,type Dashboard} from './model';

const lifetime=vi.hoisted(()=>({mounts:0,unmounts:0}));
vi.mock('../surface/render/ValueBlock',async original=>({...await original<typeof import('../surface/render/ValueBlock')>(),ValueBlock:({value}:{value:StoredValue})=>{
  const [picked,setPicked]=useState(0);
  useEffect(()=>{lifetime.mounts++;return()=>{lifetime.unmounts++;};},[]);
  return <button aria-label="Fixture view selection" onClick={()=>setPicked(picked+1)}>{String(value.data)}:{picked}</button>;
}}));
vi.mock('../surface/LiveView',()=>({LiveView:({cellHold}:{cellHold:boolean})=>{
  const [position,setPosition]=useState(0);
  useEffect(()=>{lifetime.mounts++;return()=>{lifetime.unmounts++;};},[]);
  return <button aria-label="Fixture live reading position" data-cell-hold={cellHold} onClick={()=>setPosition(position+1)}>{position}</button>;
}}));
let tree:ReactTestRenderer|undefined;
afterEach(()=>{if(tree)act(()=>tree!.unmount());tree=undefined;vi.unstubAllGlobals();lifetime.mounts=0;lifetime.unmounts=0;});
const member={id:'custom-import-id',node:'result-node',generation:'session-a',label:'$total'};
const board=():Dashboard=>({...addDashboardMember(newDashboard(),member),revision:1});
const workspace=()=>apply(apply(emptyWorkspace,{event:'created',dependencyLifetime:'continuous',node:'result-node',name:'total',command:':calc 4',dependsOn:[],interactive:false}),{event:'ready',node:'result-node',handle:'result-handle',type:'Text',bytes:1,provenance:{},kept:true,cautions:[]});
const engine=()=>({fetch:vi.fn(async()=>({type:{kind:'primitive',name:'Text'},data:'four',provenance:{}})),submit:vi.fn()}) as unknown as Engine;
const button=(label:string)=>tree!.root.findAllByType('button').find(node=>node.children.join('')===label)!;

it('preserves mounted result state through edit, regroup, cancel and save',async()=>{
  const original=board(),transport=engine(),save=vi.fn();
  const props={workspace:workspace(),engine:transport,generation:'session-a',saved:[{workspace:'lab',board:original}],boardId:original.id,onSave:save,onRemove:vi.fn(),onClose:vi.fn()};
  await act(async()=>{tree=create(<DashboardHost {...props}/>);});
  act(()=>tree!.root.findByProps({'aria-label':'Fixture view selection'}).props.onClick());
  act(()=>button('Edit').props.onClick());
  act(()=>button('Group in threes').props.onClick());
  act(()=>button('Cancel').props.onClick());
  expect(tree!.root.findByProps({'aria-label':'Fixture view selection'}).children.join('')).toBe('four:1');
  act(()=>button('Edit').props.onClick());
  const next={...original,title:'Revised',revision:1};
  act(()=>tree!.root.findByType(DashboardEditor).props.onSave(next));
  act(()=>tree!.update(<DashboardHost {...props} saved={[{workspace:'lab',board:{...next,revision:2}}]}/>));
  expect(lifetime.mounts).toBe(1);expect(lifetime.unmounts).toBe(0);
  expect(save).toHaveBeenCalledWith(next);expect(transport.submit).not.toHaveBeenCalled();
  expect(transport.fetch).toHaveBeenCalledExactlyOnceWith('result-handle');
});

it('never reads old-generation, private, missing or unconfirmed references and does not bind by label',async()=>{
  const original=board(),base=workspace(),transport=engine();
  const cases=[
    {workspace:base,generation:'session-b',text:'previous workspace session'},
    {workspace:{...base,nodes:base.nodes.map(node=>({...node,private:true}))},generation:'session-a',text:'private'},
    {workspace:{...base,nodes:base.nodes.map(node=>({...node,id:'replacement-node'}))},generation:'session-a',text:'no longer exists'},
    {workspace:{...base,nodes:base.nodes.map(node=>({...node,doubt:{capability:'fixture',safe:false,when:'unknown'}}))},generation:'session-a',text:'unconfirmed'},
  ];
  for(const item of cases){
    await act(async()=>{tree=create(<DashboardHost {...item} engine={transport} saved={[{workspace:'lab',board:original}]} boardId={original.id} onSave={vi.fn()} onRemove={vi.fn()} onClose={vi.fn()}/>);});
    expect(JSON.stringify(tree!.toJSON())).toContain(item.text);
    act(()=>tree!.unmount());tree=undefined;
  }
  expect(transport.fetch).not.toHaveBeenCalled();expect(transport.submit).not.toHaveBeenCalled();
});

it('keeps a live result mounted while a newer publication is being read, and revokes it immediately on privacy loss',async()=>{
  const original=board(),transport=engine(),base=workspace();
  const props={workspace:base,engine:transport,generation:'session-a',saved:[{workspace:'lab',board:original}],boardId:original.id,onSave:vi.fn(),onRemove:vi.fn(),onClose:vi.fn()};
  await act(async()=>{tree=create(<DashboardHost {...props}/>);});
  let release!:(value:StoredValue)=>void;
  vi.mocked(transport.fetch).mockReturnValueOnce(new Promise(resolve=>{release=resolve;}));
  const newer=apply(base,{event:'ready',node:'result-node',handle:'new-handle',type:'Text',bytes:1,provenance:{},kept:true,cautions:[]});
  await act(async()=>tree!.update(<DashboardHost {...props} workspace={newer}/>));
  expect(lifetime.mounts).toBe(1);expect(lifetime.unmounts).toBe(0);
  expect(JSON.stringify(tree!.toJSON())).toContain('latest publication');
  await act(async()=>release({type:{kind:'primitive',name:'Text'},data:'five',provenance:{}}));
  expect(lifetime.mounts).toBe(1);expect(lifetime.unmounts).toBe(0);
  act(()=>tree!.update(<DashboardHost {...props} workspace={{...newer,nodes:newer.nodes.map(node=>({...node,private:true}))}}/>));
  expect(lifetime.unmounts).toBe(1);expect(JSON.stringify(tree!.toJSON())).not.toContain('five');
});

it('preserves a live card and its reading position through publication-pending gaps without replaying its source',async()=>{
  const original=board(),transport=engine(),base=workspace();
  const props={workspace:base,engine:transport,generation:'session-a',saved:[{workspace:'lab',board:original}],boardId:original.id,onSave:vi.fn(),onRemove:vi.fn(),onClose:vi.fn()};
  await act(async()=>{tree=create(<DashboardHost {...props}/>);});
  act(()=>tree!.root.findByProps({'aria-label':'Fixture view selection'}).props.onClick());
  const publishing=apply(base,{event:'node',node:'result-node',state:'ready',constructionComplete:false,publication:{state:'pending',run:'run-next',handle:null,uncertainHandle:null,problem:null,message:'Publishing',pinBinding:null}});
  expect(publishing.nodes[0]?.handle).toBeUndefined();
  act(()=>tree!.update(<DashboardHost {...props} workspace={publishing}/>));
  expect(tree!.root.findByProps({'aria-label':'Fixture view selection'}).children.join('')).toBe('four:1');
  expect(lifetime.unmounts).toBe(0);
  expect(tree!.root.findByType(DashboardCanvas).props.sources).toHaveLength(1);
  const next=apply(publishing,{event:'ready',node:'result-node',handle:'next-handle',type:'Text',bytes:1,provenance:{},kept:false,cautions:[]});
  await act(async()=>tree!.update(<DashboardHost {...props} workspace={next}/>));
  expect(tree!.root.findByProps({'aria-label':'Fixture view selection'}).children.join('')).toBe('four:1');
  expect(lifetime.mounts).toBe(1);
  const unavailable=apply(next,{event:'node',node:'result-node',state:'ready',constructionComplete:false,publication:{state:'unavailable',run:'run-next',handle:null,uncertainHandle:null,problem:null,message:'Unavailable',pinBinding:null}});
  act(()=>tree!.update(<DashboardHost {...props} workspace={unavailable}/>));
  expect(lifetime.unmounts).toBe(1);
  expect(JSON.stringify(tree!.toJSON())).not.toContain('four:1');
  expect(transport.submit).not.toHaveBeenCalled();
});

it('reads live members through their independent display owner instead of stored snapshot handles',async()=>{
  const original=board(),transport=engine(),base=workspace();
  const streaming={...base,nodes:base.nodes.map(node=>({...node,streamOutput:true}))};
  const props={workspace:streaming,engine:transport,generation:'session-a',saved:[{workspace:'lab',board:original}],boardId:original.id,onSave:vi.fn(),onRemove:vi.fn(),onClose:vi.fn()};
  await act(async()=>{tree=create(<DashboardHost {...props}/>);});
  const position=()=>tree!.root.findByProps({'aria-label':'Fixture live reading position'});
  expect(position().props['data-cell-hold']).toBe(false);
  act(()=>position().props.onClick());
  const publishing=apply(streaming,{event:'node',node:'result-node',state:'ready',constructionComplete:false,publication:{state:'pending',run:'stream-run',handle:null,uncertainHandle:null,problem:null,message:'Publishing'}});
  act(()=>tree!.update(<DashboardHost {...props} workspace={publishing}/>));
  expect(position().children.join('')).toBe('1');
  expect(lifetime.unmounts).toBe(0);
  const next=apply(publishing,{event:'ready',node:'result-node',handle:'next-window',type:'Text',bytes:1,provenance:{},kept:false,cautions:[]});
  await act(async()=>tree!.update(<DashboardHost {...props} workspace={next}/>));
  expect(position().children.join('')).toBe('1');
  expect(transport.fetch).not.toHaveBeenCalled();
  expect(transport.submit).not.toHaveBeenCalled();
  act(()=>tree!.update(<DashboardHost {...props} workspace={{...next,nodes:next.nodes.map(node=>({...node,private:true}))}}/>));
  expect(lifetime.unmounts).toBe(1);
  expect(tree!.root.findAllByProps({'aria-label':'Fixture live reading position'})).toHaveLength(0);
});

it('should_LeadEveryHeaderWithTheParentExit_When_ShowingLibraryBoardOrUnavailableBoard',async()=>{
  // Arrange
  const original=board(),transport=engine(),close=vi.fn(),remove=vi.fn(),save=vi.fn();
  const props={workspace:workspace(),engine:transport,generation:'session-a',saved:[{workspace:'lab',board:original}],onSave:save,onRemove:remove,onClose:close};
  const exit=()=>tree!.root.findByProps({className:'dashboard-host-head'}).findAllByType('button')[0]!;
  // Act: library, then a board opened from it, then back through All dashboards.
  await act(async()=>{tree=create(<DashboardHost {...props}/>);});
  const library=exit().children.join('');
  await act(async()=>button('Open').props.onClick());
  const viewing=exit().children.join('');
  act(()=>button('All dashboards').props.onClick());
  // Assert
  expect([library,viewing]).toEqual(['Back to session','Back to session']);
  expect(button('Create dashboard')).toBeDefined();
  expect(close).not.toHaveBeenCalled();
  act(()=>exit().props.onClick());
  expect(close).toHaveBeenCalledTimes(1);
  // Act: a board opened by its parent, labelled by that parent, then no longer saved.
  act(()=>tree!.unmount());
  await act(async()=>{tree=create(<DashboardHost {...props} boardId={original.id} closeLabel="Close dashboard"/>);});
  expect(exit().children.join('')).toBe('Close dashboard');
  act(()=>tree!.update(<DashboardHost {...props} saved={[]} boardId={original.id} closeLabel="Close dashboard"/>));
  expect(JSON.stringify(tree!.toJSON())).toContain('no longer saved');
  act(()=>exit().props.onClick());
  // Assert: only the parent callback ran; nothing was saved, removed or submitted.
  expect(close).toHaveBeenCalledTimes(2);
  expect(save).not.toHaveBeenCalled();expect(remove).not.toHaveBeenCalled();
  expect(transport.submit).not.toHaveBeenCalled();
});

it('should_AskBeforeDiscardingAnUnsavedDraft_When_ExitIsChosenWhileEditing',async()=>{
  // Arrange
  const original=board(),transport=engine(),close=vi.fn(),save=vi.fn();
  const props={workspace:workspace(),engine:transport,generation:'session-a',saved:[{workspace:'lab',board:original}],boardId:original.id,onSave:save,onRemove:vi.fn(),onClose:close};
  await act(async()=>{tree=create(<DashboardHost {...props}/>);});
  act(()=>tree!.root.findByProps({'aria-label':'Fixture view selection'}).props.onClick());
  act(()=>button('Edit').props.onClick());
  act(()=>button('Group in threes').props.onClick());
  // Act
  act(()=>button('Back to session').props.onClick());
  // Assert: refused until confirmed; the draft and its mounted view stay.
  expect(close).not.toHaveBeenCalled();
  expect(tree!.root.findByProps({'aria-label':'Unsaved draft'})).toBeDefined();
  expect(tree!.root.findByType(DashboardEditor).props.editing).toBe(true);
  act(()=>button('Stay').props.onClick());
  expect(tree!.root.findAllByProps({'aria-label':'Unsaved draft'})).toHaveLength(0);
  expect(tree!.root.findByProps({'aria-label':'Fixture view selection'}).children.join('')).toBe('four:1');
  act(()=>button('Back to session').props.onClick());
  act(()=>button('Discard and return').props.onClick());
  expect(close).toHaveBeenCalledTimes(1);
  expect(save).not.toHaveBeenCalled();expect(transport.submit).not.toHaveBeenCalled();
  expect(lifetime.mounts).toBe(1);expect(lifetime.unmounts).toBe(0);
});

it('maps imported member identities to authoritative metadata and reflows without remounting leaves',()=>{
  let width=1280;const observers:(()=>void)[]=[];
  vi.stubGlobal('ResizeObserver',class{constructor(callback:()=>void){observers.push(callback);}observe(){}unobserve(){}disconnect(){}});
  const original=board(),source={...member,id:'different-picker-id',width:'preferred' as const,align:'center' as const,live:true,sizing:{min:{columns:20,rows:2},preferred:{columns:24,rows:8},max:{columns:40,rows:12}}};
  const Body=()=>{useEffect(()=>{lifetime.mounts++;return()=>{lifetime.unmounts++;};},[]);return <span>Chart</span>;};
  const props={board:original,sources:[source],renderMember:()=> <Body/>};
  act(()=>{tree=create(<DashboardCanvas {...props}/>,{createNodeMock:()=>({get clientWidth(){return width;},getBoundingClientRect:()=>({height:220})})});});
  expect(tree!.root.findByProps({className:'dashboard-card-body bounded stream-fill'}).props.style.height).toBe('8lh');
  expect(tree!.root.findByType('article').props.className).not.toContain('missing');
  width=360;act(()=>observers.forEach(callback=>callback()));
  expect(tree!.root.findByType('article').props.style.width).toBeLessThanOrEqual(width);
  expect(lifetime.mounts).toBe(1);expect(lifetime.unmounts).toBe(0);
});

it('makes a live body a stream fill of its preferred rows within its maximum, and leaves stored bodies to scroll themselves',()=>{
  const live={...member,id:'live-picker-id',live:true,width:'fill' as const,align:'start' as const,sizing:{min:{columns:20,rows:2},preferred:{columns:48,rows:30},max:{columns:320,rows:12}}};
  const props={board:board(),renderMember:()=> <span>Log</span>};
  act(()=>{tree=create(<DashboardCanvas {...props} sources={[live]}/>);});
  const body=()=>tree!.root.find(node=>typeof node.props.className==='string'&&node.props.className.startsWith('dashboard-card-body'));
  expect(body().props.className).toBe('dashboard-card-body bounded stream-fill');
  expect(body().props.style).toMatchObject({height:'12lh','--dashboard-rows':12});
  act(()=>tree!.update(<DashboardCanvas {...props} sources={[{...live,live:false}]}/>));
  expect(body().props.className).toBe('dashboard-card-body bounded');
  expect(body().props.style.height).toBeUndefined();
});
