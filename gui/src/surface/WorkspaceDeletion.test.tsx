import {act,create,type ReactTestRenderer} from 'react-test-renderer';
import {afterEach,it,expect,vi} from 'vitest';
import {WorkspaceDeletion} from './WorkspaceDeletion';
import {removeWorkspaceViews} from './workspace-tabs';
import {oneP} from './split-model';
import {read} from './commands';
let tree:ReactTestRenderer|undefined;afterEach(()=>{act(()=>tree?.unmount());vi.unstubAllGlobals();});
const preview={token:'t',workspace:'qa',identity:'i',cells:2,nodes:2,running:['n1'],streams:[],sandboxes:['dash'],runningSandboxes:['dash'],terminals:[],exclusivePayloads:1,protectedPayloads:1,sharedWorkspaces:['peer'],blockers:[],preserved:['Saved credentials'],expiresInSeconds:120};
it('opening a preview does not stop work; confirmation explicitly approves stop and protected content',async()=>{
 const requests:any[]=[];const done=vi.fn();
 vi.stubGlobal('fetch',vi.fn(async(_url,init)=>{const body=JSON.parse(init.body);requests.push(body);return Response.json(body.action==='preview'?preview:{deleted:true});}));
 await act(async()=>{tree=create(<WorkspaceDeletion binding={{workspace:'qa',generation:'g',client:'c'}} onClose={()=>{}} onDeleted={done}/>);});
 expect(requests).toEqual([{action:'preview',client:'c'}]);
 const confirm=()=>tree!.root.findAllByType('button').find(b=>b.children.join('')==='Stop and delete')!;
 expect(confirm().props.disabled).toBe(true);
 act(()=>tree!.root.findByProps({type:'checkbox'}).props.onChange({target:{checked:true}}));
 await act(async()=>confirm().props.onClick());
 expect(requests[1]).toEqual({action:'confirm',token:'t',stop:true,protected:true,client:'c'});expect(done).toHaveBeenCalledOnce();
});
it('removes only retired workspace references and keeps a usable pane',()=>{
 const state=oneP({id:'p1',title:'QA',workspace:'qa',tabs:[{workspace:'qa'},{workspace:'peer'}]});
 const result=removeWorkspaceViews(state,'qa');expect(result.panes[0]!.workspace).toBe('peer');
 const empty=removeWorkspaceViews(result,'peer');expect(empty.panes).toHaveLength(1);expect(empty.panes[0]!.workspace).toBeUndefined();
 expect(read('/workspace delete')).toEqual({kind:'workspace-delete'});
});

it('keeps plain-text stale-session details from the service',async()=>{
 const {workspaceDeletion}=await import('../workspace-deletion');
 vi.stubGlobal('fetch',vi.fn(async()=>new Response('Workspace changed; request a new deletion preview.',{status:409})));
 await expect(workspaceDeletion({workspace:'qa',generation:'old',client:'c'},{action:'preview'})).rejects.toThrow('Workspace changed');
});
