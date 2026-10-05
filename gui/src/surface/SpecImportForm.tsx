/**
 * The one import form for drafts and saved APIs: a compact accent-bordered card that captures the
 * saved revision under an alias with an explicitly typed endpoint. Field problems come from the same
 * validation authority as the command itself (`importSpecProblems`), shown under the field they
 * concern. While the revision is held, the typed values are shown read-only and kept as they are.
 *
 * The command runs in the session's current environment; this form never chooses one. Submitting
 * hands the command to the session, whose own result says whether the import worked.
 */
import { useId, useState, type ReactNode } from "react";
import { importSpecCommand, importSpecProblems } from "../api-library";

/** One reason the revision cannot be imported now, and what the person can do about it. */
export interface ImportHold { title: string; detail: string; tone: string; action?: ReactNode }

export interface SpecImportFormProps {
  name: string;
  kind: string;
  revision: string;
  state: { text: string; tone: string };
  /** The bound workspace, when the screen knows it; the environment is always the session's own. */
  workspace?: string | undefined;
  /** The executable descriptor for the saved revision, absent when none was prepared. */
  descriptorPath?: string | undefined;
  holds: readonly ImportHold[];
  busy: boolean;
  alias: string; onAlias: (value: string) => void;
  endpoint: string; onEndpoint: (value: string) => void;
  replace: boolean; onReplace: (value: boolean) => void;
  /** How many servers the document lists; none is ever filled in for the person. */
  servers?: number | undefined;
  notes?: ReactNode;
  onSubmit: (command: string) => void;
}

type Field = "alias" | "endpoint";
const plural = (count: number, one: string, many: string) => `${count} ${count === 1 ? one : many}`;

export function SpecImportForm({ name, kind, revision, state, workspace, descriptorPath, holds, busy, alias, onAlias, endpoint, onEndpoint, replace, onReplace, servers = 0, notes, onSubmit }: SpecImportFormProps) {
  const id = useId();
  const [touched, setTouched] = useState<ReadonlySet<Field>>(new Set());
  const touch = (field: Field) => setTouched(old => old.has(field) ? old : new Set([...old, field]));
  const held = holds.length > 0;
  const problems = importSpecProblems(alias, endpoint);
  // A problem is shown once the field has something in it, or once the person has left it.
  const shown = (field: Field, value: string) => (value !== "" || touched.has(field)) ? problems[field] : undefined;
  const aliasError = shown("alias", alias);
  const endpointError = shown("endpoint", endpoint);
  const visibleErrors = [aliasError, endpointError].filter(Boolean).length;
  let command = "";
  let commandError = "";
  try { if (descriptorPath) command = importSpecCommand(descriptorPath, alias, endpoint, replace); else commandError = "No executable descriptor for this revision."; }
  catch (e) { commandError = (e as Error).message; }
  const ready = !held && !!command;
  const hint = held ? "" : visibleErrors ? `fix the ${plural(visibleErrors, "field", "fields")} above` : problems.endpoint ? "type the endpoint to import" : problems.alias ? "type an alias to import" : "";
  const into = <span className="spec-import-into">{workspace && <span className="spec-import-chip">{workspace}</span>}<span className="spec-t-label">the session’s current environment · change it in /env</span></span>;

  return <form className="spec-import-form" aria-label={`Import ${name}`} onSubmit={e => { e.preventDefault(); if (!ready || busy) return; onSubmit(command); }}>
    <div className="spec-import-head">
      <span className="spec-import-name">{name}</span><span className="spec-import-kind">{kind}</span><span>{revision}</span>
      {state.tone === "mono-ok" ? <span className="spec-import-badge mono-ok">{state.text}</span> : <span className={state.tone}>{state.text}</span>}
    </div>
    <p className="spec-import-title">{`Import saved ${revision}`}</p>
    {holds.map((hold, i) => <div key={i} className={`spec-import-strip ${hold.tone}-strip`} role="status"><span className={hold.tone}>{hold.title}</span><span>{hold.detail}</span>{hold.action}</div>)}

    {held ? <div className="spec-import-grid spec-import-held">
      <span className="spec-t-label">into</span>{into}
      <span className="spec-t-label">alias</span><span className="spec-import-value">{alias || "—"}</span>
      <span className="spec-t-label">endpoint</span><span className="spec-import-value">{endpoint || "—"}</span>
    </div> : <div className="spec-import-grid">
      <span className="spec-t-label">into</span>{into}
      <label className="spec-t-label" htmlFor={`${id}-alias`}>alias</label>
      <div className="spec-import-field">
        <input id={`${id}-alias`} className="spec-import-input spec-import-alias" aria-label="Import alias" aria-invalid={!!aliasError} aria-describedby={aliasError ? `${id}-alias-error` : undefined} spellCheck={false} autoCapitalize="off" autoComplete="off"
          value={alias} onChange={e => onAlias(e.target.value)} onBlur={() => touch("alias")} />
        {aliasError && <span id={`${id}-alias-error`} className="spec-import-error mono-bad">{aliasError}</span>}
      </div>
      <label className="spec-t-label" htmlFor={`${id}-endpoint`}>endpoint</label>
      <div className="spec-import-field">
        <input id={`${id}-endpoint`} className="spec-import-input" aria-label="Import endpoint" aria-invalid={!!endpointError} aria-describedby={endpointError ? `${id}-endpoint-error` : undefined} spellCheck={false} autoCapitalize="off" autoComplete="off"
          placeholder="https://api.example.com/v1" value={endpoint} onChange={e => onEndpoint(e.target.value)} onBlur={() => touch("endpoint")} />
        {endpointError ? <span id={`${id}-endpoint-error`} className="spec-import-error mono-bad">{endpointError}</span>
          : servers > 0 && <span className="spec-t-label">{`The document lists ${servers === 1 ? "a server" : `${servers} servers`}; ${servers === 1 ? "it’s" : "one is"} used only if you type it here.`}</span>}
      </div>
      <span className="spec-t-label">existing</span>
      <label className="spec-import-check"><input type="checkbox" checked={replace} onChange={e => onReplace(e.target.checked)} /><span>{`replace ${alias || "this alias"} if it already exists in that environment`}</span></label>
    </div>}

    <div className="spec-import-foot">
      {!held && !visibleErrors && <p className="spec-t-label">{`Captures saved ${revision} and this endpoint as a read-only snapshot. The API isn’t called. Credentials are set up in /env.`}</p>}
      {!held && notes}
      <div className="spec-import-actions">
        <details className="spec-import-technical">
          <summary>technical details</summary>
          <div className="spec-import-grid">
            <span className="spec-t-label">source</span><code>{descriptorPath ?? "no descriptor"}</code>
            <span className="spec-t-label">command</span><code className="spec-command mono-meta">{held ? "" : command || commandError}</code>
          </div>
        </details>
        {hint && <span className="spec-t-label">{hint}</span>}
        <button className="spec-import-submit" disabled={!ready || busy}>import</button>
      </div>
    </div>
  </form>;
}
