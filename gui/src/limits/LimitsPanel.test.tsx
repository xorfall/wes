import {act,create,type ReactTestRenderer} from 'react-test-renderer';
import {afterEach,expect,it,vi} from 'vitest';
import {LimitsPanel,LIMIT_UNITS,parseLimit} from './LimitsPanel';
import type {LimitEntry} from './model';
let tree:ReactTestRenderer|undefined;
afterEach(()=>{if(tree)act(()=>tree!.unmount());tree=undefined;});
const entry:LimitEntry={id:'fixture.body.bytes',label:'Body bytes',description:'Maximum body retained',group:'Transport',unit:'bytes',default:1024,active:1024,saved:1024,min:1,max:1024**3,editable:true,restart:true};
const input=()=>tree!.root.findByProps({className:'limits-input'});
const button=(label:string)=>tree!.root.findAllByType('button').find(node=>node.children.join('')===label)!;
it('accepts exact base-unit values and rejects fractional bytes, overflow and malformed input',()=>{
  const units=LIMIT_UNITS.bytes;
  expect(parseLimit('1.5',units[2]!,entry)).toEqual({ok:true,value:1_572_864});
  for(const text of ['1.3','NaN','Infinity','1e3','','9007199254740992','0','-1'])expect(parseLimit(text,units[0]!,entry).ok).toBe(false);
  expect(parseLimit('1.5',LIMIT_UNITS.milliseconds[1]!,{...entry,unit:'milliseconds'})).toEqual({ok:true,value:1500});
});
it('retains drafts after failed saves and during refresh, and reports restart-pending values',async()=>{
  const save=vi.fn().mockRejectedValue(new Error('Synthetic storage failure')),props={entries:[entry],onSave:save,onReload:vi.fn()};
  act(()=>{tree=create(<LimitsPanel {...props}/>);});
  act(()=>input().props.onChange({target:{value:'2'}}));
  await act(async()=>button('Save').props.onClick());
  expect(save).toHaveBeenCalledWith({[entry.id]:2048});
  expect(input().props.value).toBe('2');expect(JSON.stringify(tree!.toJSON())).toContain('Synthetic storage failure');
  act(()=>tree!.update(<LimitsPanel {...props} entries={[{...entry,saved:4096}]}/>));
  expect(input().props.value).toBe('2');expect(JSON.stringify(tree!.toJSON())).toContain('while you were editing');
  expect(JSON.stringify(tree!.toJSON())).toContain('Restart WesDesk');
  act(()=>button('Revert').props.onClick());
  expect(input().props.value).toBe('4');
});
it('explains fixed platform boundaries with no edit field',()=>{
  act(()=>{tree=create(<LimitsPanel entries={[{...entry,id:'platform.keepalive',editable:false,reason:'Browser-defined; use ordinary requests for larger bodies',active:65536,saved:65536}]} onSave={vi.fn()} onReload={vi.fn()}/>);});
  expect(tree!.root.findAllByProps({className:'limits-input'})).toHaveLength(0);
  expect(JSON.stringify(tree!.toJSON())).toContain('Browser-defined');
});
it('converts units without changing the exact base value, including four decimal places',()=>{
 const sample={...entry,default:65536,active:65536,saved:65536};const save=vi.fn();
 act(()=>{tree=create(<LimitsPanel entries={[sample]} onSave={save} onReload={vi.fn()}/>);});
 const unit=tree!.root.findAllByType('select').find(node=>node.props['aria-label']?.includes('unit'))!;
 act(()=>unit.props.onChange({target:{value:'MiB'}}));
 expect(input().props.value).toBe('0.0625');expect(save).not.toHaveBeenCalled();
 act(()=>input().props.onChange({target:{value:'0.125'}}));
 expect(parseLimit(input().props.value,LIMIT_UNITS.bytes[2]!,sample)).toEqual({ok:true,value:131072});
});
it('keeps the search query through a composing Escape and clears it on a plain one',()=>{
  act(()=>{tree=create(<LimitsPanel entries={[entry]} onSave={vi.fn()} onReload={vi.fn()}/>);});
  const search=()=>tree!.root.findByProps({'aria-label':'Search budgets'});
  act(()=>search().props.onChange({target:{value:'body'}}));
  for(const composition of [{nativeEvent:{isComposing:true}},{isComposing:true},{keyCode:229}]){
    const composed={key:'Escape',preventDefault:vi.fn(),stopPropagation:vi.fn(),...composition};
    act(()=>search().props.onKeyDown(composed));
    expect(search().props.value).toBe('body');
    expect(composed.stopPropagation).not.toHaveBeenCalled();
  }
  const plain={key:'Escape',preventDefault:vi.fn(),stopPropagation:vi.fn()};
  act(()=>search().props.onKeyDown(plain));
  expect(search().props.value).toBe('');expect(plain.stopPropagation).toHaveBeenCalledOnce();
});
it('keeps an edited budget through composing Escape without saving, then reverts on plain Escape',()=>{
  const save=vi.fn();
  act(()=>{tree=create(<LimitsPanel entries={[entry]} onSave={save} onReload={vi.fn()}/>);});
  const initial=input().props.value;
  act(()=>input().props.onChange({target:{value:'2'}}));
  for(const composition of [{nativeEvent:{isComposing:true}},{isComposing:true},{keyCode:229}]){
    const event={key:'Escape',preventDefault:vi.fn(),stopPropagation:vi.fn(),...composition};
    act(()=>input().props.onKeyDown(event));
    expect(input().props.value).toBe('2');
    expect(event.preventDefault).not.toHaveBeenCalled();
  }
  act(()=>input().props.onKeyDown({key:'Escape',preventDefault:vi.fn(),stopPropagation:vi.fn()}));
  expect(input().props.value).toBe(initial);
  expect(save).not.toHaveBeenCalled();
});
