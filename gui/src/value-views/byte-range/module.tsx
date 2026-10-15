import { useMemo, useState, type KeyboardEvent, type ReactElement } from "react";
import type { TypeShape } from "../../protocol";
import { byteSize, DecodedBytes } from "../../presentation/bytes";
import { ellipsizeEnd } from "../../presentation/columns";
import { pageSize, type Mode } from "../../presentation/types";
import { ViewInputError, type ValueViewModule, type ViewComponentProps } from "../contract";
import "./view.css";

/** One acknowledged `:read bytes` result: original coordinates and only the bytes that read supplied. */
const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const field = (type: TypeShape | undefined, name: string): TypeShape | undefined => type?.kind === "record" ? type.fields?.find(it => it.name === name)?.type : undefined;
const is = (name: string) => (type: TypeShape | undefined) => type?.kind === "primitive" && type.name === name;
const optionalText = (type: TypeShape | undefined) => type?.kind === "option" && is("TEXT")(type.element);
const textList = (type: TypeShape | undefined) => type?.kind === "list" && is("TEXT")(type.element);
const shape: Record<string, (type: TypeShape | undefined) => boolean> = {
  source: is("TEXT"), run: optionalText, revision: is("TEXT"), handle: optionalText, digest: optionalText, selection: textList,
  from: is("TEXT"), next: is("TEXT"), extent: is("TEXT"), text: is("BOOL"), extentExhausted: is("BOOL"), data: is("BYTES"),
};

/** The engine's own per-read ceiling; a larger payload is not a native byte range. */
export const MAX_RANGE_BYTES = 65_536;
const MAX_U64 = 18446744073709551615n;
const u64 = (value: unknown): value is string => typeof value === "string" && value.length <= 20 && /^(0|[1-9]\d*)$/.test(value) && BigInt(value) <= MAX_U64;
const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/** Canonical Base64 without decoding: whole quads, padding only at the end, and zero unused bits before it. */
export function canonicalBase64(value: string): boolean {
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) return false;
  if (value.endsWith("==")) return alphabet.indexOf(value[value.length - 3]!) % 16 === 0;
  if (value.endsWith("=")) return alphabet.indexOf(value[value.length - 2]!) % 4 === 0;
  return true;
}
function optional(value: unknown): { ok: boolean; text?: string } {
  if (!object(value)) return { ok: false };
  if (value.kind === "none") return { ok: true };
  return value.kind === "some" && typeof value.value === "string" ? { ok: true, text: value.value } : { ok: false };
}

export interface ByteRange {
  readonly source: string; readonly run?: string; readonly revision: string; readonly handle?: string; readonly digest?: string;
  readonly selection: readonly string[]; readonly from: string; readonly next: string; readonly extent: string;
  readonly text: boolean; readonly exhausted: boolean; readonly stored: string; readonly size: number;
}

/** Structural and arithmetic facts only; the payload is measured, never decoded, here. */
export function readByteRange(data: unknown): ByteRange | undefined {
  if (!object(data)) return;
  const { source, revision, selection, from, next, extent, text, extentExhausted } = data;
  const stored = data.data instanceof DecodedBytes ? data.data.stored : data.data;
  const run = optional(data.run), handle = optional(data.handle), digest = optional(data.digest);
  if (typeof source !== "string" || typeof revision !== "string" || !run.ok || !handle.ok || !digest.ok) return;
  if (!Array.isArray(selection) || selection.length > 128 || !selection.every(it => typeof it === "string")) return;
  if (!u64(from) || !u64(next) || !u64(extent) || typeof text !== "boolean" || typeof extentExhausted !== "boolean") return;
  if (typeof stored !== "string" || stored.length > Math.ceil(MAX_RANGE_BYTES / 3) * 4 || !canonicalBase64(stored)) return;
  const size = byteSize(stored), start = BigInt(from), end = BigInt(next);
  if (size > MAX_RANGE_BYTES || start > end || end > BigInt(extent) || end - start !== BigInt(size) || extentExhausted !== (end === BigInt(extent))) return;
  return { source, revision, selection: selection as string[], from, next, extent, text, exhausted: extentExhausted, stored, size,
    ...(run.text === undefined ? {} : { run: run.text }), ...(handle.text === undefined ? {} : { handle: handle.text }), ...(digest.text === undefined ? {} : { digest: digest.text }) };
}

export function matchesByteRange(type: TypeShape, data: unknown): boolean {
  return type.kind === "record" && Object.entries(shape).every(([name, check]) => check(field(type, name))) && readByteRange(data) !== undefined;
}

/** Decodes only bytes [start, end) of the supplied range: the base64 quads that cover them, nothing more. */
export function decodeSpan(stored: string, start: number, end: number): Uint8Array | undefined {
  if (end <= start) return new Uint8Array(0);
  const first = Math.floor(start / 3), last = Math.ceil(end / 3);
  try {
    const binary = atob(stored.slice(first * 4, last * 4)), skip = start - first * 3, out = new Uint8Array(end - start);
    if (binary.length < skip + out.length) return;
    for (let at = 0; at < out.length; at++) out[at] = binary.charCodeAt(skip + at);
    return out;
  } catch { return undefined; }
}

const hex = (byte: number) => byte.toString(16).padStart(2, "0");
const printable = (byte: number) => byte >= 0x20 && byte <= 0x7e;
const grouped = (digits: string) => digits.replace(/\B(?=(\d{3})+(?!\d))/g, ",");

type Line = "header" | "peek" | "scope" | "identity" | "stored";
interface ByteModel {
  readonly source: object;
  readonly range: ByteRange;
  readonly mode: Mode;
  readonly columns: number;
  readonly lines: readonly Line[];
  readonly perRow: number;
  /** Rows of the grid per page; zero when the grid is not drawn. */
  readonly rows: number;
  readonly pager: boolean;
}

export const byteRangeViewModule: ValueViewModule = {
  id: "byte-range",
  matches: matchesByteRange,
  prepare(_value, preparedData) {
    const data = preparedData as Record<string, unknown>;
    // Keep the payload undecoded; the view decodes only the slice it draws.
    if (typeof data.data !== "string") return { data: preparedData, pending: false };
    return { data: { ...data, data: new DecodedBytes(data.data, byteSize(data.data), undefined, undefined, undefined) }, pending: false };
  },
  present({ data, context }, host) {
    const range = readByteRange(data);
    if (!range || !object(data)) throw new ViewInputError("Byte range fields are malformed");
    const perRow = context.columns >= 90 ? 16 : context.columns >= 56 ? 8 : 4;
    const remaining = host.remaining();
    const wanted: Line[] = context.mode === "preview"
      ? ["header", ...(range.size > 0 ? ["peek" as const] : []), "identity"]
      : ["header", "scope", "identity", ...(range.handle !== undefined || range.digest !== undefined || range.selection.length > 0 ? ["stored" as const] : [])];
    const lines = wanted.slice(0, Math.max(1, Math.min(remaining, wanted.length)));
    let left = remaining - lines.length, rows = 0, pager = false;
    const needed = Math.ceil(range.size / perRow);
    // The selected-byte line and the column header are spent with the grid, never without it.
    if (context.mode !== "preview" && needed > 0 && left >= 3) {
      left -= 2;
      const page = Math.min(pageSize(), 64);
      if (needed <= left && needed <= page) rows = needed;
      else if (left >= 2) { pager = true; rows = Math.min(left - 1, page); }
    }
    const grid = rows > 0 ? 2 + rows + (pager ? 1 : 0) : 0;
    const ownLines = Math.min(remaining, lines.length + grid);
    host.spend(ownLines);
    const model: ByteModel = { source: data, range, mode: context.mode, columns: context.columns, lines, perRow, rows, pager };
    return { model, children: [], ownLines, summary: [{ text: `${grouped(String(range.size))} B`, tone: "ink" }] };
  },
  Component: ByteRangeView,
};

interface ViewState { readonly source: object; readonly page: number; readonly selected?: number }

function ByteRangeView({ model: input }: ViewComponentProps) {
  const model = input as ByteModel, { range, perRow } = model;
  const [stored, setState] = useState<ViewState>({ source: model.source, page: 0 });
  // A refreshed or withdrawn read is a new prepared source: a byte selected in the old one never persists.
  const state = stored.source === model.source ? stored : { source: model.source, page: 0 };
  const from = BigInt(range.from);
  const perPage = Math.max(1, model.rows) * perRow, pages = Math.max(1, Math.ceil(range.size / perPage));
  const page = Math.min(state.page, pages - 1), start = page * perPage, end = Math.min(range.size, start + perPage);
  const peekEnd = Math.min(range.size, perRow);
  const drawn = useMemo(() => model.rows > 0 ? decodeSpan(range.stored, start, end) : undefined, [range.stored, model.rows, start, end]);
  const peek = useMemo(() => model.lines.includes("peek") ? decodeSpan(range.stored, 0, peekEnd) : undefined, [range.stored, model.lines, peekEnd]);
  const width = (from + BigInt(Math.max(0, range.size - 1))).toString().length;
  const last = range.size === 0 ? undefined : (BigInt(range.next) - 1n).toString();
  const fit = (text: string) => ellipsizeEnd(text, Math.max(8, model.columns));
  const choose = (selected: number) => {
    const at = Math.max(0, Math.min(range.size - 1, selected));
    setState({ source: model.source, selected: at, page: Math.floor(at / perPage) });
  };
  const keys = (event: KeyboardEvent) => {
    const steps: Readonly<Record<string, number>> = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -perRow, ArrowDown: perRow };
    const step = steps[event.key];
    if (step === undefined && event.key !== "Home" && event.key !== "End") return;
    event.preventDefault(); event.stopPropagation();
    const current = state.selected ?? start;
    choose(event.key === "Home" ? 0 : event.key === "End" ? range.size - 1 : current + step!);
  };
  const facts: Record<Line, () => ReactElement> = {
    header: () => <header className="byte-range-header" key="header"><strong>{range.size === 0 ? `No bytes at offset ${range.from}` : `Bytes ${range.from}–${last}`}</strong>
      <span className="byte-range-dim">{`${grouped(String(range.size))} B of ${grouped(range.extent)} B · ${range.text ? "Text field" : "Bytes field"}${range.exhausted ? " · reaches end" : ""}`}</span></header>,
    peek: () => <p className="byte-range-peek" key="peek">{peek ? fit(`${Array.from(peek, hex).join(" ")}${range.size > peekEnd ? " …" : ""}`) : "Bytes unreadable"}</p>,
    scope: () => <p className="byte-range-note" key="scope">{fit(range.exhausted
      ? "This read only · reaches the source end · read another range with :read bytes"
      : `This read only · next offset ${range.next} · read another range with :read bytes`)}</p>,
    identity: () => <p className="byte-range-dim" key="identity">{fit(`${range.source} · run ${range.run ?? "none"} · revision ${range.revision}`)}</p>,
    stored: () => <p className="byte-range-dim" key="stored">{fit([range.handle === undefined ? "resident value" : `stored ${range.handle}`, range.digest,
      range.selection.length ? `field ${range.selection.join(".")}` : undefined].filter(Boolean).join(" · "))}</p>,
  };
  const selected = state.selected !== undefined && state.selected < range.size ? state.selected : undefined;
  const selectedByte = selected !== undefined && selected >= start && selected < end && drawn ? drawn[selected - start] : undefined;
  const rows: ReactElement[] = [];
  if (drawn) for (let row = 0; row * perRow < drawn.length; row++) {
    const cells = drawn.subarray(row * perRow, row * perRow + perRow), base = start + row * perRow;
    rows.push(<div className="byte-range-row" role="row" key={base}>
      <span className="byte-range-offset" role="rowheader">{(from + BigInt(base)).toString().padStart(width, " ")}</span>
      <span className="byte-range-hex">{Array.from(cells, (byte, at) => <button key={at} type="button" role="gridcell" tabIndex={-1}
        aria-selected={base + at === selected} className={base + at === selected ? "byte-range-byte byte-range-chosen" : "byte-range-byte"}
        onClick={() => choose(base + at)}>{hex(byte)}</button>)}</span>
      <span className="byte-range-ascii" aria-hidden="true">{Array.from(cells, (byte, at) => <span key={at} className={base + at === selected ? "byte-range-chosen" : printable(byte) ? undefined : "byte-range-dim"}>{printable(byte) ? String.fromCharCode(byte) : "·"}</span>)}</span>
    </div>);
  }
  return <section className={`byte-range-view byte-range-${model.mode}`} aria-label="Byte range">
    {model.lines.map(line => facts[line]())}
    {model.rows > 0 && <>
      <p className="byte-range-selection" role="status">{selected === undefined ? "Select a byte to see its original offset"
        : selectedByte === undefined ? `Offset ${(from + BigInt(selected)).toString()} selected`
        : `Offset ${(from + BigInt(selected)).toString()} · 0x${hex(selectedByte)} · ${selectedByte}${printable(selectedByte) ? ` · ASCII "${String.fromCharCode(selectedByte)}"` : " · not printable ASCII"}`}</p>
      <div className="byte-range-row byte-range-columns" aria-hidden="true">
        <span className="byte-range-offset">{"offset".padStart(width, " ")}</span>
        <span className="byte-range-hex">{Array.from({ length: perRow }, (_, at) => <span key={at} className="byte-range-byte">{hex(at)}</span>)}</span>
        <span className="byte-range-ascii">ASCII</span>
      </div>
      {drawn ? <div className="byte-range-grid" role="grid" aria-label={`Bytes ${(from + BigInt(start)).toString()} to ${(from + BigInt(end - 1)).toString()}`} tabIndex={0} onKeyDown={keys}>{rows}</div>
        : <p className="byte-range-warn">Bytes unreadable</p>}
      {model.pager && <div className="byte-range-pager">
        <span>{`Rows ${(from + BigInt(start)).toString()}–${(from + BigInt(end - 1)).toString()} of this read`}</span>
        <button type="button" className="cell-action" disabled={page === 0} onClick={() => setState({ ...state, page: page - 1 })}>Earlier bytes</button>
        <button type="button" className="cell-action" disabled={page >= pages - 1} onClick={() => setState({ ...state, page: page + 1 })}>Later bytes</button>
      </div>}
    </>}
  </section>;
}
