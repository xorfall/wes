import { useMemo, useState, type ReactElement } from "react";
import { isExactNumber } from "../../exact-json";
import type { TypeShape } from "../../protocol";
import { ellipsizeEnd } from "../../presentation/columns";
import { pageSize, type Mode } from "../../presentation/types";
import { ViewInputError, type ValueViewModule, type ViewComponentProps } from "../contract";
import "./view.css";

/** A container index over one offline capture file. It is not a dissector: nothing here decodes packets. */
const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const field = (type: TypeShape | undefined, name: string): TypeShape | undefined => type?.kind === "record" ? type.fields?.find(it => it.name === name)?.type : undefined;
const is = (name: string) => (type: TypeShape | undefined) => type?.kind === "primitive" && type.name === name;
const record = (checks: Record<string, (type: TypeShape | undefined) => boolean>) => (type: TypeShape | undefined) =>
  type?.kind === "record" && Object.entries(checks).every(([name, check]) => check(field(type, name)));
const listOf = (check: (type: TypeShape | undefined) => boolean) => (type: TypeShape | undefined) => type?.kind === "list" && check(type.element);
const optionOf = (check: (type: TypeShape | undefined) => boolean) => (type: TypeShape | undefined) => type?.kind === "option" && check(type.element);

const interfaceShape = record({ id: is("INT"), section: is("INT"), linkType: is("INT"), snapLength: is("INT"), timestampScale: is("TEXT"), timestampOffset: is("TEXT") });
const packetShape = record({ number: is("INT"), interface: is("INT"), recordOffset: is("TEXT"), dataOffset: is("TEXT"), capturedLength: is("INT"), originalLength: is("INT"), timestampTicks: optionOf(is("TEXT")) });
const indexShape = record({ format: is("TEXT"), rawBytes: is("TEXT"), sha256: is("TEXT"), packetCount: is("TEXT"), indexComplete: is("BOOL"), skippedBlocks: is("INT"), interfaces: listOf(interfaceShape), packets: listOf(packetShape) });

/** The index supplies at most this many packet rows; counts beyond them are facts, not rows. */
export const MAX_INDEX_ROWS = 512;
/** The adapter describes at most this many interfaces across all sections. */
export const MAX_INTERFACES = 256;
const MAX_U64 = 18446744073709551615n;
const MIN_I64 = -9223372036854775808n, MAX_I64 = 9223372036854775807n;
/** Decimal 10^n or binary 2^n, n from 0 to 127, kept as the exact text the file declared. */
const scaleForm = /^(10|2)\^(0|[1-9]\d?|1[01]\d|12[0-7])$/;
const i64 = (value: unknown): value is string => typeof value === "string" && /^(0|-?[1-9]\d{0,18})$/.test(value) && BigInt(value) >= MIN_I64 && BigInt(value) <= MAX_I64;
const u64 = (value: unknown): value is string => typeof value === "string" && value.length <= 20 && /^(0|[1-9]\d*)$/.test(value) && BigInt(value) <= MAX_U64;
function natural(value: unknown): string | undefined {
  const text = typeof value === "number" ? (Number.isSafeInteger(value) ? String(value) : undefined)
    : isExactNumber(value) && value.text.length <= 20 && /^(0|[1-9]\d*)$/.test(value.text) ? value.text : undefined;
  return text !== undefined && /^(0|[1-9]\d*)$/.test(text) ? text : undefined;
}
/** Thousands separators applied to decimal text; never through Number, so u64 stays exact. */
export const grouped = (digits: string) => digits.replace(/\B(?=(\d{3})+(?!\d))/g, ",");

export interface CaptureInterface { readonly id: string; readonly section: string; readonly linkType: string; readonly snapLength: string; readonly scale: string; readonly offset: string }
export interface CapturePacket {
  readonly number: string; readonly interface: string; readonly recordOffset: string; readonly dataOffset: string;
  readonly captured: string; readonly original: string; readonly ticks?: string; readonly haystack: string;
}
export interface CaptureIndex {
  readonly format: "pcap" | "pcapng"; readonly rawBytes: string; readonly sha256: string; readonly packetCount: string;
  readonly complete: boolean; readonly skipped: string; readonly interfaces: readonly CaptureInterface[]; readonly packets: readonly CapturePacket[];
}

function readInterface(value: unknown): CaptureInterface | undefined {
  if (!object(value)) return;
  const id = natural(value.id), section = natural(value.section), linkType = natural(value.linkType), snapLength = natural(value.snapLength);
  const scale = value.timestampScale, offset = value.timestampOffset;
  if (id === undefined || section === undefined || linkType === undefined || snapLength === undefined) return;
  if (typeof scale !== "string" || !scaleForm.test(scale) || !i64(offset)) return;
  return { id, section, linkType, snapLength, scale, offset };
}
function readPacket(value: unknown, rawBytes: bigint): CapturePacket | undefined {
  if (!object(value)) return;
  const number = natural(value.number), iface = natural(value.interface), captured = natural(value.capturedLength), original = natural(value.originalLength);
  const { recordOffset, dataOffset, timestampTicks: ticks } = value;
  if (number === undefined || number === "0" || iface === undefined || captured === undefined || original === undefined) return;
  if (BigInt(captured) > BigInt(original) || !u64(recordOffset) || !u64(dataOffset) || !object(ticks)) return;
  if (!(ticks.kind === "none" || (ticks.kind === "some" && u64(ticks.value)))) return;
  // Coordinates must lie inside the file the digest names; anything else is not this index.
  if (BigInt(recordOffset) > BigInt(dataOffset) || BigInt(dataOffset) + BigInt(captured) > rawBytes) return;
  const tick = ticks.kind === "some" ? ticks.value as string : undefined;
  return { number, interface: iface, recordOffset, dataOffset, captured, original, ...(tick === undefined ? {} : { ticks: tick }),
    haystack: `#${number} if${iface} ${recordOffset} ${dataOffset} ${captured} ${original} ${tick ?? "unknown"}` };
}

const parsed = new WeakMap<object, CaptureIndex | null>();
/** The exact facts of a well-formed index, or undefined. Rows are checked whole before anything is shown. */
export function readCaptureIndex(data: unknown): CaptureIndex | undefined {
  if (!object(data)) return;
  if (parsed.has(data)) return parsed.get(data) ?? undefined;
  const read = (): CaptureIndex | undefined => {
    const { format, rawBytes, sha256, packetCount, indexComplete, interfaces, packets } = data;
    const skipped = natural(data.skippedBlocks);
    if ((format !== "pcap" && format !== "pcapng") || !u64(rawBytes) || !u64(packetCount) || typeof indexComplete !== "boolean" || skipped === undefined) return;
    if (typeof sha256 !== "string" || !/^sha256:[0-9a-f]{64}$/.test(sha256)) return;
    if (!Array.isArray(interfaces) || interfaces.length > MAX_INTERFACES || !Array.isArray(packets) || packets.length > MAX_INDEX_ROWS) return;
    // A complete index supplies every row; an incomplete one counts more packets than it supplies.
    const count = BigInt(packetCount), supplied = BigInt(packets.length);
    if (indexComplete ? count !== supplied : count <= supplied) return;
    const shownInterfaces: CaptureInterface[] = [], ids = new Set<string>();
    for (const it of interfaces) {
      const row = readInterface(it);
      if (!row || ids.has(row.id)) return;
      ids.add(row.id); shownInterfaces.push(row);
    }
    const size = BigInt(rawBytes), rows: CapturePacket[] = [];
    for (const it of packets) {
      const row = readPacket(it, size);
      // The first page is contiguous from packet 1 and refers only to described interfaces.
      if (!row || row.number !== String(rows.length + 1) || !ids.has(row.interface)) return;
      rows.push(row);
    }
    return { format: format as CaptureIndex["format"], rawBytes, sha256, packetCount, complete: indexComplete, skipped, interfaces: shownInterfaces, packets: rows };
  };
  const index = read();
  parsed.set(data, index ?? null);
  return index;
}

export function matchesCaptureIndex(type: TypeShape, data: unknown): boolean {
  return indexShape(type) && readCaptureIndex(data) !== undefined;
}

type Line = "header" | "meta" | "warn" | "scope" | "digest" | "caveat";
const DETAIL_LINES = 6;
interface CaptureModel {
  readonly source: object;
  readonly index: CaptureIndex;
  readonly mode: Mode;
  readonly columns: number;
  readonly narrow: boolean;
  readonly lines: readonly Line[];
  /** Filter, column header and status are drawn only when at least one row fits beside them. */
  readonly table: boolean;
  readonly rows: number;
  readonly detail: boolean;
}

const plural = (count: string, word: string) => `${grouped(count)} ${word}${count === "1" ? "" : "s"}`;
function warning(index: CaptureIndex) {
  const parts = [index.complete ? undefined : "Index incomplete · counts cover only what was indexed", index.skipped === "0" ? undefined : `${plural(index.skipped, "block")} skipped`];
  const text = parts.filter(Boolean).join(" · ");
  return text || undefined;
}

export const captureIndexViewModule: ValueViewModule = {
  id: "capture-index",
  matches: matchesCaptureIndex,
  present({ data, context }, host) {
    const index = readCaptureIndex(data);
    if (!index || !object(data)) throw new ViewInputError("Capture index fields are malformed");
    const remaining = host.remaining();
    const wanted: Line[] = context.mode === "preview"
      ? ["header", "meta", ...(warning(index) ? ["warn" as const] : [])]
      : ["header", "meta", ...(warning(index) ? ["warn" as const] : []), "scope", "digest", "caveat"];
    const lines = wanted.slice(0, Math.max(1, Math.min(remaining, wanted.length)));
    let left = remaining - lines.length, table = false, rows = 0, detail = false;
    // Filter + column header + status + one row is the least table worth drawing.
    if (context.mode !== "preview" && index.packets.length > 0 && left >= 4) {
      table = true; left -= 3;
      if (left >= DETAIL_LINES + 1) { detail = true; left -= DETAIL_LINES; }
      rows = Math.max(1, Math.min(index.packets.length, left, pageSize()));
    }
    const ownLines = Math.min(remaining, lines.length + (table ? 3 + rows : 0) + (detail ? DETAIL_LINES : 0));
    host.spend(ownLines);
    const model: CaptureModel = { source: data, index, mode: context.mode, columns: context.columns, narrow: context.columns < 72, lines, table, rows, detail };
    return { model, children: [], ownLines, summary: [{ text: plural(index.packetCount, "packet"), tone: "ink" }] };
  },
  Component: CaptureIndexView,
};

function Detail({ packet, interfaces }: { packet: CapturePacket | undefined; interfaces: ReadonlyMap<string, CaptureInterface> }) {
  if (!packet) return <div className="capture-index-detail" aria-label="Selected packet"><p className="capture-index-dim">Select a packet row to see its index facts.</p></div>;
  // Validation guarantees every packet names a described interface.
  const iface = interfaces.get(packet.interface)!;
  const clock = `scale ${iface.scale} · offset ${iface.offset} s`;
  const captured = BigInt(packet.captured), original = BigInt(packet.original);
  const end = BigInt(packet.dataOffset) + captured;
  return <div className="capture-index-detail" aria-label="Selected packet">
    <p><strong>{`Packet ${packet.number} · interface ${packet.interface}`}</strong></p>
    <p>{`Record offset ${packet.recordOffset} · data offset ${packet.dataOffset}`}</p>
    <p>{captured === 0n ? "No captured data bytes" : `Captured data at original file bytes ${packet.dataOffset} to ${(end - 1n).toString()}`}</p>
    <p>{`Captured ${grouped(packet.captured)} B · original ${grouped(packet.original)} B${captured < original ? " · fewer bytes stored than the original length" : ""}`}</p>
    <p>{packet.ticks === undefined ? `Timestamp unknown · not recorded in this index · interface ${clock}` : `Timestamp ticks ${packet.ticks} · ${clock}`}</p>
    <p>{`Interface ${iface.id} · section ${iface.section} · link type ${iface.linkType} · snap length ${grouped(iface.snapLength)}`}</p>
  </div>;
}

interface ViewState { readonly source: object; readonly query: string; readonly page: number; readonly selected?: string }

function CaptureIndexView({ model: input }: ViewComponentProps) {
  const model = input as CaptureModel, { index } = model;
  const [stored, setState] = useState<ViewState>({ source: model.source, query: "", page: 0 });
  // A refreshed or withdrawn value is a new prepared source: its old selection and filter never carry over.
  const state = stored.source === model.source ? stored : { source: model.source, query: "", page: 0 };
  const update = (change: Partial<ViewState>) => setState({ ...state, ...change });
  const interfaces = useMemo(() => new Map(index.interfaces.map(it => [it.id, it])), [index]);
  const query = state.query.trim().toLowerCase(), terms = query.split(/\s+/).filter(Boolean);
  const matched = useMemo(() => {
    const words = query.split(/\s+/).filter(Boolean);
    return words.length === 0 ? index.packets : index.packets.filter(row => words.every(word => row.haystack.toLowerCase().includes(word)));
  }, [index, query]);
  const pages = Math.max(1, Math.ceil(matched.length / Math.max(1, model.rows)));
  const page = Math.min(state.page, pages - 1), first = page * model.rows;
  const shown = matched.slice(first, first + model.rows);
  const selected = state.selected === undefined ? undefined : index.packets.find(row => row.number === state.selected);
  const bounded = BigInt(index.packets.length) < BigInt(index.packetCount);
  const warn = warning(index);
  const fit = (text: string) => ellipsizeEnd(text, Math.max(8, model.columns));
  const facts: Record<Line, () => ReactElement> = {
    header: () => <header className="capture-index-header" key="header"><strong>{`${index.format} capture index`}</strong>
      <span className={index.complete ? "capture-index-dim" : "capture-index-warn"}>{index.complete ? "index complete" : "index incomplete"}</span></header>,
    meta: () => <p className="capture-index-meta" key="meta">{fit(`${plural(index.packetCount, "packet")} · ${grouped(index.rawBytes)} B${model.lines.includes("digest") ? "" : ` · ${index.sha256.slice(0, 19)}…`}`)}</p>,
    warn: () => <p className="capture-index-warn" role="status" key="warn">{fit(warn ?? "")}</p>,
    scope: () => <p className="capture-index-note" key="scope">{fit(index.packets.length === 0 ? "No packet rows in this index view"
      : bounded ? `Rows for packets ${index.packets[0]!.number}–${index.packets[index.packets.length - 1]!.number} only · ${grouped(String(index.packets.length))} of ${grouped(index.packetCount)} · search covers these rows`
      : `All ${grouped(String(index.packets.length))} indexed packet rows are here · search covers these rows`)}</p>,
    digest: () => <p className="capture-index-meta" key="digest">{fit(`File ${index.sha256}`)}</p>,
    caveat: () => <p className="capture-index-note" key="caveat">{fit("File structure only · no addresses, protocols or payloads · says nothing about drops or traffic coverage")}</p>,
  };
  const wide = !model.narrow;
  return <section className={`capture-index-view capture-index-${model.mode}${model.narrow ? " capture-index-narrow" : ""}`} aria-label="Capture index">
    {model.lines.map(line => facts[line]())}
    {model.table && <>
      <input className="capture-index-filter" type="search" aria-label="Filter indexed packet rows" placeholder={`Filter ${grouped(String(index.packets.length))} indexed rows`}
        value={state.query} onChange={event => update({ query: event.target.value, page: 0 })} />
      <table className="capture-index-packets"><thead><tr>
        <th scope="col">#</th><th scope="col">If</th>{wide && <th scope="col">Record offset</th>}<th scope="col">Data offset</th><th scope="col">Captured</th>
        {wide && <><th scope="col">Original</th><th scope="col">Timestamp ticks</th></>}
      </tr></thead><tbody>{shown.map(row => <tr key={row.number} aria-selected={row.number === state.selected} className={row.number === state.selected ? "capture-index-selected" : undefined}>
        <td><button type="button" className="cell-action" aria-pressed={row.number === state.selected} onClick={() => update({ selected: row.number, page })}>{row.number}</button></td>
        <td>{row.interface}</td>{wide && <td>{row.recordOffset}</td>}<td>{row.dataOffset}</td><td>{row.captured}</td>
        {wide && <><td>{row.original}</td><td className={row.ticks === undefined ? "capture-index-dim" : undefined}>{row.ticks ?? "unknown"}</td></>}
      </tr>)}</tbody></table>
      <div className="capture-index-status" role="status">
        <span>{matched.length === 0 ? `No indexed rows match · ${grouped(String(index.packets.length))} rows searched`
          : `${terms.length ? `${grouped(String(matched.length))} of ${grouped(String(index.packets.length))} indexed rows match` : `${grouped(String(matched.length))} indexed rows`} · showing ${first + 1}–${first + shown.length}`}</span>
        {pages > 1 && <>
          <button type="button" className="cell-action" disabled={page === 0} onClick={() => update({ page: page - 1 })}>Earlier rows</button>
          <button type="button" className="cell-action" disabled={page >= pages - 1} onClick={() => update({ page: page + 1 })}>Later rows</button>
        </>}
      </div>
      {model.detail && <Detail packet={selected} interfaces={interfaces} />}
    </>}
  </section>;
}
