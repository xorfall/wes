import { useEffect, useState } from "react";
import type { Engine, SandboxObservation, ScopeResultSource } from "../engine";
import { isNumeric, numericText } from "../exact-json";
import type { StoredValue } from "../protocol";
import { ValueView } from "../views/Result";
import { StorageError } from "../storage-error";
import { resultFailure } from "../result-failure";

/** The fields of a backend-generated full observation and of each of its member rows. */
const OBSERVATION_FIELDS = ["kind", "members", "persistence", "state"];
const MEMBER_FIELDS = new Set(["name", "type", "state", "value", "error", "node", "revision", "retainedReference", "availability", "evidence"]);
const WITHHELD = "withheld";
const REMOVED = "removed";
const ERROR_SUMMARY_FIELDS = new Set(["code", "message"]);
const UNKNOWN_TYPE = { kind: "unknown" } as const;

/** One named member as the engine described it; `value` is present only when the read carried one. */
export interface SandboxMember {
  readonly name: string;
  readonly state: string;
  readonly type?: string;
  readonly value?: { readonly data: unknown };
  readonly error?: unknown;
  readonly source?:ScopeResultSource;
  readonly evidence?:string;
}

const isRecord = (data: unknown): data is Readonly<Record<string, unknown>> =>
  typeof data === "object" && data !== null && !Array.isArray(data) && !isNumeric(data);

/**
 * The members of a full sandbox observation, or nothing when this is not one.
 *
 * A read of a member (`$box.member.field`) returns that member's own data, which may be any user
 * value — including one shaped like a sandbox — so it is never interpreted. Only a whole-sandbox
 * read or an inspection is, and only when its data has exactly the backend's shape. Withheld rows
 * carry a name and a state and nothing else; a row that says more is not trusted as withheld.
 */
export function sandboxMembers(observation: SandboxObservation): readonly SandboxMember[] | undefined {
  const reference = observation.reference;
  if (reference !== null && reference.includes(".") && observation.inspect !== true) return undefined;
  const data = observation.value.data;
  const kind=observation.kind;
  const expected=kind==="saved"?[...OBSERVATION_FIELDS,"identity"].sort():OBSERVATION_FIELDS;
  if (!isRecord(data) || Object.keys(data).sort().join() !== expected.join()) return undefined;
  const label=kind==="sandbox"?"Sandbox":kind==="transient"?"Transient":"Saved";
  const persistence=kind==="sandbox"?"definition only":kind==="transient"?"temporary until Keep":"retained snapshot";
  if(data.kind!==label||data.persistence!==persistence||typeof data.state!=="string"||!Array.isArray(data.members))return undefined;
  const members: SandboxMember[] = [];
  for (const row of data.members) {
    if (!isRecord(row) || typeof row.name !== "string" || typeof row.state !== "string") return undefined;
    const fields = Object.keys(row);
    if (fields.some(field => !MEMBER_FIELDS.has(field))) return undefined;
    if (row.state === WITHHELD) {
      if (fields.length !== 2) return undefined;
      members.push({ name: row.name, state: WITHHELD });
      continue;
    }
    if (typeof row.type !== "string") return undefined;
    members.push({ name: row.name, state: row.state, type: row.type, source:observation.sources?.[row.name],
      ...("value" in row ? { value: { data: row.value } } : {}), ...("error" in row ? { error: row.error } : {}), ...(typeof row.evidence==="string"?{evidence:row.evidence}:{}) });
  }
  return members;
}

/** A scalar's text, quoted as JSON for strings; complex values are drawn by the value view instead. */
function scalarText(data: unknown): string | undefined {
  if (data === null) return "null";
  if (typeof data === "string") return JSON.stringify(data);
  if (typeof data === "boolean") return String(data);
  if (isNumeric(data)) return numericText(data);
  return undefined;
}

/** What an engine error record says in one line, when it has a message; otherwise it is drawn whole. */
function errorText(error: unknown): string | undefined {
  if (typeof error === "string") return error;
  if (!isRecord(error) || typeof error.message !== "string") return undefined;
  return typeof error.code === "string" && error.code !== "" ? `${error.code} · ${error.message}` : error.message;
}

/** Whether an error record carries more than the code and message its summary already shows. */
function hasErrorDetails(error: unknown): boolean {
  return isRecord(error) && Object.keys(error).some(field => !ERROR_SUMMARY_FIELDS.has(field));
}

/** Why a member shows no value: the read did not carry one, and the row says which kind of read it was. */
function absence(member: SandboxMember, inspect: boolean): string {
  if (member.state === WITHHELD) return "withheld";
  if (inspect) return "not read when inspecting";
  return "no current value";
}

function MemberValue({ member, inspect, engine, cacheKey, provenance, generation, readScope }: {generation:string;readScope?:string;
  member: SandboxMember; inspect: boolean; engine: Engine; cacheKey: string; provenance: StoredValue["provenance"];
}) {
  const error = member.error === undefined ? undefined : errorText(member.error);
  const errorView = <ValueView engine={engine} cacheKey={`${cacheKey}:error`} value={{ type: UNKNOWN_TYPE, provenance, data: member.error }} />;
  return <>
    {member.evidence && <p className="cell-quiet">{member.evidence==="incomplete"?"Incomplete captured evidence":"Captured stopped observation"}</p>}
    {member.value === undefined ? <span className="sandbox-member-absent">{absence(member, inspect)}</span>
      : scalarText(member.value.data) !== undefined ? <code className="sandbox-member-scalar">{scalarText(member.value.data)}</code>
        : <div className="sandbox-member-view"><ValueView engine={engine} cacheKey={cacheKey} readScope={readScope} stored={member.source?.handle?{handle:member.source.handle,generation,scope:member.source.scope??undefined,select:member.source.select}:undefined} value={{ type: member.source?.type??UNKNOWN_TYPE, provenance, data: member.value.data, ...(member.source?.meta?{meta:member.source.meta}:{}) }} /></div>}
    {member.error !== undefined && (error !== undefined ? <>
      <p className="sandbox-member-error" role="note">{error}</p>
      {/* The summary names the error; issues, locations and anything else it carried stay one disclosure away. */}
      {hasErrorDetails(member.error) && <details className="sandbox-member-error-details"><summary>error details</summary><div className="sandbox-member-view">{errorView}</div></details>}
    </> : <div className="sandbox-member-error sandbox-member-view">{errorView}</div>)}
  </>;
}

/** Named members, readable at once: a table where there is room, one stacked card per member where not. */
function SandboxMembers({ members, inspect, engine, keyPrefix, provenance, generation, readScope }: {generation:string;readScope?:string;
  members: readonly SandboxMember[]; inspect: boolean; engine: Engine; keyPrefix: string; provenance: StoredValue["provenance"];
}) {
  if (members.length === 0) return <p className="cell-quiet">This sandbox has no named members.</p>;
  return <div className="sandbox-members-frame">
    <table className="sandbox-members">
      <caption className="sandbox-members-caption">{members.length === 1 ? "1 member" : `${members.length} members`}</caption>
      <thead><tr><th scope="col">Member</th><th scope="col">Type</th><th scope="col">State</th><th scope="col">Value</th></tr></thead>
      <tbody>{members.map(member => <tr key={member.name} data-state={member.state}>
        <th scope="row" data-label="Member"><code>${member.name}</code></th>
        <td data-label="Type">{member.type === undefined ? <span className="sandbox-member-absent" aria-label="type withheld">—</span> : <code>{member.type}</code>}</td>
        <td data-label="State"><span className="sandbox-member-state">{member.state}</span></td>
        <td data-label="Value"><MemberValue member={member} inspect={inspect} engine={engine} cacheKey={`${keyPrefix}:${member.name}`} provenance={provenance} generation={generation} readScope={readScope} /></td>
      </tr>)}</tbody>
    </table>
  </div>;
}

/** What closing the view means for the sandbox, from the observation's own state; only an active one has work to cancel. */
function lifecycleNotice(state: SandboxObservation["state"], name: string, kind:SandboxObservation["kind"]): string {
  if(kind==="saved")return state==="unconfirmed"?"Keep was not acknowledged. Inspect this record before trying another save.":"This saved snapshot is immutable. Reading it does not run its providers.";
  const label=kind==="transient"?"Transient analysis":"Sandbox";
  if (state === "stopped") return `${label} stopped. Use :refresh $${name} to run it again.`;
  if (state === "not run") return `Only the definition was restored. Use :refresh $${name} to run it.`;
  if (state === REMOVED) return `${label} removed. Nothing here is running or can be cancelled; define $${name} again to use it.`;
  return `Closing this view leaves the ${kind==="transient"?"transient analysis":"sandbox"} active. Use :cancel $${name} to stop it.`;
}

/** A current observation, never scrollback, persisted output or a source submission loop. */
export function SandboxPanel({ engine, opened, onClose }: {
  engine: Engine; opened: SandboxObservation; onClose: () => void;
}) {
  const [current, setCurrent] = useState(opened);
  const [trouble, setTrouble] = useState<{ readonly message: string; readonly code?: string }>();
  useEffect(() => {
    setCurrent(opened); setTrouble(undefined);
    if (!opened.reference) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const read = async () => {
      let withdrawn = false;
      try {
        if (typeof document !== "undefined" && document.hidden) return;
        const next = await engine.readSandbox(opened.reference!, opened.inspect === true, opened.generation, controller.signal);
        if (!controller.signal.aborted) { setCurrent(next); setTrouble(undefined); }
      } catch (error) {
        if (!controller.signal.aborted) {
          const code = error instanceof StorageError ? error.code : undefined;
          withdrawn = code === "ANALYSIS_ACCESS_WITHDRAWN";
          setTrouble({ message: error instanceof Error ? error.message : "Analysis observation unavailable.", code });
        }
      } finally {
        if (!controller.signal.aborted && !withdrawn) timer = setTimeout(read, 1000);
      }
    };
    timer = setTimeout(read, 1000);
    return () => { controller.abort(); clearTimeout(timer); };
  }, [engine, opened]);
  const state = current.state;
  const inspect = opened.inspect === true;
  const keyPrefix = `sandbox:${current.generation}:${current.scope??"saved"}:${current.reference}:${current.sources?.[""]?.handle??""}`;
  const members = sandboxMembers(current);
  const label=current.kind==="transient"?"Transient analysis":current.kind==="saved"?"Saved snapshot":"Sandbox";
  const persistence=current.kind==="transient"?"temporary RAM and disk · auto-Keep off":current.kind==="saved"?(state==="unconfirmed"?"Keep unconfirmed":"retained snapshot"):"results in memory";
  const source=current.sources?.[""];
  const withdrawn = trouble?.code === "ANALYSIS_ACCESS_WITHDRAWN";
  const failure = resultFailure(trouble?.code);
  return <section role="dialog" aria-label={`${label} ${opened.name}`} aria-modal="true" className="sandbox-panel" onKeyDown={event => {
    if (event.key === "Escape") { event.stopPropagation(); onClose(); }
  }}>
    <header><strong title={opened.reference ?? opened.name}>{opened.name}</strong><span>{label} · {inspect ? "inspect" : "values"} · {persistence}</span>
      <button type="button" autoFocus onClick={onClose}>close</button></header>
    <div className="sandbox-panel-body">
      {source?.evidence && <p className="cell-quiet">{source.evidence==="incomplete"?"Incomplete captured evidence; saving it did not complete the producing run.":"Captured stopped observation; reading it does not restart the source."}</p>}
      {trouble && <p role="alert">{trouble.message} {withdrawn ? "Access withdrawn." : "Previous observation is shown."}</p>}
      {failure && <p className="cell-quiet">{failure.next}</p>}
      {withdrawn ? null : members ? <SandboxMembers members={members} inspect={inspect} engine={engine} keyPrefix={keyPrefix} provenance={current.value.provenance} generation={current.generation} readScope={current.scope??undefined} />
        : <div className="sandbox-member-view"><ValueView engine={engine} value={current.value} cacheKey={keyPrefix} readScope={current.scope??undefined} stored={source?.handle?{handle:source.handle,generation:current.generation,scope:source.scope??undefined,select:source.select}:undefined} /></div>}
      {current.kind!=="saved" && <p className="cell-quiet">Providers use their real configured targets. Isolation applies to results, not machine or service effects.</p>}
      {current.kind==="transient" && <p className="cell-quiet"><code>{`:keep $${opened.name}.member > saved_result`}</code> saves one result and its required data dependencies. <code>{`:keep $${opened.name} > saved_analysis`}</code> saves the captured members and their states. Unsaved values do not return after reopening.</p>}
      <p className="cell-quiet">{lifecycleNotice(state, opened.name,current.kind)}</p>
    </div>
  </section>;
}
