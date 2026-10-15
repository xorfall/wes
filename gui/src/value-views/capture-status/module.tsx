import { useId, useState, type KeyboardEvent, type ReactElement } from "react";
import { isExactNumber } from "../../exact-json";
import type { TypeShape } from "../../protocol";
import { ellipsizeEnd } from "../../presentation/columns";
import { pageSize, type Mode } from "../../presentation/types";
import { ViewInputError, type ValueViewModule, type ViewComponentProps } from "../contract";
import "./view.css";

/**
 * Reported status of one external capture. These are prefix facts the provider reported, never proof
 * of a live owner, of delivery or of traffic coverage; this view has no command, fetch or control.
 */
const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const field = (type: TypeShape | undefined, name: string): TypeShape | undefined => type?.kind === "record" ? type.fields?.find(it => it.name === name)?.type : undefined;
const is = (name: string) => (type: TypeShape | undefined) => type?.kind === "primitive" && type.name === name;
const record = (checks: Record<string, (type: TypeShape | undefined) => boolean>) => (type: TypeShape | undefined) =>
  type?.kind === "record" && Object.entries(checks).every(([name, check]) => check(field(type, name)));

const profileShape = record({ name: is("TEXT"), backend: is("TEXT"), interface: is("TEXT"), filter: is("TEXT"), durationMs: is("INT"),
  maxBytes: is("INT"), packets: is("INT"), snapLength: is("INT"), promiscuous: is("BOOL") });
const statusShape = record({ profile: profileShape, phase: is("TEXT"), rawBytes: is("TEXT"), packetCount: is("TEXT"), truncatedPackets: is("TEXT"),
  drops: type => type?.kind === "option" && is("TEXT")(type.element), containerComplete: is("BOOL"), indexComplete: is("BOOL"), coverageKnown: is("BOOL") });

/** A rolling window longer than this is not a status window this view validates. */
export const MAX_WINDOW = 4096;
/** A complete index has a row for every packet, so it covers at most this many. */
export const INDEX_ROWS = 512;
const MAX_U64 = 18446744073709551615n;
const phases = ["opening", "ready", "stopping", "ended", "incomplete"] as const;
type Phase = typeof phases[number];
const terminal = (phase: Phase) => phase === "ended" || phase === "incomplete";

const u64 = (value: unknown): value is string => typeof value === "string" && value.length <= 20 && /^(0|[1-9]\d*)$/.test(value) && BigInt(value) <= MAX_U64;
function within(value: unknown, min: bigint, max: bigint): string | undefined {
  const text = typeof value === "number" ? (Number.isSafeInteger(value) ? String(value) : undefined) : isExactNumber(value) ? value.text : undefined;
  if (text === undefined || text.length > 20 || !/^(0|-?[1-9]\d*)$/.test(text)) return;
  return BigInt(text) >= min && BigInt(text) <= max ? text : undefined;
}
const chars = (text: string) => [...text].length;
const controls = /[\u0000-\u001f\u007f]/;
function text(value: unknown, max: number, allowed = controls): value is string {
  return typeof value === "string" && value.length > 0 && chars(value) <= max && !allowed.test(value);
}
/** Thousands separators applied to decimal text; never through Number, so u64 stays exact. */
export const grouped = (digits: string) => digits.replace(/\B(?=(\d{3})+(?!\d))/g, ",");

export interface CaptureProfile {
  readonly name: string; readonly backend: string; readonly interface: string; readonly filter: string;
  readonly durationMs: string; readonly maxBytes: string; readonly packets: string; readonly snapLength: string; readonly promiscuous: boolean;
}
export interface CaptureStatus {
  readonly profile: CaptureProfile; readonly phase: Phase; readonly rawBytes: string; readonly packetCount: string;
  readonly truncated: string; readonly drops?: string; readonly containerComplete: boolean; readonly indexComplete: boolean;
}

function readProfile(value: unknown): CaptureProfile | undefined {
  if (!object(value)) return;
  const { name, backend, interface: iface, filter, promiscuous } = value;
  const durationMs = within(value.durationMs, 1n, 120_000n), maxBytes = within(value.maxBytes, 24n, 536_870_912n);
  const packets = within(value.packets, 1n, 1_000_000n), snapLength = within(value.snapLength, 1n, 65_535n);
  // An empty filter is the explicit absence of one; every other text keeps its limits.
  if (!text(name, 64) || !(filter === "" || text(filter, 4096, /[\u0000\u000b\u000c\u000e-\u001f\u007f]/)) || typeof promiscuous !== "boolean") return;
  // An explicit local interface name: never option-shaped, a path, a remote or a pipe specification.
  if (!text(iface, 1024) || /^-|[\\/:@]/.test(iface)) return;
  // An explicit absolute local Unix executable path; it is shown, never probed or run.
  if (!text(backend, 4096) || backend.length < 2 || !backend.startsWith("/") || backend.endsWith("/")) return;
  if (durationMs === undefined || maxBytes === undefined || packets === undefined || snapLength === undefined) return;
  return { name, backend, interface: iface, filter, durationMs, maxBytes, packets, snapLength, promiscuous };
}

/** One status, or undefined when any fact is malformed or contradicts another. */
export function readStatus(value: unknown): CaptureStatus | undefined {
  if (!object(value)) return;
  const profile = readProfile(value.profile);
  const { phase, rawBytes, packetCount, truncatedPackets, drops, containerComplete, indexComplete, coverageKnown } = value;
  if (!profile || typeof phase !== "string" || !(phases as readonly string[]).includes(phase)) return;
  if (!u64(rawBytes) || !u64(packetCount) || !u64(truncatedPackets)) return;
  if (typeof containerComplete !== "boolean" || typeof indexComplete !== "boolean" || coverageKnown !== false) return;
  // Only the normalized Wes option; unknown drops are `none`, never zero.
  const dropped = object(drops) && drops.kind === "none" ? { known: false } as const
    : object(drops) && drops.kind === "some" && u64(drops.value) ? { known: true, value: drops.value as string } as const : undefined;
  if (!dropped) return;
  const count = BigInt(packetCount), raw = BigInt(rawBytes);
  // Truncated packets are among the counted ones; the profile's packet limit bounds the count;
  // the acknowledged raw prefix never exceeds the frozen size limit, and a counted packet lies in it,
  // behind at least the smallest container and record headers.
  if (BigInt(truncatedPackets) > count || count > BigInt(profile.packets) || raw > BigInt(profile.maxBytes)) return;
  if (count > 0n && raw < 24n + 16n * count) return;
  // A complete container ends on a record boundary after at least its 24-byte header; a complete index indexes
  // every packet, so it needs that boundary and no more than 512 packets; a reported end needs a complete container.
  if (containerComplete && raw < 24n) return;
  if (indexComplete && (!containerComplete || count > BigInt(INDEX_ROWS))) return;
  if (phase === "ended" && !containerComplete) return;
  return { profile, phase: phase as Phase, rawBytes, packetCount, truncated: truncatedPackets, ...(dropped.known ? { drops: dropped.value } : {}), containerComplete, indexComplete };
}

const sameProfile = (a: CaptureProfile, b: CaptureProfile) => (Object.keys(a) as (keyof CaptureProfile)[]).every(key => a[key] === b[key]);
/** A later status of the same running capture never reports fewer bytes, packets, truncations or drops. */
function follows(previous: CaptureStatus, next: CaptureStatus) {
  if (terminal(previous.phase) || !sameProfile(previous.profile, next.profile)) return true;
  const grows = (a: string, b: string) => BigInt(a) <= BigInt(b);
  return grows(previous.rawBytes, next.rawBytes) && grows(previous.packetCount, next.packetCount) && grows(previous.truncated, next.truncated)
    && (previous.drops === undefined || next.drops === undefined || grows(previous.drops, next.drops));
}
/** Every status of a window, oldest first, validated whole; undefined when any is malformed or contradictory. */
export function readWindow(values: readonly unknown[]): CaptureStatus[] | undefined {
  if (values.length > MAX_WINDOW) return;
  const statuses: CaptureStatus[] = [];
  for (const value of values) {
    const status = readStatus(value);
    if (!status || (statuses.length > 0 && !follows(statuses[statuses.length - 1]!, status))) return;
    statuses.push(status);
  }
  return statuses;
}

export function matchesCaptureStatus(type: TypeShape, data: unknown): boolean {
  if (statusShape(type)) return readStatus(data) !== undefined;
  return type.kind === "list" && statusShape(type.element) && Array.isArray(data) && readWindow(data) !== undefined;
}

type Tab = "status" | "profile" | "history";
type Fact = "counts" | "drops" | "container" | "scope" | "note";
interface HistoryRow { readonly position: number; readonly status: CaptureStatus }
interface StatusModel {
  readonly source: object;
  readonly mode: Mode;
  readonly columns: number;
  readonly narrow: boolean;
  readonly latest?: CaptureStatus;
  readonly total: number;
  readonly window: boolean;
  /** Facts drawn beneath the header when there is no room for tabs (always in preview). */
  readonly facts: readonly Fact[];
  /** Lines each tab panel may draw; zero means no tabs. */
  readonly panel: number;
  /** Newest first, only as many as the history panel can draw. */
  readonly history: readonly HistoryRow[];
}

/** The disclaimer outranks the scope when height runs out. */
const STATUS_FACTS: readonly Fact[] = ["counts", "drops", "container", "note", "scope"];
const PROFILE_ROWS = 10;
/** The profile row stating the join requirement; it spans the panel so no label column clips it. */
const SNAPSHOT = "Snapshot";

export const captureStatusViewModule: ValueViewModule = {
  id: "capture-status",
  matches: matchesCaptureStatus,
  present({ type, data, context }, host) {
    const window = type.kind === "list";
    const statuses = window ? (Array.isArray(data) ? readWindow(data) : undefined) : (s => s ? [s] : undefined)(readStatus(data));
    if (!statuses || !object(data) && !Array.isArray(data)) throw new ViewInputError("Capture status fields are malformed");
    const latest = statuses[statuses.length - 1];
    const remaining = host.remaining();
    let left = Math.max(0, remaining - 1), facts: readonly Fact[] = [], panel = 0, history: HistoryRow[] = [];
    if (latest) {
      const tabbed = context.mode !== "preview" && left >= 4;
      if (tabbed) {
        left -= 1;
        const historyNeed = statuses.length > 1 ? 2 + Math.min(statuses.length, pageSize()) : 0;
        panel = Math.min(left, Math.max(STATUS_FACTS.length, PROFILE_ROWS, historyNeed));
        const rows = Math.max(0, Math.min(statuses.length, panel - 2, pageSize()));
        history = statuses.length > 1 ? statuses.slice(-rows).reverse().map((status, at) => ({ position: statuses.length - at, status })) : [];
      } else facts = (context.mode === "preview" ? STATUS_FACTS.slice(0, 3) : STATUS_FACTS).slice(0, left);
    } else if (context.mode !== "preview" && left >= 1) facts = ["note"];
    const ownLines = Math.min(remaining, 1 + facts.length + (panel > 0 ? 1 + panel : 0));
    host.spend(ownLines);
    const model: StatusModel = { source: data as object, mode: context.mode, columns: context.columns, narrow: context.columns < 72,
      ...(latest ? { latest } : {}), total: statuses.length, window, facts, panel, history };
    return { model, children: [], ownLines, summary: [{ text: latest ? `${grouped(latest.packetCount)} packets · ${latest.phase}` : "no status reported", tone: latest ? "ink" : "faint" }] };
  },
  Component: CaptureStatusView,
};

const plural = (count: string, word: string) => `${grouped(count)} ${word}${count === "1" ? "" : "s"}`;
const phaseTone = (phase: Phase) => phase === "incomplete" ? "capture-status-warn" : phase === "ready" ? "capture-status-ok" : "capture-status-dim";
function factText(fact: Fact, status: CaptureStatus, window: boolean): string {
  const { profile } = status;
  switch (fact) {
    case "counts": return `${plural(status.packetCount, "packet")} · ${grouped(status.rawBytes)} B · ${status.truncated === "0" ? "none truncated"
      : `${grouped(status.truncated)} truncated · snap length ${grouped(profile.snapLength)} requested`}`;
    case "drops": return `${status.drops === undefined ? "Drops unknown" : `${plural(status.drops, "drop")} reported`} · traffic coverage not established`;
    case "container": return `Container ${status.containerComplete ? "complete" : "not complete"} · ${status.indexComplete ? "packet index complete · every packet has a row"
      : `packet index not complete · complete only when every packet has a row, at most ${INDEX_ROWS}`}`;
    case "scope": return `Interface ${profile.interface} · ${profile.filter === "" ? "no filter" : `filter ${profile.filter}`}`;
    case "note": return window ? "Reported status · selected or restored values are not a live capture" : "Reported status · not proof of a live capture or of stored bytes";
  }
}
function profileRows(profile: CaptureProfile): readonly (readonly [string, string])[] {
  return [["Name", profile.name], ["Interface", profile.interface], ["Filter", profile.filter === "" ? "none · no filter" : profile.filter],
    ["Backend", `${profile.backend} · recorded path, not run by this view`],
    ["Duration limit", `${grouped(profile.durationMs)} ms`], ["Size limit", `${grouped(profile.maxBytes)} B`], ["Packet limit", grouped(profile.packets)],
    ["Snap length", `${grouped(profile.snapLength)} B requested`], ["Promiscuous", profile.promiscuous ? "requested" : "not requested"],
    // A stop receipt or a reported end never proves the join, so the phase never stands in for it; the requirement leads.
    [SNAPSHOT, "Physical join required · :read bytes of the original recording · frozen snapshot · an ended phase is not proof"]];
}

/** Rows beyond the panel height are counted, never silently dropped. */
function bounded<T>(rows: readonly T[], lines: number): { shown: readonly T[]; hidden: number } {
  if (rows.length <= lines) return { shown: rows, hidden: 0 };
  const shown = rows.slice(0, Math.max(0, lines - 1));
  return { shown, hidden: rows.length - shown.length };
}

interface ViewState { readonly source: object; readonly tab: Tab; readonly selected?: number }

function CaptureStatusView({ model: input }: ViewComponentProps) {
  const model = input as StatusModel, { latest } = model;
  const id = useId();
  const [stored, setState] = useState<ViewState>({ source: model.source, tab: "status" });
  // A replaced or withdrawn input keeps only the chosen tab; a selected history row never carries over.
  const state = stored.source === model.source ? stored : { source: model.source, tab: stored.tab };
  const fit = (value: string, reserve = 0) => ellipsizeEnd(value, Math.max(8, model.columns - reserve));
  if (!latest) return <section className={`capture-status-view capture-status-${model.mode}`} aria-label="Capture status">
    <header className="capture-status-header"><strong>Waiting for a reported capture status</strong></header>
    {model.facts.includes("note") && <p className="capture-status-dim">{fit("Empty window · no profile or readiness is known yet")}</p>}
  </section>;

  const tabs: { id: Tab; label: string }[] = [{ id: "status", label: "Status" }, { id: "profile", label: "Profile" },
    ...(model.history.length > 0 ? [{ id: "history" as const, label: `History · ${grouped(String(model.total))}` }] : [])];
  const tab = tabs.some(it => it.id === state.tab) ? state.tab : "status";
  const choose = (next: Tab) => setState({ source: model.source, tab: next, ...(state.selected === undefined ? {} : { selected: state.selected }) });
  const keys = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault(); event.stopPropagation();
    const current = tabs.findIndex(it => it.id === tab);
    const next = event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : (current + (event.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length;
    choose(tabs[next]!.id);
    event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next]?.focus();
  };
  const header = <header className="capture-status-header">
    <strong>{latest.profile.name}</strong>
    <span className={phaseTone(latest.phase)}>{`phase ${latest.phase}`}</span>
    <span className="capture-status-dim">{model.window ? `latest of ${grouped(String(model.total))} reported status${model.total === 1 ? "" : "es"}` : "reported status"}</span>
  </header>;
  const tone = (name: Fact) => name === "drops" ? (latest.drops === undefined ? "capture-status-unknown" : undefined) : name === "note" || name === "scope" ? "capture-status-dim" : undefined;
  const fact = (name: Fact) => <p key={name} className={tone(name)}>{fit(factText(name, latest, model.window))}</p>;
  if (model.panel === 0) return <section className={`capture-status-view capture-status-${model.mode}`} aria-label="Capture status">{header}{model.facts.map(fact)}</section>;

  const selected = state.selected === undefined ? undefined : model.history.find(row => row.position === state.selected);
  const panels: Record<Tab, () => ReactElement> = {
    status: () => { const { shown, hidden } = bounded(STATUS_FACTS, model.panel), note = factText("note", latest, model.window); return <>
      {shown.map(fact)}
      {/* The hidden count leads so narrow widths keep it; a hidden disclaimer stays short here and whole in the title. */}
      {hidden > 0 && <p className="capture-status-dim" title={note}>{fit(`${plural(String(hidden), "fact")} hidden · ${shown.includes("note") ? "need more height" : "status snapshot"}`)}</p>}</>; },
    profile: () => { const { shown, hidden } = bounded(profileRows(latest.profile), model.panel); return <>
      <table className="capture-status-profile"><tbody>{shown.map(([name, value]) =>
        <tr key={name}>{name === SNAPSHOT ? <td colSpan={2} title={value}>{fit(value)}</td>
          : <><th scope="row">{name}</th><td title={value}>{fit(value, 18)}</td></>}</tr>)}</tbody></table>
      {hidden > 0 && <p className="capture-status-dim">{fit(`${plural(String(hidden), "more row")} need more height`)}</p>}</>; },
    history: () => <>
      <table className="capture-status-history"><thead><tr><th scope="col">#</th><th scope="col">Phase</th><th scope="col">Packets</th>
        {!model.narrow && <><th scope="col">Bytes</th><th scope="col">Truncated</th></>}<th scope="col">Drops</th></tr></thead>
        <tbody>{model.history.map(({ position, status }) => <tr key={position} aria-selected={position === state.selected} className={position === state.selected ? "capture-status-selected" : undefined}>
          <td><button type="button" className="cell-action" aria-pressed={position === state.selected} onClick={() => setState({ source: model.source, tab, selected: position })}>{String(position)}</button></td>
          <td className={phaseTone(status.phase)}>{status.phase}</td><td>{status.packetCount}</td>
          {!model.narrow && <><td>{status.rawBytes}</td><td>{status.truncated}</td></>}
          <td className={status.drops === undefined ? "capture-status-unknown" : undefined}>{status.drops ?? "unknown"}</td>
        </tr>)}</tbody></table>
      <p className="capture-status-dim" role="status">{fit(selected
        ? `Status ${selected.position} · container ${selected.status.containerComplete ? "complete" : "not complete"} · packet index ${selected.status.indexComplete ? "complete" : "not complete"} · ${sameProfile(selected.status.profile, latest.profile) ? "same profile" : `profile ${selected.status.profile.name}`}`
        : `Newest ${grouped(String(model.history.length))} of ${grouped(String(model.total))} reported statuses · select one for its container facts`)}</p>
    </>,
  };
  return <section className={`capture-status-view capture-status-${model.mode}${model.narrow ? " capture-status-narrow" : ""}`} aria-label="Capture status">
    {header}
    <div className="capture-status-tabs" role="tablist" aria-label="Capture status sections" onKeyDown={keys}>
      {tabs.map(it => <button key={it.id} id={`${id}-${it.id}-tab`} type="button" role="tab" className="cell-action" tabIndex={tab === it.id ? 0 : -1}
        aria-selected={tab === it.id} aria-controls={`${id}-${it.id}`} onClick={() => choose(it.id)}>{it.label}</button>)}
    </div>
    <div id={`${id}-${tab}`} role="tabpanel" aria-label={tabs.find(it => it.id === tab)!.label} aria-labelledby={`${id}-${tab}-tab`} className="capture-status-panel">{panels[tab]()}</div>
  </section>;
}
