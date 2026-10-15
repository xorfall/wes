import { useId, useMemo, useState, type KeyboardEvent, type ReactElement } from "react";
import { isExactNumber } from "../../exact-json";
import type { TypeShape } from "../../protocol";
import { ellipsizeEnd } from "../../presentation/columns";
import { pageSize, type Mode } from "../../presentation/types";
import { ViewInputError, type ValueViewModule, type ViewComponentProps } from "../contract";
import { grouped, matchesCaptureIndex, readCaptureIndex, type CaptureIndex, type CaptureInterface } from "../capture-index/module";
import "./view.css";

/**
 * A finite offline header analysis of one capture file: decoder identity, the bounded options it ran
 * with, the container index, and fixed-header rows for the leading packets only. Field coordinates are
 * facts about the original file; this view never reads, fetches or exports those bytes.
 */
const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const field = (type: TypeShape | undefined, name: string): TypeShape | undefined => type?.kind === "record" ? type.fields?.find(it => it.name === name)?.type : undefined;
const is = (name: string) => (type: TypeShape | undefined) => type?.kind === "primitive" && type.name === name;
const record = (checks: Record<string, (type: TypeShape | undefined) => boolean>) => (type: TypeShape | undefined) =>
  type?.kind === "record" && Object.entries(checks).every(([name, check]) => check(field(type, name)));
const listOf = (check: (type: TypeShape | undefined) => boolean) => (type: TypeShape | undefined) => type?.kind === "list" && check(type.element);
const optionOf = (check: (type: TypeShape | undefined) => boolean) => (type: TypeShape | undefined) => type?.kind === "option" && check(type.element);

const decoderShape = record({ name: is("TEXT"), version: is("TEXT"), runtime: is("TEXT"), codeSha256: is("TEXT"), containerSha256: is("TEXT"), schemaSha256: is("TEXT") });
const optionsShape = record({ packetLimit: is("INT"), headerBytes: is("INT") });
const fieldShape = record({ layer: is("TEXT"), key: is("TEXT"), label: is("TEXT"), value: is("TEXT"), offset: is("TEXT"), length: is("INT") });
const packetShape = record({ number: is("INT"), interface: is("INT"), dataOffset: is("TEXT"), capturedLength: is("INT"), originalLength: is("INT"), protocol: is("TEXT"),
  source: optionOf(is("TEXT")), destination: optionOf(is("TEXT")), sourcePort: optionOf(is("INT")), destinationPort: optionOf(is("INT")),
  status: is("TEXT"), inspectedBytes: is("INT"), fields: listOf(fieldShape), notes: listOf(is("TEXT")) });
const analysisShape = record({ decoder: decoderShape, options: optionsShape, packets: listOf(packetShape), sampleComplete: is("BOOL"), limitations: listOf(is("TEXT")) });

/** Declared bounds of the analysis types; anything larger is not this analysis. */
export const MAX_SAMPLE_ROWS = 512;
export const MAX_FIELDS = 64;
export const MAX_NOTES = 8;
export const MAX_LIMITATIONS = 16;
export const MAX_HEADER_BYTES = 65_536;
/** Search text is bounded and filters only the supplied sample rows, in memory. */
export const MAX_QUERY = 128;
const MAX_U64 = 18446744073709551615n;
const statuses = ["decoded", "unsupported", "truncated", "fragmented", "malformed"] as const;
export type DecodeStatus = typeof statuses[number];

const u64 = (value: unknown): value is string => typeof value === "string" && value.length <= 20 && /^(0|[1-9]\d*)$/.test(value) && BigInt(value) <= MAX_U64;
/** A canonical integer within bounds, as exact decimal text; a Number is accepted only when it is safe. */
function within(value: unknown, min: bigint, max: bigint): string | undefined {
  const text = typeof value === "number" ? (Number.isSafeInteger(value) ? String(value) : undefined) : isExactNumber(value) ? value.text : undefined;
  if (text === undefined || text.length > 20 || !/^(0|-?[1-9]\d*)$/.test(text)) return;
  return BigInt(text) >= min && BigInt(text) <= max ? text : undefined;
}
/** Controls and bidirectional overrides could disguise a shown value; they are refused, not stripped. */
const unsafe = /[\u0000-\u001f\u007f-\u009f\u200e\u200f\u202a-\u202e\u2066-\u2069]/;
const text = (value: unknown, max: number): value is string => typeof value === "string" && value.length > 0 && value.length <= max * 2 && [...value].length <= max && !unsafe.test(value);
const digestForm = /^sha256:[0-9a-f]{64}$/;
/** Only the native option wire form; JSON null or a bare value is not an option. */
function option<T>(value: unknown, read: (inner: unknown) => T | undefined): { readonly value?: T } | undefined {
  if (!object(value)) return;
  if (value.kind === "none") return {};
  if (value.kind !== "some") return;
  const inner = read(value.value);
  return inner === undefined ? undefined : { value: inner };
}

export interface DecoderIdentity { readonly name: string; readonly version: string; readonly runtime: string; readonly code: string; readonly container: string; readonly schema: string }
export interface DecodedField {
  readonly layer: string; readonly key: string; readonly label: string; readonly value: string;
  /** Original file coordinates: first and last byte, and the first byte relative to the packet data. */
  readonly offset: string; readonly last: string; readonly relative: string; readonly length: string;
}
export interface DecodedPacket {
  readonly number: string; readonly interface: string; readonly dataOffset: string; readonly captured: string; readonly original: string;
  readonly protocol: string; readonly source?: string; readonly destination?: string; readonly sourcePort?: string; readonly destinationPort?: string;
  readonly status: DecodeStatus; readonly inspected: string; readonly fields: readonly DecodedField[]; readonly notes: readonly string[];
}
export interface CaptureAnalysis {
  readonly decoder: DecoderIdentity; readonly packetLimit: string; readonly headerBytes: string; readonly index: CaptureIndex;
  readonly packets: readonly DecodedPacket[]; readonly complete: boolean; readonly limitations: readonly string[];
}

function readDecoder(value: unknown): DecoderIdentity | undefined {
  if (!object(value)) return;
  const { name, version, runtime, codeSha256, containerSha256, schemaSha256 } = value;
  if (!text(name, 64) || !text(version, 32) || !text(runtime, 64)) return;
  if (typeof codeSha256 !== "string" || typeof containerSha256 !== "string" || typeof schemaSha256 !== "string") return;
  if (![codeSha256, containerSha256, schemaSha256].every(it => digestForm.test(it))) return;
  return { name, version, runtime, code: codeSha256, container: containerSha256, schema: schemaSha256 };
}

function readField(value: unknown, start: bigint, end: bigint): DecodedField | undefined {
  if (!object(value)) return;
  const { layer, key, label, offset } = value, shown = value.value, length = within(value.length, 1n, BigInt(MAX_HEADER_BYTES));
  if (!text(layer, 32) || !text(key, 64) || !text(label, 96) || !text(shown, 256) || !u64(offset) || length === undefined) return;
  // A field lies wholly inside the inspected prefix of its own packet's original bytes.
  const first = BigInt(offset), after = first + BigInt(length);
  if (first < start || after > end) return;
  return { layer, key, label, value: shown, offset, last: (after - 1n).toString(), relative: (first - start).toString(), length };
}

function readPacket(value: unknown, at: number, index: CaptureIndex, headerBytes: string): DecodedPacket | undefined {
  if (!object(value)) return;
  const number = within(value.number, 1n, BigInt(MAX_SAMPLE_ROWS)), iface = within(value.interface, 0n, MAX_U64);
  const captured = within(value.capturedLength, 0n, MAX_U64), original = within(value.originalLength, 0n, MAX_U64);
  const inspected = within(value.inspectedBytes, 0n, BigInt(MAX_HEADER_BYTES));
  const { dataOffset, protocol, status, fields, notes } = value;
  if (number === undefined || iface === undefined || captured === undefined || original === undefined || inspected === undefined || !u64(dataOffset)) return;
  // The sample is the leading packets in container order, and each row is the very packet the index describes.
  const row = index.packets[at];
  if (number !== String(at + 1) || !row || row.number !== number || row.interface !== iface || row.dataOffset !== dataOffset
    || row.captured !== captured || row.original !== original) return;
  if (!text(protocol, 32) || typeof status !== "string" || !(statuses as readonly string[]).includes(status)) return;
  const source = option(value.source, it => text(it, 64) ? it : undefined), destination = option(value.destination, it => text(it, 64) ? it : undefined);
  const sourcePort = option(value.sourcePort, it => within(it, 0n, 65_535n)), destinationPort = option(value.destinationPort, it => within(it, 0n, 65_535n));
  if (!source || !destination || !sourcePort || !destinationPort) return;
  // Only the header prefix of what was captured is ever inspected.
  if (BigInt(inspected) > BigInt(captured) || BigInt(inspected) > BigInt(headerBytes)) return;
  // A packet stored shorter than its original length is never reported as cleanly decoded.
  if (status === "decoded" && captured !== original) return;
  if (!Array.isArray(fields) || fields.length > MAX_FIELDS || !Array.isArray(notes) || notes.length > MAX_NOTES) return;
  if (!notes.every(note => text(note, 256))) return;
  const start = BigInt(dataOffset), end = start + BigInt(inspected), shown: DecodedField[] = [];
  for (const it of fields) {
    const decoded = readField(it, start, end);
    if (!decoded) return;
    shown.push(decoded);
  }
  return { number, interface: iface, dataOffset, captured, original, protocol, status: status as DecodeStatus, inspected, fields: shown, notes: notes as string[],
    ...(source.value === undefined ? {} : { source: source.value }), ...(destination.value === undefined ? {} : { destination: destination.value }),
    ...(sourcePort.value === undefined ? {} : { sourcePort: sourcePort.value }), ...(destinationPort.value === undefined ? {} : { destinationPort: destinationPort.value }) };
}

const parsed = new WeakMap<object, CaptureAnalysis | null>();
/** The exact facts of a well-formed, self-consistent analysis, or undefined. Everything is checked before anything is shown. */
export function readCaptureAnalysis(data: unknown): CaptureAnalysis | undefined {
  if (!object(data)) return;
  if (parsed.has(data)) return parsed.get(data) ?? undefined;
  const read = (): CaptureAnalysis | undefined => {
    const decoder = readDecoder(data.decoder), index = readCaptureIndex(data.index);
    const options = object(data.options) ? data.options : {};
    const packetLimit = within(options.packetLimit, 1n, BigInt(MAX_SAMPLE_ROWS)), headerBytes = within(options.headerBytes, 64n, BigInt(MAX_HEADER_BYTES));
    const { packets, sampleComplete, limitations } = data;
    if (!decoder || !index || packetLimit === undefined || headerBytes === undefined || typeof sampleComplete !== "boolean") return;
    if (!Array.isArray(packets) || packets.length > MAX_SAMPLE_ROWS || !Array.isArray(limitations) || limitations.length > MAX_LIMITATIONS) return;
    if (!limitations.every(it => text(it, 512))) return;
    // Exactly the first min(limit, count) container packets; complete only when that is every packet.
    const count = BigInt(index.packetCount), limit = BigInt(packetLimit), supplied = BigInt(packets.length);
    if (supplied !== (limit < count ? limit : count) || sampleComplete !== (supplied === count)) return;
    const rows: DecodedPacket[] = [];
    for (let at = 0; at < packets.length; at++) {
      const row = readPacket(packets[at], at, index, headerBytes);
      if (!row) return;
      rows.push(row);
    }
    return { decoder, packetLimit, headerBytes, index, packets: rows, complete: sampleComplete, limitations: limitations as string[] };
  };
  const analysis = read();
  parsed.set(data, analysis ?? null);
  return analysis;
}

export function matchesCaptureAnalysis(type: TypeShape, data: unknown): boolean {
  const indexType = field(type, "index");
  return analysisShape(type) && indexType !== undefined && object(data) && matchesCaptureIndex(indexType, data.index) && readCaptureAnalysis(data) !== undefined;
}

type Line = "header" | "coverage" | "status" | "source" | "caveat";
type Tab = "packet" | "decoder" | "limits";
/** The detail panel holds four packet lines, a field header, rows and a pager; the table keeps a few rows beside it. */
const PANEL_MIN = 8, PANEL_MAX = 16, TABLE_ROWS = 4;
interface AnalysisModel {
  readonly source: object;
  readonly analysis: CaptureAnalysis;
  readonly mode: Mode;
  readonly columns: number;
  readonly narrow: boolean;
  readonly lines: readonly Line[];
  /** Filter, column header and status are drawn only when at least one row fits beside them. */
  readonly table: boolean;
  readonly rows: number;
  /** Lines of the tab panel beneath its tab row; zero means no detail panel. */
  readonly panel: number;
}

const plural = (count: string, word: string) => `${grouped(count)} ${word}${count === "1" ? "" : "s"}`;

export const captureAnalysisViewModule: ValueViewModule = {
  id: "capture-analysis",
  matches: matchesCaptureAnalysis,
  present({ data, context }, host) {
    const analysis = readCaptureAnalysis(data);
    if (!analysis || !object(data)) throw new ViewInputError("Capture analysis fields are malformed");
    const remaining = host.remaining(), sampled = analysis.packets.length;
    const wanted: Line[] = context.mode === "preview" ? ["header", "coverage", "status"] : ["header", "coverage", "status", "source", "caveat"];
    const lines = wanted.slice(0, Math.max(1, Math.min(remaining, wanted.length)));
    let left = remaining - lines.length, table = false, rows = 0, panel = 0;
    if (context.mode !== "preview") {
      // Filter + column header + status + one row is the least table worth drawing.
      if (sampled > 0 && left >= 4) { table = true; left -= 3; }
      const keep = table ? 1 : 0;
      if (left >= 1 + PANEL_MIN + keep) {
        panel = Math.max(PANEL_MIN, Math.min(PANEL_MAX, left - 1 - (table ? Math.min(sampled, TABLE_ROWS) : 0)));
        left -= 1 + panel;
      }
      if (table) rows = Math.max(1, Math.min(sampled, left, pageSize()));
    }
    const ownLines = Math.min(remaining, lines.length + (table ? 3 + rows : 0) + (panel > 0 ? 1 + panel : 0));
    host.spend(ownLines);
    const model: AnalysisModel = { source: data, analysis, mode: context.mode, columns: context.columns, narrow: context.columns < 72, lines, table, rows, panel };
    return { model, children: [], ownLines, summary: [{ text: `${grouped(String(sampled))} of ${plural(analysis.index.packetCount, "packet")} sampled`, tone: "ink" }] };
  },
  Component: CaptureAnalysisView,
};

const meaning: Readonly<Record<DecodeStatus, string>> = {
  decoded: "fixed headers decoded · payload, options and checksums not checked",
  unsupported: "link type or protocol not supported by this decoder",
  truncated: "header bytes missing from the capture or the inspected prefix",
  fragmented: "IPv4 fragment · transport header not decoded without reassembly",
  malformed: "header fields contradict each other",
};
const endpoint = (address: string | undefined, port: string | undefined) =>
  address === undefined ? (port === undefined ? undefined : `port ${port}`) : port === undefined ? address : `${address}:${port}`;
const span = (first: string, last: string) => first === last ? first : `${first}–${last}`;

interface Searchable { readonly core: string; readonly parts: readonly string[] }
const searchable = new WeakMap<DecodedPacket, Searchable>();
/** Row facts plus every decoded field's layer, key, label and value and every note; built only once a query is entered. */
function search(row: DecodedPacket): Searchable {
  let found = searchable.get(row);
  if (found === undefined) {
    found = { core: [`#${row.number}`, `if${row.interface}`, row.protocol, row.status, row.source, row.destination, row.sourcePort, row.destinationPort,
      row.dataOffset, row.captured, row.original].filter(it => it !== undefined).join(" ").toLowerCase(),
      parts: [...row.fields.map(it => `${it.layer} ${it.key} ${it.label} ${it.value}`.toLowerCase()), ...row.notes.map(it => it.toLowerCase())] };
    searchable.set(row, found);
  }
  return found;
}
const hit = (row: DecodedPacket, word: string) => { const it = search(row); return it.core.includes(word) || it.parts.some(part => part.includes(word)); };

function counts(analysis: CaptureAnalysis) {
  const status = new Map<DecodeStatus, number>(), protocol = new Map<string, number>();
  for (const row of analysis.packets) {
    status.set(row.status, (status.get(row.status) ?? 0) + 1);
    protocol.set(row.protocol, (protocol.get(row.protocol) ?? 0) + 1);
  }
  const shownStatus = statuses.filter(it => status.has(it)).map(it => `${grouped(String(status.get(it)))} ${it}`);
  const protocols = [...protocol].slice(0, 6).map(([name, count]) => `${name} ${grouped(String(count))}`);
  return { status: shownStatus.join(" · "), protocols: `${protocols.join(" · ")}${protocol.size > 6 ? ` · ${protocol.size - 6} more protocols` : ""}`,
    issues: analysis.packets.length - (status.get("decoded") ?? 0) };
}

interface ViewState {
  readonly source: object; readonly query: string; readonly page: number; readonly tab?: Tab;
  readonly selected?: string; readonly field?: number; readonly fieldPage: number;
}
const fresh = (source: object): ViewState => ({ source, query: "", page: 0, fieldPage: 0 });

type Item = { readonly kind: "note"; readonly text: string; readonly at: number } | { readonly kind: "field"; readonly field: DecodedField; readonly at: number };

function PacketPanel({ packet, link, lines, chosen, page, narrow, fit, choose, turn }: {
  packet: DecodedPacket | undefined; link: string | undefined; lines: number; chosen: number | undefined; page: number; narrow: boolean;
  fit: (text: string) => string; choose: (at: number) => void; turn: (page: number) => void;
}) {
  if (!packet) return <p className="capture-analysis-dim">Select a sampled packet row to see its decoded header fields.</p>;
  const from = endpoint(packet.source, packet.sourcePort), to = endpoint(packet.destination, packet.destinationPort);
  const route = from === undefined && to === undefined ? "addresses not decoded" : `${from ?? "source not decoded"} → ${to ?? "destination not decoded"}`;
  const start = BigInt(packet.dataOffset), captured = BigInt(packet.captured), inspected = BigInt(packet.inspected);
  const stored = captured === 0n ? "no captured bytes" : `original bytes ${span(packet.dataOffset, (start + captured - 1n).toString())}`;
  const looked = inspected === 0n ? "nothing inspected" : `inspected ${grouped(packet.inspected)} B at ${span(packet.dataOffset, (start + inspected - 1n).toString())}`;
  const items: Item[] = [...packet.notes.map((note, at) => ({ kind: "note" as const, text: note, at })), ...packet.fields.map((it, at) => ({ kind: "field" as const, field: it, at }))];
  // Four packet lines above; the grid spends its header, its rows and a pager only when the items do not fit.
  const grid = lines - 4;
  const perPage = items.length + 1 <= grid ? items.length : Math.max(1, grid - 2);
  const pages = Math.max(1, Math.ceil(items.length / perPage)), current = Math.min(page, pages - 1);
  const shown = items.slice(current * perPage, current * perPage + perPage);
  const selected = chosen === undefined ? undefined : packet.fields[chosen];
  const layerOf = (item: Item) => item.kind === "note" ? "Note" : item.field.layer;
  return <>
    <p><strong>{fit(`Packet ${packet.number} · interface ${packet.interface}${link === undefined ? "" : ` · link type ${link}`} · ${packet.protocol} · ${packet.status} · ${route}`)}</strong></p>
    <p>{fit(`${stored} · captured ${grouped(packet.captured)} of ${grouped(packet.original)} B · ${looked}`)}</p>
    <p className={packet.status === "decoded" ? "capture-analysis-dim" : "capture-analysis-warn"}>{fit(`${meaning[packet.status]} · ${packet.notes.length === 0 ? "no decoder notes" : plural(String(packet.notes.length), "decoder note")}`)}</p>
    <p className="capture-analysis-selection" role="status">{fit(selected
      ? `${selected.layer} ${selected.label} = ${selected.value} · original capture bytes ${span(selected.offset, selected.last)} (${plural(selected.length, "byte")}, packet offset ${selected.relative}) · coordinates only, bytes not read by this view`
      : packet.fields.length === 0 ? "No header fields were decoded from the inspected bytes"
      : `${plural(String(packet.fields.length), "header field")} · select one for its original byte coordinates`)}</p>
    {items.length > 0 && grid >= 2 && <table className="capture-analysis-fields" aria-label={`Header fields of packet ${packet.number}`}>
      <thead><tr><th scope="col">Layer</th><th scope="col">Field</th><th scope="col">Value</th><th scope="col">Original bytes</th></tr></thead>
      <tbody>{shown.map((item, at) => {
        const layer = layerOf(item), opens = at === 0 || layerOf(shown[at - 1]!) !== layer;
        const layerCell = <td className={opens ? "capture-analysis-layer" : "capture-analysis-layer capture-analysis-continued"}>{opens ? layer : ""}</td>;
        if (item.kind === "note") return <tr key={`note-${item.at}`} className="capture-analysis-note-row">{layerCell}<td colSpan={3} title={item.text}>{item.text}</td></tr>;
        const it = item.field, active = item.at === chosen;
        return <tr key={`field-${item.at}`} aria-selected={active} className={active ? "capture-analysis-selected" : undefined}>
          {layerCell}
          <td><button type="button" className="cell-action" aria-pressed={active} aria-label={`${it.layer} ${it.label}`} onClick={() => choose(item.at)}>{it.label}</button></td>
          <td className="capture-analysis-value" title={it.value}>{it.value}</td>
          <td>{narrow ? span(it.offset, it.last) : `${span(it.offset, it.last)} · ${it.length} B`}</td>
        </tr>;
      })}</tbody></table>}
    {pages > 1 && grid >= 2 && <div className="capture-analysis-pager">
      <span>{`Rows ${current * perPage + 1}–${current * perPage + shown.length} of ${items.length}`}</span>
      <button type="button" className="cell-action" disabled={current === 0} onClick={() => turn(current - 1)}>Earlier fields</button>
      <button type="button" className="cell-action" disabled={current >= pages - 1} onClick={() => turn(current + 1)}>Later fields</button>
    </div>}
  </>;
}

/** Rows beyond the panel height are counted, never silently dropped. */
function bounded<T>(rows: readonly T[], lines: number): { shown: readonly T[]; hidden: number } {
  if (rows.length <= lines) return { shown: rows, hidden: 0 };
  const shown = rows.slice(0, Math.max(0, lines - 1));
  return { shown, hidden: rows.length - shown.length };
}

function CaptureAnalysisView({ model: input }: ViewComponentProps) {
  const model = input as AnalysisModel, { analysis } = model, { index, decoder } = analysis;
  const id = useId();
  const [stored, setState] = useState<ViewState>(() => fresh(model.source));
  // Replaced or withdrawn data is a new prepared source: its filter, page, tab and selections never carry over.
  const state = stored.source === model.source ? stored : fresh(model.source);
  const update = (change: Partial<ViewState>) => setState({ ...state, ...change });
  const interfaces = useMemo(() => new Map<string, CaptureInterface>(index.interfaces.map(it => [it.id, it])), [index]);
  const summary = useMemo(() => counts(analysis), [analysis]);
  const query = state.query.trim().toLowerCase();
  const matched = useMemo(() => {
    const words = query.split(/\s+/).filter(Boolean);
    return words.length === 0 ? analysis.packets : analysis.packets.filter(row => words.every(word => hit(row, word)));
  }, [analysis, query]);
  const sampled = analysis.packets.length;
  const pages = Math.max(1, Math.ceil(matched.length / Math.max(1, model.rows)));
  const page = Math.min(state.page, pages - 1), first = page * model.rows;
  const shown = matched.slice(first, first + model.rows);
  const selected = state.selected === undefined ? undefined : analysis.packets[Number(state.selected) - 1];
  const fit = (value: string) => ellipsizeEnd(value, Math.max(8, model.columns));
  const choosePacket = (number: string) => setState({ source: model.source, query: state.query, page, tab: "packet", selected: number, fieldPage: 0 });
  const last = sampled === 0 ? undefined : analysis.packets[sampled - 1]!.number;

  const facts: Record<Line, () => ReactElement> = {
    header: () => <header className="capture-analysis-header" key="header"><strong>Packet header analysis</strong>
      <span className="capture-analysis-dim">{`${decoder.name} ${decoder.version}`}</span>
      <span className={analysis.complete ? "capture-analysis-dim" : "capture-analysis-warn"}>{analysis.complete ? "every container packet sampled" : "partial sample"}</span></header>,
    coverage: () => <p className="capture-analysis-meta" key="coverage">{fit(sampled === 0
      ? `No packets in this ${index.format} container · nothing sampled · header prefix ≤ ${grouped(analysis.headerBytes)} B`
      : `Sample packets 1–${last} of ${grouped(index.packetCount)} · limit ${analysis.packetLimit} · header prefix ≤ ${grouped(analysis.headerBytes)} B per packet`)}</p>,
    status: () => <p className={summary.issues > 0 ? "capture-analysis-warn" : "capture-analysis-meta"} key="status">{fit(sampled === 0
      ? "No decode statuses · no rows sampled" : `${summary.status} · ${summary.protocols}`)}</p>,
    source: () => <p className="capture-analysis-meta" key="source">{fit(`${index.format} ${grouped(index.rawBytes)} B · ${index.sha256} · index ${index.complete ? "complete" : "incomplete"}, ${plural(String(index.packets.length), "row")}${index.skipped === "0" ? "" : ` · ${plural(index.skipped, "block")} skipped`}`)}</p>,
    caveat: () => <p className="capture-analysis-note" key="caveat">{fit("Fixed headers only · checksums unverified · no payload, flow, RTT, reassembly or decryption · not traffic coverage")}</p>,
  };

  const tabs: { id: Tab; label: string }[] = [
    ...(model.table ? [{ id: "packet" as const, label: selected ? `Packet ${selected.number}` : "Packet" }] : []),
    { id: "decoder", label: "Decoder" }, { id: "limits", label: `Limits · ${analysis.limitations.length}` }];
  const tab = tabs.find(it => it.id === state.tab)?.id ?? tabs[0]!.id;
  const keys = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault(); event.stopPropagation();
    const current = tabs.findIndex(it => it.id === tab);
    const next = event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : (current + (event.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length;
    update({ tab: tabs[next]!.id });
    event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next]?.focus();
  };
  const decoderRows: readonly (readonly [string, string])[] = [
    ["Decoder", `${decoder.name} ${decoder.version} · ${decoder.runtime}`], ["Code", decoder.code], ["Container", decoder.container], ["Schema", decoder.schema],
    ["Options", `first ${plural(analysis.packetLimit, "packet")} · header prefix ${grouped(analysis.headerBytes)} B`],
    ["Capture", `${index.format} · ${grouped(index.rawBytes)} B · ${index.sha256}`],
    ["Index", `${plural(index.packetCount, "packet")} · ${plural(String(index.packets.length), "row")} · ${index.complete ? "complete" : "incomplete"} · ${plural(index.skipped, "skipped block")} · ${plural(String(index.interfaces.length), "interface")}`],
    ["Identity", "as recorded by the helper · not verified here · grants no access to original bytes"],
  ];
  const limits = [...(analysis.limitations.length === 0 ? [{ text: "The helper recorded no limitations", tone: "capture-analysis-warn" }] : analysis.limitations.map(it => ({ text: it, tone: undefined }))),
    { text: "This view: decoded means fixed headers decoded; checksum fields are shown unverified", tone: "capture-analysis-dim" },
    { text: "Not implemented: payload, options, flow, RTT, retransmission, decryption, reassembly", tone: "capture-analysis-dim" }];
  const panels: Record<Tab, () => ReactElement> = {
    packet: () => <PacketPanel packet={selected} link={selected ? interfaces.get(selected.interface)?.linkType : undefined} lines={model.panel}
      chosen={state.field !== undefined && selected && state.field < selected.fields.length ? state.field : undefined} page={state.fieldPage} narrow={model.narrow} fit={fit}
      choose={at => update({ field: at })} turn={next => update({ fieldPage: next })} />,
    decoder: () => { const { shown: rows, hidden } = bounded(decoderRows, model.panel); return <>
      <table className="capture-analysis-facts"><tbody>{rows.map(([name, value]) => <tr key={name}><th scope="row">{name}</th><td title={value}>{value}</td></tr>)}</tbody></table>
      {hidden > 0 && <p className="capture-analysis-dim">{`${plural(String(hidden), "more row")} need more height`}</p>}</>; },
    limits: () => { const { shown: rows, hidden } = bounded(limits, model.panel); return <>
      <ul className="capture-analysis-limits">{rows.map((it, at) => <li key={at} className={it.tone} title={it.text}>{it.text}</li>)}</ul>
      {hidden > 0 && <p className="capture-analysis-dim">{`${plural(String(hidden), "more limitation")} need more height`}</p>}</>; },
  };

  const wide = !model.narrow;
  return <section className={`capture-analysis-view capture-analysis-${model.mode}${model.narrow ? " capture-analysis-narrow" : ""}`} aria-label="Capture analysis">
    {model.lines.map(line => facts[line]())}
    {model.table && <>
      <input className="capture-analysis-filter" type="search" aria-label="Filter sampled packet rows" maxLength={MAX_QUERY}
        placeholder={`Filter ${grouped(String(sampled))} sampled rows · facts, all fields, notes`} value={state.query}
        onChange={event => update({ query: event.target.value.slice(0, MAX_QUERY), page: 0 })} />
      <table className="capture-analysis-packets" aria-label="Sampled packets"><thead><tr>
        <th scope="col">#</th>{wide && <th scope="col">If</th>}<th scope="col">Protocol</th><th scope="col">Status</th>
        {wide && <><th scope="col">Source</th><th scope="col">Destination</th><th scope="col">Captured</th></>}<th scope="col">Inspected</th>{wide && <th scope="col">Fields</th>}
      </tr></thead><tbody>{shown.map(row => {
        const active = row.number === state.selected, from = endpoint(row.source, row.sourcePort), to = endpoint(row.destination, row.destinationPort);
        return <tr key={row.number} aria-selected={active} className={active ? "capture-analysis-selected" : undefined}>
          <td><button type="button" className="cell-action" aria-pressed={active} onClick={() => choosePacket(row.number)}>{row.number}</button></td>
          {wide && <td>{row.interface}</td>}<td>{row.protocol}</td>
          <td className={row.status === "decoded" ? "capture-analysis-ok" : "capture-analysis-warn"}>{row.status}</td>
          {wide && <><td className={from === undefined ? "capture-analysis-dim" : undefined}>{from ?? "not decoded"}</td>
            <td className={to === undefined ? "capture-analysis-dim" : undefined}>{to ?? "not decoded"}</td><td>{row.captured}</td></>}
          <td>{row.inspected}</td>{wide && <td>{row.fields.length}</td>}
        </tr>;
      })}</tbody></table>
      <div className="capture-analysis-status" role="status">
        <span>{matched.length === 0 ? `No sampled rows match · ${grouped(String(sampled))} rows searched`
          : `${query ? `${grouped(String(matched.length))} of ${grouped(String(sampled))} sampled rows match` : `${grouped(String(sampled))} sampled rows`} · showing ${first + 1}–${first + shown.length} · search covers this sample only`}</span>
        {pages > 1 && <>
          <button type="button" className="cell-action" disabled={page === 0} onClick={() => update({ page: page - 1 })}>Earlier rows</button>
          <button type="button" className="cell-action" disabled={page >= pages - 1} onClick={() => update({ page: page + 1 })}>Later rows</button>
        </>}
      </div>
    </>}
    {model.panel > 0 && <div className="capture-analysis-detail">
      <div className="capture-analysis-tabs" role="tablist" aria-label="Capture analysis details" onKeyDown={keys}>
        {tabs.map(it => <button key={it.id} id={`${id}-${it.id}-tab`} type="button" role="tab" className="cell-action" tabIndex={tab === it.id ? 0 : -1}
          aria-selected={tab === it.id} aria-controls={`${id}-${it.id}`} onClick={() => update({ tab: it.id })}>{it.label}</button>)}
      </div>
      <div id={`${id}-${tab}`} role="tabpanel" aria-labelledby={`${id}-${tab}-tab`} className="capture-analysis-panel">{panels[tab]()}</div>
    </div>}
  </section>;
}
