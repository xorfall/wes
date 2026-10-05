import {expect,it} from 'vitest';
import {dashboardSource} from './sources';
import {bindValueViews,valueViewModules} from '../value-views/registry';
import {emptyWorkspace,apply} from '../workspace';
import type {StoredValue} from '../protocol';
const node=apply(emptyWorkspace,{event:'created',dependencyLifetime:'continuous',node:'card',name:'latency',command:':calc 1',dependsOn:[],interactive:false}).nodes[0]!;
const module=valueViewModules.named('metric')!;
const instance=(digest=module.definition!.digest):StoredValue=>({type:{kind:'meta',name:'ViewInstance'},data:{definition:'Metric',digest},provenance:{}});
it('resolves management contract names by exact digest, including installed revisions instead of selecting a current namesake',()=>{
 const value=instance();
 expect(dashboardSource(node,'generation',value).width).toBe('preferred');
 expect(dashboardSource(node,'generation',value).sizing).toEqual(module.definition!.layout!.expanded);
 expect(dashboardSource(node,'generation',instance('unavailable-revision')).width).toBe('fill');
 const old={...module,definition:{...module.definition!,digest:'previous-revision',layout:{...module.definition!.layout!,expanded:{...module.definition!.layout!.expanded,placement:{width:'preferred' as const,align:'center' as const}}}}};
 const previous=instance('previous-revision');bindValueViews(previous,[old]);
 expect(dashboardSource(node,'generation',previous).align).toBe('center');
 // A workspace-scoped catalogue must not borrow an unavailable global renderer.
 const withheld=instance();bindValueViews(withheld,[]);
 expect(dashboardSource(node,'generation',withheld).width).toBe('fill');
});
