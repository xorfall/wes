import {useCallback,useEffect,useMemo,useState,type ReactNode} from 'react';
import type {Engine} from '../engine';
import type {Workspace,WorkspaceNode} from '../workspace';
import type {StoredValue} from '../protocol';
import {useResults} from '../surface/results';
import {ValueBlock} from '../surface/render/ValueBlock';
import {LiveView} from '../surface/LiveView';
import {resultAccess} from '../surface/result-access';
import {DashboardEditor} from './DashboardEditor';
import {newDashboard,type Dashboard,type DashboardMember} from './model';
import {dashboardSource,memberProblem,readableDashboardNode} from './sources';
import {exportDashboard,importDashboard,type SavedDashboard} from './storage';
import './dashboard.css';

function MemberResult({member,node,engine,generation,onValue}:{member:DashboardMember;node?:WorkspaceNode;engine:Engine;generation?:string;onValue:(node:string,value:StoredValue)=>void}) {
  const problem=memberProblem(member,generation,node),live=!problem&&generation&&resultAccess(node).live;
  const handle=!problem&&!live?node?.handle:undefined;
  const {held,reads,retry,observations}=useResults(engine,generation,handle?[handle]:[],node&&!problem&&!live?[node]:[]);
  const observation=observations.get(member.node);
  const value=observation?.value??(handle?held.get(handle):undefined);
  // Report metadata in an effect: a render must never update the parent canvas.
  useReportValue(member.node,value,onValue);
  if(problem)return <p className="mono-warn" role="status">{problem}</p>;
  if(live&&node&&generation)return <LiveView mode="expanded" engine={engine} generation={generation} node={node} cellHold={false}
    key={`${generation}:${node.id}:${node.command}:${JSON.stringify(node.environment)}`}/>;
  const read=handle?reads.get(handle):undefined;
  const failure=read?.problem?<p role="status">{read.problem} <button className="cell-action" onClick={()=>retry(handle!)}>Read again</button></p>:undefined;
  if(!value&&failure)return failure;
  if(!value)return <p className="mono-dim" role="status">Reading existing result…</p>;
  return <>{failure}{observation?.state!=='current'&&observation&&<p className="mono-dim" role="status">{observation.staleReason??'Waiting for the latest publication…'}</p>}<ValueBlock engine={engine} value={value} cacheKey={`${generation}:${observation?.handle??handle}`} bindingKey={`dashboard:${member.id}`} mode="expanded" inCell={false}/></>;
}
function useReportValue(node:string,value:StoredValue|undefined,onValue:(node:string,value:StoredValue)=>void){useEffect(()=>{if(value)onValue(node,value);},[node,value,onValue]);}

const SHARE_NOTE='Layout and result references only. Data and credentials are not included. References resolve only in the workspace session that produced them.';
const IMPORT_NOTE='Loads as an unsaved draft; nothing is stored until you Save. Imported references stay with their original results. Replace unavailable ones with current results explicitly.';

/** A bounded, monospaced definition text. Read-only text selects on focus; nothing touches the clipboard. */
function Definition({label,value,note,onChange,children}:{label:string;value:string;note:string;onChange?:(text:string)=>void;children:ReactNode}) {
  return <section className="dashboard-definition" aria-label={label}>
    <label className="dashboard-definition-label">{label}
      <textarea className="dashboard-definition-text" value={value} readOnly={!onChange} spellCheck={false} rows={12}
        onFocus={onChange?undefined:event=>event.currentTarget.select()} onChange={onChange?event=>onChange(event.target.value):undefined}/>
    </label>
    <p className="dashboard-host-note">{note}</p>
    <div className="dashboard-host-actions">{children}</div>
  </section>;
}

const message=(error:unknown)=>error instanceof Error?error.message:String(error);

/** The label of the dashboard exit when the parent does not name where it leads. */
export const DASHBOARD_EXIT_LABEL='Back to session';

/** The one exit every dashboard state shows first in its header. The parent owns where it leads and names it. */
function DashboardExit({label,onExit}:{label:string;onExit:()=>void}) {
  return <button type="button" className="dashboard-button dashboard-host-exit" onClick={onExit}>{label}</button>;
}

/** A dashboard that cannot be shown, with the same exit as a shown one. */
export function DashboardUnavailable({message,closeLabel=DASHBOARD_EXIT_LABEL,onClose}:{message:string;closeLabel?:string;onClose:()=>void}) {
  return <section className="dashboard-host">
    <header className="dashboard-host-head">
      <DashboardExit label={closeLabel} onExit={onClose}/>
      <div className="dashboard-host-title"><p className="dashboard-host-note" role="status">{message}</p></div>
    </header>
  </section>;
}

/**
 * Integration host resolves references; presentation components have no engine access.
 * `onClose` leaves the dashboard the way its parent opened it; `closeLabel` names that exit.
 */
export function DashboardHost({workspace,engine,generation,saved,boardId,edit=false,closeLabel=DASHBOARD_EXIT_LABEL,onSave,onRemove,onClose}:{workspace:Workspace;engine:Engine;generation?:string;saved:readonly SavedDashboard[];boardId?:string;edit?:boolean;closeLabel?:string;onSave:(board:Dashboard)=>void;onRemove:(board:Dashboard)=>void;onClose:()=>void}) {
  const [shownId,setShownId]=useState(boardId);
  const existing=saved.find(entry=>entry.board.id===shownId)?.board;
  // Opened straight into creating: cancelling that draft leaves the screen instead of showing the library.
  const [direct]=useState(edit&&!existing);
  const [fresh,setFresh]=useState<Dashboard|undefined>(()=>edit&&!existing?newDashboard():undefined);
  const [editing,setEditing]=useState(edit);
  const [problem,setProblem]=useState<string>();
  const [definition,setDefinition]=useState<{readonly mode:'share'|'import';readonly text:string}>();
  const [removing,setRemoving]=useState(false);
  // Leaving while editing asks first; the draft and its mounted views stay until Discard.
  const [leaving,setLeaving]=useState(false);
  const [values,setValues]=useState<ReadonlyMap<string,StoredValue>>(new Map());
  const report=useCallback((node:string,value:StoredValue)=>setValues(previous=>previous.get(node)===value?previous:new Map(previous).set(node,value)),[]);
  const sources=useMemo(()=>generation?workspace.nodes.filter(readableDashboardNode).map(node=>dashboardSource(node,generation,values.get(node.id))):[],[workspace.nodes,generation,values]);
  const render=(member:DashboardMember):ReactNode=><MemberResult member={member} node={workspace.nodes.find(node=>node.id===member.node)} engine={engine} generation={generation} onValue={report}/>;
  const board=fresh??existing;

  const reset=()=>{setProblem(undefined);setDefinition(undefined);setRemoving(false);setLeaving(false);};
  const open=(id:string|undefined,editNow:boolean)=>{reset();setShownId(id);setEditing(editNow);};
  const startDraft=(draft:Dashboard)=>{reset();setFresh(draft);setEditing(true);};
  // A saved draft keeps its id, so the same keyed editor continues with the stored board: no view remounts.
  const save=(next:Dashboard)=>{try{onSave(next);setShownId(next.id);setFresh(undefined);setEditing(false);setProblem(undefined);setLeaving(false);}catch(error){setProblem(message(error));}};
  const cancel=()=>{
    setProblem(undefined);setEditing(false);setLeaving(false);
    if(fresh){setFresh(undefined);if(direct)onClose();}
  };
  const remove=(target:Dashboard)=>{
    try{onRemove(target);reset();if(boardId!==undefined)onClose();else setShownId(undefined);}catch(error){setProblem(message(error));}
  };
  const load=()=>{try{startDraft(importDashboard(definition?.text??''));}catch(error){setProblem(message(error));}};

  if(board)return <section className="dashboard-host">
    <header className="dashboard-host-head">
      <DashboardExit label={closeLabel} onExit={editing?()=>setLeaving(true):onClose}/>
      <div className="dashboard-host-title"><strong>{board.title}</strong><span className="mono-dim">${board.name}</span>
        {fresh&&<span className="dashboard-host-status">New draft · not saved</span>}</div>
      {!editing&&existing&&<div className="dashboard-host-actions">
        {boardId===undefined&&<button type="button" className="dashboard-button" onClick={()=>open(undefined,false)}>All dashboards</button>}
        <button type="button" className="dashboard-button" onClick={()=>open(existing.id,true)}>Edit</button>
        <button type="button" className="dashboard-button" aria-pressed={definition?.mode==='share'}
          onClick={()=>{setRemoving(false);setDefinition(definition?.mode==='share'?undefined:{mode:'share',text:exportDashboard(existing)});}}>Share definition</button>
        <button type="button" className="dashboard-button" aria-pressed={removing} onClick={()=>{setDefinition(undefined);setRemoving(!removing);}}>Remove layout</button>
      </div>}
    </header>
    {editing&&leaving&&<div className="dashboard-host-confirm" role="group" aria-label="Unsaved draft">
      <p>Leave this dashboard? The draft is not saved and will be discarded.</p>
      <div className="dashboard-host-actions">
        <button type="button" className="dashboard-button" onClick={()=>setLeaving(false)}>Stay</button>
        <button type="button" className="dashboard-button danger" onClick={onClose}>Discard and return</button>
      </div>
    </div>}
    {!editing&&removing&&existing&&<div className="dashboard-host-confirm" role="group" aria-label="Remove layout">
      <p>Remove the ${existing.name} layout? Its results and their commands are not affected.</p>
      <div className="dashboard-host-actions">
        <button type="button" className="dashboard-button" onClick={()=>setRemoving(false)}>Keep</button>
        <button type="button" className="dashboard-button danger" onClick={()=>remove(existing)}>Remove layout</button>
      </div>
    </div>}
    {!editing&&problem&&<p role="alert" className="dashboard-host-problem">{problem}</p>}
    {!editing&&definition?.mode==='share'&&<Definition label="Dashboard definition" value={definition.text} note={SHARE_NOTE}>
      <button type="button" className="dashboard-button" onClick={()=>setDefinition(undefined)}>Close definition</button>
    </Definition>}
    <div className="dashboard-host-body">
      <DashboardEditor key={board.id} board={board} editing={editing} sources={sources} renderMember={render}
        problem={editing?problem:undefined} onSave={save} onCancel={cancel}/>
    </div>
  </section>;
  if(shownId!==undefined&&boardId===shownId)return <DashboardUnavailable message="This dashboard is no longer saved in this workspace." closeLabel={closeLabel} onClose={onClose}/>;
  return <section className="dashboard-host">
    <header className="dashboard-host-head">
      <DashboardExit label={closeLabel} onExit={onClose}/>
      <div className="dashboard-host-title"><strong>Dashboards</strong><span className="mono-dim">{saved.length} saved</span></div>
      <div className="dashboard-host-actions">
        <button type="button" className="dashboard-button primary" onClick={()=>startDraft(newDashboard())}>Create dashboard</button>
        <button type="button" className="dashboard-button" aria-pressed={definition?.mode==='import'}
          onClick={()=>{setProblem(undefined);setDefinition(definition?.mode==='import'?undefined:{mode:'import',text:''});}}>Import definition</button>
      </div>
    </header>
    {problem&&<p role="alert" className="dashboard-host-problem">{problem}</p>}
    {definition?.mode==='import'&&<Definition label="Dashboard definition" value={definition.text} note={IMPORT_NOTE} onChange={text=>setDefinition({mode:'import',text})}>
      <button type="button" className="dashboard-button" onClick={()=>{setProblem(undefined);setDefinition(undefined);}}>Cancel import</button>
      <button type="button" className="dashboard-button primary" disabled={!definition.text.trim()} onClick={load}>Load draft</button>
    </Definition>}
    {saved.length?<ul className="dashboard-library" aria-label="Saved dashboards">
      {saved.map(({board})=><li key={board.id} className="dashboard-library-item">
        <div className="dashboard-library-name"><strong>{board.title}</strong><span className="mono-dim">${board.name}</span></div>
        <span className="dashboard-library-meta">{board.members.length} result{board.members.length===1?'':'s'} · /tab ${board.name} · /split ${board.name}</span>
        <div className="dashboard-host-actions">
          <button type="button" className="dashboard-button" onClick={()=>open(board.id,false)}>Open</button>
          <button type="button" className="dashboard-button" onClick={()=>open(board.id,true)}>Edit</button>
        </div>
      </li>)}
    </ul>:<p className="dashboard-host-note">No dashboards saved. Create one from existing results.</p>}
  </section>;
}
