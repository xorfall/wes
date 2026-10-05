/**
 * The operations of one API as a hairline table: method, route, operation, summary, inputs and
 * returns in their own columns, one line per operation. Opening a row unfolds its documentation,
 * inputs, body, responses, auth and evidence inside the table, under a quiet accent wash. Pointers
 * always use the operation's original index, so filtering never changes a target.
 */
import { useState, type ReactNode } from "react";
import { authProvenanceLabel, type SchemaProvenance } from "../api-library";
import type { PreviewOperation, PreviewParameter } from "../draft-api";
import { referencedType } from "./SchemaProvenance";
import { SpecDocumentation, documentationSummary } from "./SpecDocumentation";
import { evidenceSummary, inputsSummary, methodTone, responseShape, statusTone } from "./spec-table";
import "./spec-reader.css";

/** The operations table's columns, which an opened row's detail spans. */
const OPERATION_COLUMNS = 7;
const requiredText =(required: boolean | undefined) => required === undefined ? "unknown" : required ? "yes" : "no";
const requiredTone = (required: boolean | undefined) => required === undefined ? "mono-warn" : required ? "" : "mono-dim";

export function SpecOperations({ operations, types = {}, provenance, stale = false, problems = [], onSource, onEvidence, onType, evidence, responseTargets = "index" }: {
  operations: readonly PreviewOperation[]; types?: Record<string, unknown>; provenance?: SchemaProvenance; stale?: boolean;
  problems?: readonly { target: string; severity: string }[];
  onSource?: (pointer: string) => void; onEvidence?: (pointer: string) => void; onType?: (name: string) => void;
  evidence?: (pointer: string) => ReactNode; responseTargets?: "index" | "status";
}) {
  const [filter, setFilter] = useState("");
  const [open, setOpen] = useState<Set<number>>(new Set());
  const [evidenceOpen, setEvidenceOpen] = useState<Set<number>>(new Set());
  const shown = operations.filter(op => `${op.name} ${op.method} ${op.route} ${op.summary ?? ""} ${op.description ?? ""}`.toLowerCase().includes(filter.toLowerCase()));
  const openShown = shown.filter(op => open.has(op.index)).length;
  const flip = (set: (update: (old: Set<number>) => Set<number>) => void, id: number) => set(old => { const next = new Set(old); if (!next.delete(id)) next.add(id); return next; });
  const typeLink = (expression: string) => onType && referencedType(expression, types)
    ? <button type="button" className="spec-type-link" onClick={() => onType(expression)}>{expression} ›</button>
    : <code>{expression}</code>;

  return <section className="spec-reader" aria-label="Draft operations">
    <div className="spec-reader-tools"><input className="settings-field" aria-label="Filter spec operations" placeholder="Filter operations" value={filter} onChange={e => setFilter(e.target.value)} />
      <span className="spec-reader-count">{shown.length} of {operations.length}{openShown > 0 ? ` · ${openShown} open` : ""}</span><button type="button" className="spec-reader-link" onClick={() => setOpen(new Set())}>collapse all</button></div>
    {!operations.length ? <p>No operations in this text.</p> : !shown.length ? <p>No matching operations.</p> : <div role="table" aria-label="Operations" className="spec-t spec-ops">
      <div role="row" className="spec-t-row spec-ops-row spec-t-head">
        <div role="columnheader" className="spec-t-th spec-c-chev" />
        <div role="columnheader" className="spec-t-th spec-c-method">method</div>
        <div role="columnheader" className="spec-t-th spec-c-route">route<span className="spec-t-narrow"> · operation</span></div>
        <div role="columnheader" className="spec-t-th spec-c-op">operation</div>
        <div role="columnheader" className="spec-t-th spec-c-sum">summary</div>
        <div role="columnheader" className="spec-t-th spec-c-in" aria-description="? marks an input the document says is optional">inputs</div>
        <div role="columnheader" className="spec-t-th spec-c-ret">returns</div>
      </div>
      {shown.map(op => {
        const target = `#/operations/${op.index}`;
        const own = problems.filter(p => p.target === target || p.target.startsWith(`${target}/`));
        const blocking = own.filter(p => p.severity === "error").length;
        const expanded = open.has(op.index);
        const summary = documentationSummary(op.summary, op.description);
        const inputs = op.parameters.filter(p => p.location !== "body");
        const bodies = op.parameters.filter(p => p.location === "body");
        const inputText = inputsSummary(op.parameters);
        const first = op.responses[0];
        const toggle = () => flip(setOpen, op.index);
        const evidenceButton = (label: string, pointer: string) => onEvidence && <button type="button" className="spec-evidence-link" aria-label={label} onClick={() => onEvidence(pointer)}>evidence</button>;
        const parameterPointer = (p: PreviewParameter) => `${target}/parameters/${op.parameters.indexOf(p)}`;
        const bodyNamed = bodies.length > 1 || bodies.some(p => p.name !== "body");
        const documentedOnly = Object.entries(op.responseDescriptions ?? {}).filter(([status]) => !op.responses.some(r => String(r.status) === status));
        const status = stale ? <span className="spec-row-status mono-faint">stale</span>
          : own.length > 0 && <span className={`spec-row-status ${blocking ? "mono-bad" : "mono-warn"}`}>{blocking ? `${blocking} blocking` : `${own.length} advisory`}</span>;
        return <div role="rowgroup" className={`spec-t-group ${expanded ? "spec-t-open" : ""}`} key={op.index}>
          <div role="row" className="spec-t-row spec-ops-row">
            <div role="cell" className="spec-t-td spec-c-chev"><button type="button" className="spec-t-chev" aria-expanded={expanded} aria-label={`${expanded ? "Collapse" : "Expand"} operation ${op.name}`} onClick={toggle}>{expanded ? "▾" : "▸"}</button></div>
            <div role="cell" className="spec-t-td spec-c-method"><span className={`spec-method ${methodTone(op.method)}`}>{op.method}</span></div>
            <div role="cell" className="spec-t-td spec-c-route" aria-description={op.route}><code className="spec-operation-route">{op.route}</code></div>
            <div role="cell" className="spec-t-td spec-c-op"><button type="button" tabIndex={-1} className="spec-t-rowbtn spec-operation-name" aria-description={op.name} onClick={toggle}>{op.name}</button>{status && <span className="spec-t-narrow spec-row-narrow">{status}</span>}</div>
            <div role="cell" className="spec-t-td spec-c-sum" aria-description={summary || undefined}>{status}<span className="spec-doc-summary">{summary}</span></div>
            <div role="cell" className="spec-t-td spec-c-in" aria-description={inputText || undefined}>{inputText || <span className="mono-faint">—</span>}</div>
            <div role="cell" className="spec-t-td spec-c-ret">{first
              ? <><span className={statusTone(first.status)}>{first.status ?? "?"}</span><span className="mono-dim spec-ret-shape">{` · ${responseShape(first, types)}`}</span></>
              : <span className="mono-dim">no responses</span>}</div>
          </div>
          {expanded && <div role="row" className="spec-t-detail-row"><div role="cell" aria-colspan={OPERATION_COLUMNS} className="spec-t-detail">
            {summary && <><span className="spec-t-label spec-t-narrow">summary</span><p className="spec-t-prose spec-t-narrow">{summary}</p></>}
            {!!op.description?.trim() && <><span className="spec-t-label">description</span><SpecDocumentation text={op.description} /></>}

            {inputs.length > 0 && <><span className="spec-t-label">inputs</span>
              <div role="table" aria-label={`Inputs of ${op.name}`} className="spec-t spec-t-sub spec-inputs">
                <div role="row" className="spec-t-row spec-inputs-row spec-t-head">
                  <div role="columnheader" className="spec-t-th spec-c-name">name</div><div role="columnheader" className="spec-t-th spec-c-loc">in</div>
                  <div role="columnheader" className="spec-t-th spec-c-type">type</div><div role="columnheader" className="spec-t-th spec-c-req"><span className="spec-t-wide">required</span><span className="spec-t-narrow">req.</span></div>
                  <div role="columnheader" className="spec-t-th spec-c-desc">description</div>
                </div>
                {inputs.map(p => <div role="row" className="spec-t-row spec-inputs-row" key={parameterPointer(p)}>
                  <div role="cell" className="spec-t-tdw spec-c-name"><span className="spec-t-name">{p.name}</span><span className="spec-t-label spec-t-narrow">{` ${p.location}`}</span></div>
                  <div role="cell" className="spec-t-td spec-c-loc mono-dim">{p.location}</div>
                  <div role="cell" className="spec-t-tdw spec-c-type">{typeLink(p.type)}</div>
                  <div role="cell" className={`spec-t-td spec-c-req ${requiredTone(p.required)}`}>{requiredText(p.required)}</div>
                  <div role="cell" className="spec-t-tdw spec-c-desc"><SpecDocumentation text={p.description} />{evidenceButton(`Provenance of parameter ${p.name} of ${op.name}`, parameterPointer(p))}</div>
                </div>)}
              </div></>}

            {bodies.length > 0 && <><span className="spec-t-label">body</span>
              <div role="table" aria-label={`Request body of ${op.name}`} className={`spec-t spec-t-sub spec-body-table ${bodyNamed ? "spec-body-named" : ""}`}>
                <div role="row" className="spec-t-row spec-body-row spec-t-head">
                  {bodyNamed && <div role="columnheader" className="spec-t-th spec-c-name">name</div>}
                  <div role="columnheader" className="spec-t-th spec-c-type">type</div><div role="columnheader" className="spec-t-th spec-c-req"><span className="spec-t-wide">required</span><span className="spec-t-narrow">req.</span></div>
                  <div role="columnheader" className="spec-t-th spec-c-desc">description</div>
                </div>
                {bodies.map(p => <div role="row" className="spec-t-row spec-body-row" key={parameterPointer(p)}>
                  {bodyNamed && <div role="cell" className="spec-t-tdw spec-c-name"><span className="spec-t-name">{p.name}</span></div>}
                  <div role="cell" className="spec-t-tdw spec-c-type">{typeLink(p.type)}</div>
                  <div role="cell" className={`spec-t-td spec-c-req ${requiredTone(p.required)}`}>{requiredText(p.required)}</div>
                  <div role="cell" className="spec-t-tdw spec-c-desc"><SpecDocumentation text={p.description} />{evidenceButton(`Provenance of parameter ${p.name} of ${op.name}`, parameterPointer(p))}</div>
                </div>)}
              </div></>}

            <span className="spec-t-label">responses</span>
            {op.responses.length + documentedOnly.length === 0 ? <p className="spec-t-note">No responses in this text.</p>
              : <div role="table" aria-label={`Responses of ${op.name}`} className="spec-t spec-t-sub spec-responses">
                <div role="row" className="spec-t-row spec-responses-row spec-t-head">
                  <div role="columnheader" className="spec-t-th spec-c-status">status</div><div role="columnheader" className="spec-t-th spec-c-desc">description</div><div role="columnheader" className="spec-t-th spec-c-type">type</div>
                </div>
                {op.responses.map((r, j) => <div role="row" className="spec-t-row spec-responses-row" key={j}>
                  <div role="cell" className={`spec-t-td spec-c-status spec-t-strong ${statusTone(r.status)}`}>{r.status ?? "unknown status"}</div>
                  <div role="cell" className="spec-t-tdw spec-c-desc"><SpecDocumentation text={op.responseDescriptions?.[String(r.status)]} />{r.mediaType && <span className="spec-media-type">{r.mediaType}</span>}
                    {evidenceButton(`Provenance of response ${r.status} of ${op.name}`, `${target}/responses/${responseTargets === "status" ? r.status : j}`)}</div>
                  <div role="cell" className="spec-t-tdw spec-c-type">{r.type ? typeLink(r.type) : <span className="mono-dim">{r.type === null ? "empty body" : "unknown shape"}</span>}</div>
                </div>)}
                {documentedOnly.map(([code, description]) => <div role="row" className="spec-t-row spec-responses-row" key={code}>
                  <div role="cell" className="spec-t-td spec-c-status spec-t-strong mono-dim">{code}</div>
                  <div role="cell" className="spec-t-tdw spec-c-desc"><SpecDocumentation text={description} /></div>
                  <div role="cell" className="spec-t-tdw spec-c-type"><span className="mono-dim">documented · no result contract</span></div>
                </div>)}
              </div>}

            <span className="spec-t-label">auth</span>
            <div role="group" className="spec-auth" aria-label={`Auth of ${op.name}`}><p>{authProvenanceLabel(op.auth, provenance, op.index)}{onEvidence && " "}{evidenceButton(`Auth provenance of ${op.name}`, `${target}/auth`)}</p>
              <p className="spec-t-note">Credentials are configured in /env. No credentials attached does not establish public access.</p></div>

            {(evidence || onSource) && <><span className="spec-t-label">evidence</span>
              <div className="spec-t-evidence">
                <div className="spec-t-line">
                  {evidence && <button type="button" className="spec-t-layer" aria-expanded={evidenceOpen.has(op.index)} onClick={() => flip(setEvidenceOpen, op.index)}><span className="mono-ref">{evidenceOpen.has(op.index) ? "▾" : "▸"}</span>Evidence<span className="mono-dim">{` · ${evidenceSummary(provenance, target)}`}</span></button>}
                  <span className="spec-t-fill" />
                  {onSource && <button type="button" className="spec-t-action" onClick={() => onSource(target)}>go to source</button>}
                </div>
                {evidence && evidenceOpen.has(op.index) && evidence(target)}
              </div></>}
          </div></div>}
        </div>;
      })}
    </div>}
    {operations.length > 0 && <p className="spec-t-footnote">Descriptions come from the source document. A row without one shows nothing in its place.</p>}
  </section>;
}
