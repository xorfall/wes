import { workspaceHeaders } from './workspace-binding';
export interface DeletionBinding {workspace:string; generation:string; client:string}
export interface WorkspaceDeletionPreview {
 token:string; workspace:string; identity:string; cells:number; nodes:number;
 running:string[]; streams:string[]; sandboxes:string[]; runningSandboxes:string[]; terminals:string[];
 exclusivePayloads:number; protectedPayloads:number; sharedWorkspaces:string[]; blockers:string[]; preserved:string[]; expiresInSeconds:number;
}
export async function workspaceDeletion(binding:DeletionBinding, action:{action:'preview'}|{action:'confirm';token:string;stop:boolean;protected:boolean}, signal?:AbortSignal):Promise<WorkspaceDeletionPreview|{deleted:true}> {
 const response=await fetch('/workspace-deletion',{method:'POST',headers:{...workspaceHeaders(binding.workspace),'X-Wes-Session':binding.generation,'Content-Type':'application/json'},body:JSON.stringify({...action,client:binding.client}),signal});
 const text=await response.text();
 let body: any;
 try {body=JSON.parse(text);} catch {throw new Error(text||'Workspace deletion service did not return a response.');}
 if(!response.ok)throw Object.assign(new Error(body.message??'Workspace deletion could not be confirmed. Reopen the workspace list before retrying.'),{code:body.code});
 return body;
}
