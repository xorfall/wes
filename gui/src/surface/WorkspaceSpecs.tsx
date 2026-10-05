import { useState } from "react";
import type { ImportedSpec, ImportedSpecResult } from "../workspace-specs";
import { ProvenanceDetail, SchemaFieldsTable } from "./SchemaProvenance";
import { CopyButton, formatBytes } from "./spec-table";
import { readSchemaProvenance, type Descriptor } from "../api-library";
import { descriptorPreview } from "../draft-api";
import { SpecOperations } from "./SpecOperations";
import { SpecSourceEditor } from "./SpecSourceEditor";
import { HomeCard, HomeReadGate, HomeTitle, shortSource, workspaceRowId, type HomeExpansion, type HomeRead } from "./spec-home";

/** A snapshot's revision is its content digest; a closed row and the heading show its start, the details all of it. */
const DIGEST_PREVIEW = 8;
/** An opened row's detail spans every column of the imports table. */
const WORKSPACE_COLUMNS = 7;

/**
 * The imports of the bound workspace across its environments, one row per environment and alias.
 * Opening a row reads the captured contract only: never the original file, the library or the API.
 */
export function WorkspaceSpecs({ workspace, specs, read, busy, expansion, onOpen, onRetry }: {
  workspace: string; specs: ImportedSpec[]; read: HomeRead; busy: boolean; expansion: HomeExpansion;
  onOpen: (spec: ImportedSpec) => void; onRetry: () => void;
}) {
  return <HomeCard label="Workspace APIs" head={<>
    <HomeTitle title="Workspace APIs" count={read.state === "ready" ? specs.length : undefined} />
    <span className="spec-home-note"><span className="spec-home-wide">contracts captured at import, per environment · </span>read-only</span>
  </>}>
    <HomeReadGate read={read} what="this workspace’s imports" empty={specs.length === 0} emptyText="Nothing imported in this workspace yet." busy={busy} onRetry={onRetry}>
      <div className="spec-home-t" role="table" aria-label={`Imports in ${workspace}`}>
        <div className="spec-home-row spec-home-ws" role="row">
          <div className="spec-home-th" role="columnheader" /><div className="spec-home-th" role="columnheader">alias</div>
          <div className="spec-home-th" role="columnheader" aria-label="environment"><span className="spec-home-wide">environment</span><span className="spec-home-narrow">env</span></div><div className="spec-home-th spec-home-wide" role="columnheader">revision</div>
          <div className="spec-home-th spec-home-wide" role="columnheader">size</div><div className="spec-home-th spec-home-wide" role="columnheader">captured from</div>
          <div className="spec-home-th" role="columnheader" aria-label="Action" />
        </div>
        {specs.map(spec => <WorkspaceRow key={workspaceRowId(spec)} spec={spec} busy={busy} expansion={expansion} onOpen={onOpen} />)}
      </div>
    </HomeReadGate>
  </HomeCard>;
}

function WorkspaceRow({ spec, busy, expansion, onOpen }: { spec: ImportedSpec; busy: boolean; expansion: HomeExpansion; onOpen: (spec: ImportedSpec) => void }) {
  const id = workspaceRowId(spec);
  const expanded = expansion.open.has(id);
  const toggle = () => expansion.onToggle(id);
  const origin = spec.origin ? shortSource(spec.origin) : <span className="mono-faint">not recorded</span>;
  return <div className={expanded ? "spec-home-open" : undefined} role="rowgroup">
    <div className="spec-home-row spec-home-ws" role="row">
      <div className="spec-home-td spec-home-c-chev" role="cell"><button type="button" className="spec-home-chev" aria-expanded={expanded} aria-label={`${expanded ? "Collapse" : "Expand"} ${spec.alias} in ${spec.environment}`} onClick={toggle}>{expanded ? "▾" : "▸"}</button></div>
      <div className="spec-home-td" role="cell" aria-description={spec.alias}>
        <span className="spec-home-line"><button type="button" className="spec-home-name" tabIndex={-1} onClick={toggle}>{spec.alias}</button></span>
        <span className="spec-home-line spec-home-narrow mono-dim" aria-description={spec.origin}>{origin}</span>
      </div>
      <div className="spec-home-td" role="cell" aria-description={spec.environment}>{spec.environment}</div>
      <div className="spec-home-td spec-home-wide mono-dim" role="cell" aria-description={`sha256:${spec.revision}`}>{spec.revision.slice(0, DIGEST_PREVIEW)}</div>
      <div className="spec-home-td spec-home-wide mono-dim" role="cell">{formatBytes(spec.bytes)}</div>
      <div className="spec-home-td spec-home-wide mono-dim" role="cell" aria-description={spec.origin}>{origin}</div>
      <div className="spec-home-td spec-home-c-act" role="cell"><button type="button" className="screen-chip cell-action" disabled={busy} aria-label={`Open imported ${spec.alias} in ${spec.environment}`} onClick={() => onOpen(spec)}>open</button></div>
    </div>
    {expanded && <div role="row"><div className="spec-home-detail" role="cell" aria-colspan={WORKSPACE_COLUMNS}>
      <span className="spec-home-label">captured from</span>
      <span className="spec-home-full">{spec.origin ? <>{spec.origin}<span className="spec-home-copy"><CopyButton text={spec.origin} label="Copy captured path" /></span></> : <span className="mono-faint">not recorded</span>}</span>
      <span className="spec-home-label">revision</span>
      <span className="spec-home-full mono-dim">{`sha256:${spec.revision}`}<span className="spec-home-copy"><CopyButton text={spec.revision} label="Copy revision" /></span></span>
      <span className="spec-home-label spec-home-narrow">size</span>
      <span className="spec-home-full spec-home-narrow mono-dim">{`${formatBytes(spec.bytes)} · ${spec.bytes} bytes`}</span>
      <span className="spec-home-label" /><span className="spec-home-label">Open shows the contract as captured. It doesn’t call the API, and a later change to the file or the library doesn’t reach it.</span>
    </div></div>}
  </div>;
}
const unchanged = () => {};

/**
 * One imported snapshot, read-only: a compact heading with its alias, environment, digest, size and
 * origin exactly as captured, then the same operations, schema and source views as the library.
 * Nothing here reads the original file or the API, and nothing is inferred beyond the capture.
 */
export function ImportedSpecInspector({ result, workspace }: { result: ImportedSpecResult; workspace?: string | undefined }) {
  const [tab, setTab] = useState<"operations" | "schema" | "source">("operations");
  const [schemaType, setSchemaType] = useState<{ name: string }>();
  const [target, setTarget] = useState<string>();
  const [details, setDetails] = useState(false);
  // Legacy descriptors are still inspectable as exact source; never reinterpret or upgrade them.
  const parsed = JSON.parse(result.source.trimStart());
  const descriptor: Descriptor | undefined = parsed.version === 1 ? parsed : undefined;
  const provenance = readSchemaProvenance(descriptor?.source);
  const { spec } = result;
  const pick = (next: string) => setTarget(old => old === next ? undefined : next);
  return <section className="spec-snapshot" aria-label="Imported API snapshot">
    <p className="spec-snapshot-cap"><span className="spec-snapshot-kicker">workspace · imported snapshot</span></p>
    <header className="spec-snapshot-card">
      <div className="spec-snapshot-head">
        <h2>{spec.alias}</h2><span className="spec-snapshot-kind">imported snapshot</span><span>{`in ${spec.environment}`}</span><span className="mono-faint">·</span>
        <span className="mono-dim" aria-description={`sha256:${spec.revision}`}>{`${spec.revision.slice(0, DIGEST_PREVIEW)}…`}</span><CopyButton text={spec.revision} label="Copy full snapshot revision" />
        <span className="spec-snapshot-badge">read-only</span>
      </div>
      <p className="spec-snapshot-origin">{spec.origin ? `captured from ${spec.origin} · ${formatBytes(spec.bytes)}` : `captured · ${formatBytes(spec.bytes)}`}</p>
      <p className="spec-snapshot-note">{`${workspace ? `This is what ${workspace} calls as ${spec.alias} in ${spec.environment}. ` : ""}Nothing here can be edited or saved, and opening it doesn’t call the API.`}</p>
      <div className="spec-snapshot-layers">
        <button type="button" className="spec-t-layer" aria-expanded={details} onClick={() => setDetails(open => !open)}><span className="mono-ref">{details ? "▾" : "▸"}</span>Captured details<span className="mono-dim">{formatBytes(spec.bytes)}</span></button>
      </div>
      {details && <dl className="spec-snapshot-details">
        <dt>revision</dt><dd><code>{`sha256 ${spec.revision}`}</code></dd>
        <dt>origin</dt><dd>{spec.origin || <span className="mono-faint">not recorded</span>}</dd>
        <dt>size</dt><dd>{`${spec.bytes} bytes`}</dd>
        <dt>scope</dt><dd>Changing the original file or a library revision does not change this import. Import a revised spec explicitly to replace it.</dd>
      </dl>}
    </header>
    <div className="spec-tabs" role="tablist" aria-label="Imported API views">{(["operations", "schema", "source"] as const).map(t => <button type="button" role="tab" aria-selected={tab === t} key={t} className="spec-tab" data-count={descriptor ? t === "operations" ? descriptor.operations.length : t === "schema" ? `${Object.keys(descriptor.types ?? {}).length} types` : undefined : undefined} onClick={() => setTab(t)}>{t}</button>)}</div>
    {tab === "source" && <SpecSourceEditor source={result.source} readOnly onChange={unchanged} onSave={unchanged} onValidate={unchanged}/>}
    {!descriptor ? <p className="settings-note">Open source to inspect this captured contract.</p> : <>
      <div hidden={tab !== "schema"}><SchemaFieldsTable types={descriptor.types ?? {}} provenance={provenance} requestedType={schemaType} {...(target ? { selected: target } : {})} onSelect={pick}/>{target && tab === "schema" && <ProvenanceDetail target={target} provenance={provenance}/>}</div>
      <div hidden={tab !== "operations"}><SpecOperations types={descriptor.types} operations={descriptorPreview(descriptor).operations} provenance={provenance} responseTargets="status" onSource={() => setTab("source")} evidence={at => <ProvenanceDetail target={at} provenance={provenance}/>} onType={name => { setSchemaType({ name }); setTab("schema"); }}/></div>
    </>}
  </section>;
}
