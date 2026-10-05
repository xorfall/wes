import { useEffect, useState } from "react";
import type { Engine, SandboxObservation } from "../engine";
import { isNumeric, numericText } from "../exact-json";
import type { StoredValue } from "../protocol";
import { ValueView } from "../views/Result";

/** The fields of a backend-generated full observation and of each of its member rows. */
const OBSERVATION_FIELDS = ["kind", "members", "persistence", "state"];
const MEMBER_FIELDS = new Set(["name", "type", "state", "value", "error"]);
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
  if (!isRecord(data) || Object.keys(data).sort().join() !== OBSERVATION_FIELDS.join()) return undefined;
  if (data.kind !== "Sandbox" || data.persistence !== "definition only" || typeof data.state !== "string" || !Array.isArray(data.members)) return undefined;
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
    members.push({ name: row.name, state: row.state, type: row.type,
      ...("value" in row ? { value: { data: row.value } } : {}), ...("error" in row ? { error: row.error } : {}) });
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

function MemberValue({ member, inspect, engine, cacheKey, provenance }: {
  member: SandboxMember; inspect: boolean; engine: Engine; cacheKey: string; provenance: StoredValue["provenance"];
}) {
  const error = member.error === undefined ? undefined : errorText(member.error);
  const errorView = <ValueView engine={engine} cacheKey={`${cacheKey}:error`} value={{ type: UNKNOWN_TYPE, provenance, data: member.error }} />;
  return <>
    {member.value === undefined ? <span className="sandbox-member-absent">{absence(member, inspect)}</span>
      : scalarText(member.value.data) !== undefined ? <code className="sandbox-member-scalar">{scalarText(member.value.data)}</code>
        : <div className="sandbox-member-view"><ValueView engine={engine} cacheKey={cacheKey} value={{ type: UNKNOWN_TYPE, provenance, data: member.value.data }} /></div>}
    {member.error !== undefined && (error !== undefined ? <>
      <p className="sandbox-member-error" role="note">{error}</p>
      {/* The summary names the error; issues, locations and anything else it carried stay one disclosure away. */}
      {hasErrorDetails(member.error) && <details className="sandbox-member-error-details"><summary>error details</summary><div className="sandbox-member-view">{errorView}</div></details>}
    </> : <div className="sandbox-member-error sandbox-member-view">{errorView}</div>)}
  </>;
}

/** Named members, readable at once: a table where there is room, one stacked card per member where not. */
function SandboxMembers({ members, inspect, engine, keyPrefix, provenance }: {
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
        <td data-label="Value"><MemberValue member={member} inspect={inspect} engine={engine} cacheKey={`${keyPrefix}:${member.name}`} provenance={provenance} /></td>
      </tr>)}</tbody>
    </table>
  </div>;
}

/** What closing the view means for the sandbox, from the observation's own state; only an active one has work to cancel. */
function lifecycleNotice(state: SandboxObservation["state"], name: string): string {
  if (state === "stopped") return `Sandbox stopped. Use :refresh $${name} to run it again.`;
  if (state === "not run") return `Only the definition was restored. Use :refresh $${name} to run it.`;
  if (state === REMOVED) return `Sandbox removed. Nothing here is running or can be cancelled; define $${name} again to use it.`;
  return `Closing this view leaves the sandbox active. Use :cancel $${name} to stop it.`;
}

/** A current observation, never scrollback, persisted output or a source submission loop. */
export function SandboxPanel({ engine, opened, onClose }: {
  engine: Engine; opened: SandboxObservation; onClose: () => void;
}) {
  const [current, setCurrent] = useState(opened);
  const [trouble, setTrouble] = useState<string>();
  useEffect(() => {
    setCurrent(opened); setTrouble(undefined);
    if (!opened.reference) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const read = async () => {
      try {
        if (typeof document !== "undefined" && document.hidden) return;
        const next = await engine.readSandbox(opened.reference!, opened.inspect === true, opened.generation, controller.signal);
        if (!controller.signal.aborted) { setCurrent(next); setTrouble(undefined); }
      } catch (error) {
        if (!controller.signal.aborted) setTrouble(error instanceof Error ? error.message : "Sandbox observation unavailable.");
      } finally {
        if (!controller.signal.aborted) timer = setTimeout(read, 1000);
      }
    };
    timer = setTimeout(read, 1000);
    return () => { controller.abort(); clearTimeout(timer); };
  }, [engine, opened]);
  const state = current.state;
  const inspect = opened.inspect === true;
  const keyPrefix = `sandbox:${opened.generation}:${opened.reference}`;
  const members = sandboxMembers(current);
  return <section role="dialog" aria-label={`Sandbox ${opened.name}`} aria-modal="true" className="sandbox-panel" onKeyDown={event => {
    if (event.key === "Escape") { event.stopPropagation(); onClose(); }
  }}>
    <header><strong title={opened.reference ?? opened.name}>{opened.name}</strong><span>Sandbox · {inspect ? "inspect" : "current values"} · results in memory</span>
      <button type="button" autoFocus onClick={onClose}>close</button></header>
    <div className="sandbox-panel-body">
      {trouble && <p role="alert">{trouble} Previous observation is shown.</p>}
      {members ? <SandboxMembers members={members} inspect={inspect} engine={engine} keyPrefix={keyPrefix} provenance={current.value.provenance} />
        : <div className="sandbox-member-view"><ValueView engine={engine} value={current.value} cacheKey={keyPrefix} /></div>}
      <p className="cell-quiet">Providers use their real configured targets. Isolation applies to results, not machine or service effects.</p>
      <p className="cell-quiet">{lifecycleNotice(state, opened.name)}</p>
    </div>
  </section>;
}
