import { parseExactJson, stringifyExactJson } from "../../exact-json";
import { useEffect, useId, useMemo, useState } from "react";
import type { TypeShape } from "../../protocol";
import { byteSize, DecodedBytes, decodeBytes } from "../../presentation/bytes";
import { ellipsizeEnd } from "../../presentation/columns";
import type { Mode, Tone } from "../../presentation/types";
import { matchesHttp, decodeBody, headerValue, max_decoded_body, textBody, type Header, type HttpResponse } from "../../views/http-response";
import type { ValueViewModule, ViewComponentProps, ViewValue } from "../contract";
import "./view.css";

const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const primitive = (type: TypeShape | undefined, name: string) => type?.kind === "primitive" && type.name === name;
const field = (type: TypeShape, name: string): TypeShape | undefined => type.kind === "record" ? type.fields?.find(it => it.name === name)?.type : undefined;
export { matchesHttp } from "../../views/http-response";

export function statusTone(status: number): Tone { return status >= 400 ? "warn" : status >= 200 && status < 300 ? "ok" : "meta"; }
const reasons: Readonly<Record<number, string>> = { 200: "OK", 201: "Created", 202: "Accepted", 204: "No Content", 205: "Reset Content", 206: "Partial Content", 301: "Moved Permanently", 302: "Found", 304: "Not Modified", 307: "Temporary Redirect", 308: "Permanent Redirect", 400: "Bad Request", 401: "Unauthorized", 403: "Forbidden", 404: "Not Found", 405: "Method Not Allowed", 409: "Conflict", 415: "Unsupported Media Type", 422: "Unprocessable Content", 429: "Too Many Requests", 500: "Internal Server Error", 502: "Bad Gateway", 503: "Service Unavailable", 504: "Gateway Timeout" };
export const statusLabel = (status: number) => `${status}${reasons[status] ? ` ${reasons[status]}` : ""}`;
function media(headers: readonly Header[]) {
  const values = headers.filter(it => it.name.toLowerCase() === "content-type").map(it => it.value.trim());
  return { contentType: values[0] ?? "", ambiguous: new Set(values.map(it => it.toLowerCase())).size > 1 };
}
function json(text: string | undefined, contentType: string) {
  if (text === undefined || !/^application\/(?:[\w.-]+\+)?json(?:\s*;|$)/i.test(contentType)) return undefined;
  try { return { value: parseExactJson(text) as unknown }; } catch { return undefined; }
}
function reading(stored: string, shown: { text?: string; problem?: string; encoding?: string }, contentType: string): DecodedBytes {
  const parsed = json(shown.text, contentType);
  const jsonProblem = shown.text !== undefined && shown.text.length > 0 && /^application\/(?:[\w.-]+\+)?json(?:\s*;|$)/i.test(contentType) && !parsed
    ? "Body is not valid JSON; shown as text" : undefined;
  return new DecodedBytes(stored, byteSize(stored), shown.text, shown.problem ?? jsonProblem, shown.encoding, parsed);
}
function wire(value: ViewValue): HttpResponse | undefined {
  if (!matchesHttp(value.type, value.data) || !primitive(field(value.type, "body"), "BYTES")) return;
  const data = value.data as Record<string, unknown>;
  if (typeof data.body !== "string" || typeof data.bodyKind === "string") return;
  return { status: Number(data.status), version: String(data.version), headers: data.headers as Header[], body: data.body, bytes: true };
}
function bodySync(value: ViewValue): DecodedBytes | undefined {
  const response = wire(value); if (!response) return;
  const { contentType, ambiguous } = media(response.headers), size = byteSize(response.body);
  if (ambiguous) return new DecodedBytes(response.body, size, undefined, "Conflicting Content-Type headers; body retained as bytes", undefined);
  if (size > max_decoded_body()) return new DecodedBytes(response.body, size, undefined, "Body exceeds the 16 MiB decoding limit", undefined);
  if (headerValue(response, "content-encoding").split(",").some(it => it.trim() && it.trim().toLowerCase() !== "identity")) {
    return new DecodedBytes(response.body, size, undefined, undefined, undefined, undefined, true);
  }
  try {
    const bytes = Uint8Array.from(atob(response.body), char => char.charCodeAt(0));
    return reading(response.body, textBody(bytes, contentType), contentType);
  } catch { return new DecodedBytes(response.body, size, undefined, "Invalid base64 body", undefined); }
}

interface ResponseIssue { readonly path: string; readonly code: string; readonly message: string }
interface HttpModel {
  readonly validation?: { state: string; issues: readonly ResponseIssue[] };
  readonly mode: Mode;
  readonly narrow: boolean;
  readonly oneLine: boolean;
  readonly status: number;
  readonly version: string;
  readonly compactMeta: string;
  readonly headers: readonly Header[];
  readonly note?: string;
  readonly pending: boolean;
  readonly showMeta: boolean;
  readonly showNote: boolean;
  readonly empty: boolean;
  readonly original?: string;
  readonly notes: readonly string[];
}

function responseNotes(response: Record<string, unknown>, headers: readonly Header[], problem?: string): string[] {
  const notes: string[] = [];
  for (const name of ["location", "retry-after", "www-authenticate"]) {
    for (const header of headers.filter(it => it.name.toLowerCase() === name)) {
      const label = name === "location" ? "Location" : name === "retry-after" ? "Retry-After" : "WWW-Authenticate";
      notes.push(`${label}: ${ellipsizeEnd(header.value, 240)}`);
    }
  }
  if (/^application\/problem\+json(?:\s*;|$)/i.test(media(headers).contentType)) {
    const body = response.body instanceof DecodedBytes ? response.body.json?.value : response.body;
    if (object(body)) for (const key of ["title", "detail"]) if (typeof body[key] === "string" && body[key]) notes.push(ellipsizeEnd(body[key], 400));
  }
  if (problem) notes.push(problem);
  else if (response.bodyKind === "bytes") notes.push("Body kept as bytes");
  return notes.slice(0, 8);
}

export const httpViewModule: ValueViewModule = {
  id: "http",
  matches: matchesHttp,
  prepare(value, preparedData) {
    const body = bodySync(value);
    const data = preparedData as Record<string, unknown>;
    // Retained evidence is decoded only when Raw is opened, independently of the body reading.
    const original = primitive(field(value.type, "originalBody"), "BYTES") && typeof data.originalBody === "string"
      ? new DecodedBytes(data.originalBody, byteSize(data.originalBody), undefined, undefined, undefined) : data.originalBody;
    return { data: { ...data, ...(original === undefined ? {} : { originalBody: original }), ...(body ? { body } : {}) }, pending: body?.pending ?? false };
  },
  async prepareAsync(value, preparedData, signal) {
    const response = wire(value);
    if (!response) return { data: preparedData, pending: false };
    const shown = await decodeBody(response, signal);
    const body = reading(response.body, shown, media(response.headers).contentType);
    return { data: { ...(preparedData as object), body }, pending: false };
  },
  present({ type, data, context }, host) {
    const response = data as Record<string, unknown>, headers = response.headers as Header[];
    const body = response.body, decoded = body instanceof DecodedBytes ? body : undefined;
    const original = decoded?.stored ?? (response.originalBody instanceof DecodedBytes ? response.originalBody.stored : undefined);
    const contentType = media(headers).contentType;
    const evidence = response.validation;
    const validation = object(evidence) && typeof evidence.state === "string" && Array.isArray(evidence.issues)
      ? { state: evidence.state, issues: evidence.issues.filter((i): i is ResponseIssue => object(i) && typeof i.path === "string" && typeof i.code === "string" && typeof i.message === "string") } : undefined;
    const empty = response.bodyKind === "empty" || (decoded !== undefined && !decoded.pending && decoded.size === 0);
    const notes = responseNotes(response, headers, decoded?.problem);
    const contractNote = validation?.state === "mismatch" ? `Documented shape differs${validation.issues.length ? ` · ${validation.issues.length} reported issue${validation.issues.length === 1 ? "" : "s"}` : ""}`
      : validation?.state === "unreadable" ? "Body could not be read against the documented shape" : undefined;
    const note = contractNote ?? notes[0];
    // Reserve metadata/notices before descending so they cannot displace body rows after layout.
    const ownLines = Math.min(host.remaining(), context.mode === "preview" ? (note ? 3 : 2) : (note ? 6 : 5));
    host.spend(ownLines);
    const bodyData = empty ? "" : decoded?.json ? decoded.json.value : body;
    const bodyType = decoded?.json ? { kind: "unknown" as const } : field(type, "body")!;
    const children = host.remaining() > 0 ? [host.child("body", bodyType, bodyData)] : [];
    const size = decoded?.size ?? (original === undefined ? undefined : byteSize(original));
    const meta = [contentType || "Content-Type not provided", size === undefined ? undefined : `${size.toLocaleString("en-US")} B`].filter(Boolean).join(" · ");
    const model: HttpModel = { mode: context.mode, narrow: context.columns < 60, oneLine: context.mode === "preview" && ownLines < 2, status: Number(response.status), version: String(response.version),
      compactMeta: ellipsizeEnd(meta, context.columns), headers, validation,
      note, notes, showMeta: ownLines > 1, showNote: ownLines > 2, pending: decoded?.pending ?? false, empty, original };
    return { model, children, ownLines, summary: [{ text: String(model.status), tone: "ink" }] };
  },
  Component: HttpView,
};

function Copy({ text, label }: { text: () => string; label: string }) {
  const [state, setState] = useState("");
  useEffect(() => { if (!state) return; const timer = setTimeout(() => setState(""), 1800); return () => clearTimeout(timer); }, [state]);
  return <button className="screen-chip cell-action" type="button" onClick={() => {
    if (!navigator.clipboard) { setState("Copy unavailable"); return; }
    void navigator.clipboard.writeText(text()).then(() => setState("Copied"), () => setState("Copy failed"));
  }}>{state || label}</button>;
}

function OriginalContent({ encoded }: { encoded: string }) {
  const [limit, setLimit] = useState(16_384);
  const text = useMemo(() => byteSize(encoded) <= max_decoded_body() ? decodeBytes(encoded) : undefined, [encoded]);
  const [formatted, setFormatted] = useState(false);
  const [wrap, setWrap] = useState(true);
  const reading = useMemo(() => {
    if (!formatted || text === undefined) return { text: text ?? encoded };
    try { return { text: stringifyExactJson(parseExactJson(text), 2) }; }
    catch { return { text, problem: "JSON formatting unavailable · showing original content" }; }
  }, [formatted, text, encoded]);
  const content = reading.text;
  const [url, setUrl] = useState<string>();
  useEffect(() => {
    try {
      const bytes = Uint8Array.from(atob(encoded), char => char.charCodeAt(0));
      const href = URL.createObjectURL(new Blob([bytes], { type: "application/octet-stream" }));
      setUrl(href); return () => URL.revokeObjectURL(href);
    } catch { setUrl(undefined); }
  }, [encoded]);
  return <div className="http-raw-body">
    <div className="http-view-tools"><span className="mono-dim">{text === undefined ? "Base64 · retained bytes" : "UTF-8 · retained bytes"}</span>
      {text !== undefined && <Copy text={() => text} label="Copy original" />}
      <Copy text={() => encoded} label="Copy base64" />
      {url && <a className="cell-action" href={url} download="body.bin">Download body</a>}</div>
    <div className="http-raw-display-tools" role="group" aria-label="Body display">
      {text !== undefined && <><button className="cell-action" aria-pressed={!formatted} onClick={() => { setFormatted(false); setLimit(16_384); }}>Original</button>
      <button className="cell-action" aria-pressed={formatted} onClick={() => { setFormatted(true); setLimit(16_384); }}>Formatted JSON</button></>}
      <button className="cell-action" aria-pressed={wrap} onClick={() => setWrap(it => !it)}>Wrap</button>
    </div>
    {formatted && !reading.problem && <p className="http-raw-note">Formatted for reading · copy and download use retained bytes</p>}
    {reading.problem && <p className="http-raw-note" role="status">{reading.problem}</p>}
    <pre className={`http-view-text http-raw-code${wrap ? "" : " http-raw-nowrap"}`} aria-label={formatted && !reading.problem ? "Formatted body JSON" : "Retained body bytes"} tabIndex={0}>{content.slice(0, limit)}</pre>
    {content.length > limit && <button className="cell-action" onClick={() => setLimit(it => it + 16_384)}>Show more · {content.length - limit} characters remaining</button>}
  </div>;
}

function HeaderSection({ headers }: { headers: readonly Header[] }) {
  const [limit, setLimit] = useState(50);
  const priority = ["content-type", "content-length", "location", "retry-after", "www-authenticate", "cache-control", "etag", "x-request-id", "content-encoding", "set-cookie"];
  const ordered = useMemo(() => headers.map((header, index) => ({ header, index, important: priority.includes(header.name.toLowerCase()) }))
    .sort((a, b) => Number(b.important) - Number(a.important) || a.index - b.index), [headers]);
  return <div>
      <div className="http-view-tools"><Copy label="Copy headers" text={() => headers.map(h => `${h.name}: ${h.value}`).join("\n")} /></div>
      <table className="http-view-headers"><tbody>{ordered.slice(0, limit).map(({ header, index, important }) => <tr key={index}><th scope="row" className={important ? "http-header-important" : undefined}>{header.name}</th><td>{header.value}</td></tr>)}</tbody></table>
      {headers.length > limit && <button className="cell-action" onClick={() => setLimit(it => it + 50)}>Show more · {headers.length - limit} headers remaining</button>}
      {headers.length === 0 && <p>No headers retained</p>}
  </div>;
}

function RawSection({ model }: { model: HttpModel }) {
  const representation = `${model.version.slice(0, 256)} ${statusLabel(model.status)}\n${model.headers.slice(0, 50).map(h => `${h.name.slice(0, 512)}: ${h.value.slice(0, 8192)}`).join("\n")}`;
  return <div className="http-raw">
    <section className="http-raw-section"><h3>Response representation</h3>
    <p className="http-raw-note">Reconstructed from retained metadata; not the original HTTP message.</p>
    <pre className="http-view-text http-raw-code" tabIndex={0}>{representation.slice(0, 16_384)}</pre>
    {(model.headers.length > 50 || representation.length > 16_384 || model.headers.some(h => h.name.length > 512 || h.value.length > 8192)) && <p>Representation preview is limited; all retained headers are available in Headers.</p>}
    </section><section className="http-raw-section"><h3>Retained body bytes <span className="mono-faint">· {model.original === undefined ? "not retained" : `${byteSize(model.original).toLocaleString("en-US")} B`}</span></h3>
    {model.original === undefined ? <p>Original body was not retained. Re-serialized data is not the original response.</p> : <OriginalContent key={model.original} encoded={model.original} />}
    </section>
  </div>;
}

function ResponseNotices({ model }: { model: HttpModel }) {
  const [fields, setFields] = useState(false);
  const validation = model.validation;
  const differs = validation && ["mismatch", "unreadable"].includes(validation.state);
  return <>
    {differs && <div className="http-view-notice"><span>{model.note}</span>{model.mode !== "preview" && <>
      {validation.issues.length > 0 && <button type="button" className="cell-action" aria-expanded={fields} onClick={() => setFields(it => !it)}>{fields ? "Hide fields" : "Show fields"}</button>}
      {fields && validation.issues.map((issue, at) => <p key={at}>{issue.path || "/"} · {issue.message} <span className="mono-faint">{issue.code}</span></p>)}
    </>}</div>}
    {(model.mode === "preview" ? (differs ? [] : model.notes.slice(0, 1)) : model.notes).map((note, at) => <p className="http-view-notice" key={at}>{note}</p>)}
  </>;
}

function HttpView({ model: input, children, renderChild }: ViewComponentProps) {
  const model = input as HttpModel;
  const compact = model.mode === "preview";
  const [selected, setSelected] = useState("body");
  const [visited, setVisited] = useState<ReadonlySet<string>>(new Set(["body"]));
  const id = useId();
  const sections = [{ id: "body", label: "Body" }, { id: "headers", label: `Headers · ${model.headers.length}` }, { id: "raw", label: "Raw" }];
  const choose = (name: string) => { setSelected(name); setVisited(was => new Set(was).add(name)); };
  const body = <div className="http-view-body" aria-label="Response body">{model.pending ? <p>Decoding body…</p> : model.empty ? <p>Empty body</p> : children.map(child => <div key={child.path}>{renderChild(child)}</div>)}</div>;
  return <section className={`http-value-view http-value-${model.mode}${model.oneLine ? " http-value-one-line" : ""}${model.narrow ? " http-value-narrow" : ""}`} aria-label="HTTP response">
    <header className="http-view-status"><strong><span className={`http-status-dot mono-${statusTone(model.status)}`} aria-hidden="true">●</span> {statusLabel(model.status)}</strong>
      {model.showMeta && <span className="http-view-meta">{model.compactMeta}</span>}
      {!compact && <span className="http-view-version">{model.version}</span>}
    </header>
    {compact ? <>{model.showNote && <ResponseNotices model={model} />}{!model.oneLine && children.length > 0 && body}</> : <>
      <div className="http-view-inspector">
        <ResponseNotices model={model} />
        {!model.narrow && <div className="http-view-tabs" role="tablist" aria-label="HTTP response sections" onKeyDown={event => {
          if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
          event.preventDefault(); event.stopPropagation();
          const current = sections.findIndex(section => section.id === selected);
          const next = event.key === "Home" ? 0 : event.key === "End" ? sections.length - 1 : (current + (event.key === "ArrowRight" ? 1 : sections.length - 1)) % sections.length;
          choose(sections[next]!.id);
          event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next]?.focus();
        }}>{sections.map(section => <button key={section.id} id={`${id}-${section.id}-tab`} type="button" role="tab" tabIndex={selected === section.id ? 0 : -1} aria-selected={selected === section.id} aria-controls={`${id}-${section.id}`} className="cell-action" onClick={() => choose(section.id)}>{section.label}</button>)}</div>}
        {sections.map(section => <section key={section.id} className="http-view-section">
          {model.narrow && <button type="button" className="cell-action http-view-section-toggle" aria-expanded={selected === section.id} aria-controls={`${id}-${section.id}`} onClick={() => choose(section.id)}>{selected === section.id ? "▾" : "▸"} {section.label}</button>}
          <div id={`${id}-${section.id}`} role={model.narrow ? "region" : "tabpanel"} aria-label={section.label} aria-labelledby={model.narrow ? undefined : `${id}-${section.id}-tab`} hidden={selected !== section.id}>
            {section.id === "body" ? body : visited.has(section.id) && (section.id === "headers" ? <HeaderSection headers={model.headers} /> : <RawSection model={model} />)}
          </div>
        </section>)}
      </div>
    </>}
  </section>;
}
