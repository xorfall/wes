import {useEffect,useState} from 'react';
import {workspaceDeletion,type DeletionBinding,type WorkspaceDeletionPreview} from '../workspace-deletion';
import {applicationLog} from '../application-log';
export function WorkspaceDeletion({binding,onClose,onDeleted}:{binding:DeletionBinding;onClose:()=>void;onDeleted:()=>void}) {
 const [preview,setPreview]=useState<WorkspaceDeletionPreview>();const [error,setError]=useState('');const [busy,setBusy]=useState(false);const [protectedData,setProtected]=useState(false);
 const load=async(signal?:AbortSignal)=>{setError('');setPreview(undefined);setProtected(false);try{setPreview(await workspaceDeletion(binding,{action:'preview'},signal) as WorkspaceDeletionPreview);}catch(e){if(!signal?.aborted)setError((e as Error).message);}};
 useEffect(()=>{const abort=new AbortController();void load(abort.signal);return()=>abort.abort();},[binding.workspace,binding.generation]);
 const stop=!!preview&&(preview.running.length+preview.streams.length+preview.runningSandboxes.length+preview.terminals.length>0);
 const confirm=async()=>{if(!preview||busy)return;setBusy(true);setError('');try{await workspaceDeletion(binding,{action:'confirm',token:preview.token,stop,protected:protectedData});onDeleted();}catch(e){const failure=e as Error&{code?:string};applicationLog.add({level:'error',code:failure.code??'WORKSPACE_DELETE',source:'Workspace deletion',operation:'Delete',workspace:binding.workspace,generation:binding.generation,message:failure.message});setError(failure.message);setPreview(undefined);if(failure.code==='STO010')onDeleted();}finally{setBusy(false);}};
 return <section className="workspace-delete-panel" role="dialog" aria-modal="true" aria-label={`Delete workspace ${binding.workspace}`} onKeyDown={e=>{if(e.key==='Escape'&&!busy)onClose();}}>
 <header><h2>Delete workspace <span className="mono-ref">{binding.workspace}</span></h2><button autoFocus type="button" className="screen-chip" disabled={busy} onClick={onClose}>cancel</button></header>
 {!preview&&!error&&<p>Reading deletion effects…</p>}
 {preview&&<><p>{preview.cells} cells · {preview.nodes} nodes · {preview.sandboxes.length} sandbox definitions · {preview.exclusivePayloads} exclusive result payloads</p>
 <p>This removes this workspace’s definitions and recorded history. It cannot undo external effects.</p>
 {preview.sandboxes.length>0&&<p>Sandboxes: {preview.sandboxes.join(', ')}</p>}
 {stop&&<><h3>Work to stop</h3><ul><li>Executing nodes: {preview.running.join(', ')||'none'}</li><li>Streams: {preview.streams.join(', ')||'none'}</li><li>Sandbox runtimes: {preview.runningSandboxes.join(', ')||'none'}</li><li>Terminals: {preview.terminals.length}</li></ul><p>Stopping waits for owned jobs to exit. A terminal’s agent may be disconnected from other workspaces; their work is preserved. If deletion is refused afterward, stopped jobs will remain stopped.</p></>}
 <h3>Preserved</h3><ul>{preview.preserved.map(p=><li key={p}>{p}</li>)}</ul>
 {preview.sharedWorkspaces.length>0&&<p>Shared results remain in: {preview.sharedWorkspaces.join(', ')}</p>}
 {preview.protectedPayloads>0&&<label><input type="checkbox" checked={protectedData} disabled={busy} onChange={e=>setProtected(e.target.checked)}/>Delete {preview.protectedPayloads} Protected result payloads whose last owner is this workspace</label>}
 {preview.blockers.length>0&&<><h3>Blocked</h3><ul>{preview.blockers.map(p=><li key={p}>{p}</li>)}</ul></>}
 <p className="mono-dim">Preview expires in {preview.expiresInSeconds} seconds. Changes require a new preview. Uncertain external outcomes block deletion.</p>
 <button type="button" className="screen-chip mono-bad" disabled={busy||preview.blockers.length>0||(preview.protectedPayloads>0&&!protectedData)} onClick={()=>void confirm()}>{busy?'Stopping and deleting…':stop?'Stop and delete':'Delete workspace'}</button></>}
 {error&&<p role="alert" className="mono-bad">{error}</p>}
 {!busy&&<button type="button" className="screen-chip" onClick={()=>void load()}>refresh preview</button>}
 </section>;
}
