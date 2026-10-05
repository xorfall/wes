import { workspaceSpecs, type SpecWorkspace, type ImportedSpec, type ImportedSpecResult } from "../../workspace-specs";
import { WorkspaceSpecs, ImportedSpecInspector } from "../WorkspaceSpecs";
import { useEffect, useRef, useState } from "react";
import { SpecImportForm } from "../SpecImportForm";
import { libraryRequest, readSchemaProvenance, type SpecPackage, type SpecResult } from "../../api-library";
import { type Segment } from "../MonoLine";
import { Screen, leaving } from "../Screen";
import { ProvenanceDetail, ProvenanceNotice, SchemaFieldsTable } from "../SchemaProvenance";
import { SpecOperations } from "../SpecOperations";
import { SpecSourceEditor } from "../SpecSourceEditor";
import { DraftEditor, revisionLabel, type DraftControls } from "../DraftEditor";
import { descriptorPreview, sameKey, type DraftSummary } from "../../draft-api";
import { SpecIdentity, SpecLibrary, SpecSource, libraryGroups } from "../SpecLibrary";
import { SpecDescribe } from "../SpecDescribe";
import { READ, READING, workspaceRowId, type HomeRead } from "../spec-home";
import "../spec.css";

const failedRead=(e:unknown):HomeRead=>({state:"failed",message:(e as Error).message});
/** Each landing read records its own outcome; waiting for all of them never lets one failure hide another section. */
const settled=async(...reads:Promise<void>[])=>{await Promise.allSettled(reads);};

const TABS = ["operations", "schema", "source", "diagnostics", "import"] as const;
type Tab = typeof TABS[number];

export function SpecScreen({top,onClose,onSubmit,chrome="full",binding}:{binding?:SpecWorkspace;top:readonly Segment[];onClose:()=>void;onSubmit:(source:string)=>string|undefined|Promise<string|undefined>;chrome?:"full"|"pane"}) {
  const [imports,setImports]=useState<ImportedSpec[]>([]);const [imported,setImported]=useState<ImportedSpecResult>();
  const [packages,setPackages]=useState<SpecPackage[]>([]);const [selected,setSelected]=useState<SpecResult>();
  const [tab,setTab]=useState<Tab>("operations");const [target,setTarget]=useState<string>();
  const [source,setSource]=useState("");const [diagnostics,setDiagnostics]=useState<string[]>([]);const [valid,setValid]=useState(false);
  const [busy,setBusy]=useState(false);const [message,setMessage]=useState("");const [failed,setFailed]=useState(false);
  const [alias,setAlias]=useState("");const [endpoint,setEndpoint]=useState("");const [replace,setReplace]=useState(false);const [exportPath,setExportPath]=useState("");
  const [location,setLocation]=useState("");const [provider,setProvider]=useState("");const [schemaType,setSchemaType]=useState<{ name: string }>();
  const [leavingDirty,setLeavingDirty]=useState(false);const pending=useRef<()=>void>();const sourceRef=useRef(source);sourceRef.current=source;
  const [drafts,setDrafts]=useState<DraftSummary[]>([]);const [openedDraft,setOpenedDraft]=useState<DraftSummary>();const [draftSubject,setDraftSubject]=useState<Segment[]>([]);const [draftRevision,setDraftRevision]=useState<string>();const draftControls=useRef<DraftControls>();
  const dirty=!!selected&&source!==selected.source;
  // A draft's editor owns its text; the screen only asks whether it is dirty and how to save it.
  const draftDirty=()=>!!openedDraft&&!!draftControls.current?.dirty;
  const run=async(work:()=>Promise<void>)=>{if(busy)return;setBusy(true);setFailed(false);setMessage("");try{await work();}catch(e){setMessage((e as Error).message);setFailed(true);}finally{setBusy(false);}};
  const [libraryRead,setLibraryRead]=useState<HomeRead>(READING);const [importsRead,setImportsRead]=useState<HomeRead>(binding?READING:READ);
  const [expanded,setExpanded]=useState<ReadonlySet<string>>(new Set());const [describeOpen,setDescribeOpen]=useState<boolean>();
  const listAll=async(signal?:AbortSignal)=>{try{const r=await libraryRequest<{packages:SpecPackage[];drafts:DraftSummary[]}>({action:"list"},signal);setPackages(r.packages);setDrafts(Array.isArray(r.drafts)?r.drafts:[]);setLibraryRead(READ);}catch(e){if(!signal?.aborted)setLibraryRead(failedRead(e));throw e;}};
  const listImports=async(signal?:AbortSignal)=>{if(!binding)return;try{const r=await workspaceSpecs<{specs:ImportedSpec[]}>(binding,undefined,signal);setImports(r.specs);setImportsRead(READ);}catch(e){if(!signal?.aborted)setImportsRead(failedRead(e));throw e;}};
  // An opened editor reports a failed refresh in its status line; the landing shows it in the section that failed.
  const load=async(signal?:AbortSignal)=>{await Promise.all([listAll(signal),listImports(signal)]);};
  const reload=(signal?:AbortSignal)=>settled(listAll(signal),listImports(signal));
  const runRef=useRef(run);runRef.current=run;
  useEffect(()=>{const abort=new AbortController();void reload(abort.signal);return()=>abort.abort();},[]);
  const accept=(r:SpecResult)=>{setSelected(r);setSource(r.source);setAlias(r.descriptor.provider);setValid(true);setDiagnostics(r.descriptor.diagnostics??[]);setTarget(undefined);};
  const pick=(next:string)=>setTarget(target===next?undefined:next);
  const guard=(action:()=>void)=>{if(dirty||draftDirty()){pending.current=action;setLeavingDirty(true);}else action();};
  const backToLibrary=()=>guard(()=>{setOpenedDraft(undefined);setSelected(undefined);setImported(undefined);setMessage("");setFailed(false);draftControls.current=undefined;});
  const selectDraft=(d:DraftSummary)=>guard(()=>{setSelected(undefined);setMessage("");setFailed(false);setDraftSubject([]);setDraftRevision(d.revision);setOpenedDraft(d);});
  const select=(p:SpecPackage)=>guard(()=>void run(async()=>{setOpenedDraft(undefined);accept(await libraryRequest<SpecResult>({action:"inspect",key:p.key,revision:p.revision}));setTab("operations");setEndpoint("");setReplace(false);}));
  const selectImported=(spec:ImportedSpec)=>guard(()=>void run(async()=>{if(!binding)return;const result=await workspaceSpecs<ImportedSpecResult>(binding,spec);setSelected(undefined);setOpenedDraft(undefined);setImported(result);}));
  const validate=async()=>{const text=sourceRef.current;const r=await libraryRequest<{valid:boolean;diagnostics:string[]}>({action:"validateDescriptor",text});if(text!==sourceRef.current)return false;setDiagnostics(r.diagnostics);setValid(r.valid);setMessage(r.valid?"Validation passed.":"Validation failed; fix the source before saving or importing.");setFailed(!r.valid);return r.valid;};
  const save=async()=>{if(!selected||!await validate())return false;const text=sourceRef.current;const r=await libraryRequest<SpecResult>({action:"saveDescriptor",key:selected.package.key,revision:selected.package.revision,text});if(sourceRef.current===text){accept(r);}else{setSelected(r);setValid(false);}await load();setMessage("Revision saved. Existing imports keep their captured bytes.");return sourceRef.current===text;};
  const change=(text:string)=>{setSource(text);sourceRef.current=text;setValid(false);setDiagnostics([]);};
  const credentials=[...new Set(selected?.descriptor.operations.flatMap(o=>o.auth.map(a=>a.secret).filter((s):s is string=>!!s))??[])];
  const currentRevision=selected?packages.filter(p=>sameKey(p.key,selected.package.key)).findIndex(p=>p.revision===selected.package.revision)+1:0;
  const provenance=selected?readSchemaProvenance(selected.descriptor.source):undefined;
  const draftRevisions=(d:DraftSummary)=>drafts.filter(o=>sameKey(o.key,d.key));
  const editing=!!openedDraft||!!selected||!!imported;
  // Describe jobs publish drafts in the session, so the overview catches up whenever the person returns to it.
  useEffect(()=>{if(editing||typeof window==="undefined"||typeof document==="undefined")return;const refresh=()=>{if(document.visibilityState!=="hidden")void runRef.current(()=>reload());};window.addEventListener("focus",refresh);document.addEventListener("visibilitychange",refresh);return()=>{window.removeEventListener("focus",refresh);document.removeEventListener("visibilitychange",refresh);};},[editing]);
  const artifact=openedDraft??selected?.package;
  // Expansion belongs to the landing, so expand/collapse all reach both tables and survive opening an API.
  const home={open:expanded,onToggle:(id:string)=>setExpanded(old=>{const next=new Set(old);if(!next.delete(id))next.add(id);return next;})};
  const libraryEmpty=libraryRead.state==="ready"&&packages.length+drafts.length===0;
  const homeTools=<><button type="button" className="screen-chip cell-action" onClick={()=>setExpanded(new Set([...imports.map(workspaceRowId),...libraryGroups(drafts,packages).map(g=>g.id)]))}>expand all</button><button type="button" className="screen-chip cell-action" onClick={()=>setExpanded(new Set())}>collapse all</button></>;
  return <Screen name="/spec" top={top} chrome={chrome} onClose={()=>guard(onClose)} subject={imported?[{text:`${imported.spec.alias} · ${imported.spec.environment} · imported snapshot`,role:"mono-dim"}]:openedDraft?draftSubject:selected?[{text:`${selected.descriptor.provider} · r${currentRevision} · ${dirty?"unsaved":valid?"valid":"validate required"} · ${selected.package.accepted?"reviewed":"not reviewed"}`,role:"mono-dim"}]:[{text:"API library",role:"mono-dim"}]} footer={leaving({text:"edits create revisions · imports and API calls are separate",role:"mono-faint"})} tools={editing?<div className="spec-tools"><button type="button" className="screen-chip cell-action" disabled={busy} onClick={backToLibrary}>← Back to library</button></div>:homeTools}>
    <div className="spec-body">
      {!editing&&<div className="spec-home">
      {binding&&<WorkspaceSpecs workspace={binding.workspace} specs={imports} read={importsRead} busy={busy} expansion={home} onOpen={selectImported} onRetry={()=>void run(()=>settled(listImports()))}/>}
      <SpecLibrary drafts={drafts} packages={packages} read={libraryRead} busy={busy} expansion={home} onDraft={selectDraft} onPackage={select} onRefresh={()=>void run(()=>reload())}/>
      <SpecDescribe location={location} provider={provider} onLocation={setLocation} onProvider={setProvider} open={describeOpen??libraryEmpty} accent={libraryEmpty} busy={busy}
        onToggle={()=>setDescribeOpen(!(describeOpen??libraryEmpty))} onDescribe={command=>guard(()=>void run(async()=>{const cell=await onSubmit(command);if(cell)onClose();}))}/>
      </div>}
      {imported&&<ImportedSpecInspector result={imported} workspace={binding?.workspace}/>}
      {artifact&&<header className="spec-editor-heading"><div><h2>{artifact.key.service}</h2><span>{openedDraft?`Draft ${revisionLabel(draftRevisions(openedDraft),draftRevision??openedDraft.revision)}`:`Saved API · r${currentRevision}`}</span></div><details className="spec-secondary"><summary>Source details</summary><SpecIdentity apiKey={artifact.key}/><SpecSource origin={artifact.origin} className="spec-editor-origin"/></details></header>}
      {openedDraft&&<DraftEditor key={`${JSON.stringify(openedDraft.key)}:${openedDraft.revision}`} opened={openedDraft} revisions={draftRevisions(openedDraft)} controls={draftControls} onSubject={setDraftSubject} onSaved={d=>{setDraftRevision(d.revision);void load().catch(e=>{setFailed(true);setMessage((e as Error).message);});}} onSubmit={onSubmit} onClose={onClose} workspace={binding?.workspace}/>}
      {selected&&!openedDraft&&<>
        <p role="status" className={`spec-state ${dirty ? "mono-warn" : "mono-ok"}`}>{dirty ? "Unsaved changes · preview shows the saved revision. Validate and save before import." : `r${currentRevision} saved · ready to import`}</p>
        <details className="spec-secondary"><summary>Details · review {selected.package.accepted ? "recorded" : "optional"} · {diagnostics.length} notes</summary><ProvenanceNotice provenance={provenance} dirty={dirty} revision={selected.package.revision}/><p className="spec-origin mono-faint">Revision sha256 {selected.package.revision}</p><button type="button" className="screen-chip cell-action" disabled={busy||dirty||selected.package.accepted} onClick={()=>void run(async()=>{accept(await libraryRequest({action:"accept",key:selected.package.key,revision:selected.package.revision}));await load();})}>mark reviewed</button></details>
        <div className="spec-tabs" role="tablist" aria-label="Saved API views">{TABS.map(t=><button type="button" role="tab" aria-selected={tab===t} key={t} className="spec-tab" data-count={t==="operations"?selected.descriptor.operations.length:t==="schema"?`${Object.keys(selected.descriptor.types??{}).length} types`:undefined} disabled={busy||(t==="import"&&dirty)} onClick={()=>setTab(t)}>{t}{t==="diagnostics"?` ${diagnostics.length}`:""}</button>)}</div>
        <div hidden={tab!=="operations"}><SpecOperations types={selected.descriptor.types} operations={descriptorPreview(selected.descriptor).operations} provenance={provenance} stale={dirty} responseTargets="status" onEvidence={pick} onSource={()=>setTab("source")} onType={name=>{setSchemaType({ name });setTab("schema");}} evidence={at=><ProvenanceDetail target={at} provenance={provenance}/>}/>{target&&tab==="operations"&&<ProvenanceDetail target={target} provenance={provenance}/>}</div>
        <div hidden={tab!=="schema"}><SchemaFieldsTable types={selected.descriptor.types} provenance={provenance} requestedType={schemaType} {...(target?{selected:target}:{})} onSelect={pick}/>{target&&tab==="schema"&&<ProvenanceDetail target={target} provenance={provenance}/>}</div>
        {tab==="source"&&<><div className="spec-tools"><button type="button" className="screen-chip cell-action" disabled={busy} onClick={()=>void run(async()=>{await validate();})}>validate</button><button type="button" className="screen-chip chip-chosen" disabled={busy||!dirty} onClick={()=>void run(async()=>{await save();})}>save revision</button><span className="mono-faint">Saving never imports or calls the API.</span></div><SpecSourceEditor key={`${selected.package.revision}`} source={source} onChange={change} onSave={()=>void run(async()=>{await save();})} onValidate={()=>void run(async()=>{await validate();})}/><div className="settings-field-row"><input className="settings-field" aria-label="Spec export path" value={exportPath} onChange={e=>setExportPath(e.target.value)} placeholder="Absolute path for a new spec file"/><button className="screen-chip cell-action" type="button" disabled={busy||dirty||!exportPath} onClick={()=>void run(async()=>{const r=await libraryRequest<{exportedPath:string}>({action:"exportDescriptor",key:selected.package.key,revision:selected.package.revision,file:exportPath});setMessage(`Exported ${r.exportedPath}`);})}>export</button></div></>}
        {tab==="diagnostics"&&<>{diagnostics.length?diagnostics.map((text,i)=><p key={i} className={valid?"mono-dim":"mono-bad"}>{text}</p>):<p className="mono-dim">{dirty&&!valid?"Validate the edited source first.":"No diagnostics."}</p>}<details><summary>Source evidence</summary><pre className="spec-detail">{JSON.stringify(selected.descriptor.source,null,2)}</pre></details></>}
        {tab==="import"&&<SpecImportForm name={selected.package.key.service} kind="saved API" revision={`r${currentRevision}`} workspace={binding?.workspace} descriptorPath={selected.descriptorPath} busy={busy} servers={selected.descriptor.servers?.length}
          state={dirty?{text:"● unsaved changes",tone:"mono-warn"}:{text:"ready to import",tone:"mono-ok"}}
          holds={dirty?[{tone:"mono-warn",title:"Import held.",detail:`Import uses saved text only; your edits after r${currentRevision} aren’t saved.`}]:[]}
          alias={alias} onAlias={setAlias} endpoint={endpoint} onEndpoint={setEndpoint} replace={replace} onReplace={setReplace}
          notes={<p className="spec-t-label">{credentials.length?`Credential slots: ${credentials.join(", ")}. Bind API credentials in /env; never use the describe key.`:"No API credential slots in this revision."}</p>}
          onSubmit={command=>{if(dirty)return;void run(async()=>{const cell=await onSubmit(command);if(cell)onClose();});}}/>}
      </>}
      {leavingDirty&&<div role="alertdialog" aria-label="Unsaved spec edits" className="spec-unsaved"><p>{openedDraft?"Unsaved draft edits. Save them (problems and all) or discard them before leaving.":"Unsaved spec edits. Save a revision or discard them before leaving."}</p><button className="screen-chip cell-action" onClick={()=>{setLeavingDirty(false);pending.current=undefined;}}>keep editing</button><button className="screen-chip cell-action" disabled={busy} onClick={()=>{const next=pending.current;pending.current=undefined;setLeavingDirty(false);if(selected)setSource(selected.source);next?.();}}>discard</button><button className="screen-chip chip-chosen" disabled={busy} onClick={()=>void run(async()=>{if(openedDraft?await draftControls.current?.save():await save()){const next=pending.current;pending.current=undefined;setLeavingDirty(false);next?.();}})}>{openedDraft?"save draft":"save revision"}</button></div>}
      {message&&<p role="status" className={`spec-message ${failed?"mono-bad":"mono-dim"}`}>{message}</p>}
    </div>
  </Screen>;
}
