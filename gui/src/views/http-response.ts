import { budget } from "../limits/policy";
/** HTTP display decoding only. Stored bytes and execution requests are never rewritten. */
import type { StoredValue, TypeShape } from "../protocol";
import { readableData } from "../presentation/readable";

export interface Header { readonly name: string; readonly value: string }
export interface HttpResponse {
  readonly status: number;
  readonly version: string;
  readonly headers: readonly Header[];
  readonly body: string;
  readonly bytes: boolean;
}
const object = (data: unknown): data is Record<string, unknown> => typeof data === "object" && data !== null && !Array.isArray(data);
/** One structural predicate for automatic HTTP rendering and header facts. */
export function matchesHttp(type: TypeShape, data: unknown): boolean {
  const primitive = (shape: TypeShape | undefined, name: string) => shape?.kind === "primitive" && shape.name === name;
  const field = (shape: TypeShape, name: string) => shape.kind === "record" ? shape.fields?.find(it => it.name === name)?.type : undefined;
  const headers = field(type, "headers");
  return type.kind === "record" && primitive(field(type, "status"), "INT")
    && primitive(field(type, "version"), "TEXT") && headers?.kind === "list"
    && primitive(field(headers.element, "name"), "TEXT") && primitive(field(headers.element, "value"), "TEXT")
    && field(type, "body") !== undefined && object(data) && Number.isInteger(data.status)
    && Number(data.status) >= 100 && Number(data.status) <= 599 && typeof data.version === "string"
    && Object.hasOwn(data, "body") && Array.isArray(data.headers)
    && data.headers.every(it => object(it) && typeof it.name === "string" && typeof it.value === "string");
}

export function responseOf(value: Pick<StoredValue, "type" | "data">): HttpResponse | undefined {
  const { type, data } = value;
  if (!object(data) || typeof data.status !== "number" || typeof data.body !== "string" || !Array.isArray(data.headers)) return;
  const bodyType = type?.kind === "record" ? type.fields?.find(field => field.name === "body")?.type : undefined;
  if (!(type?.kind === "record" && (type.name === "HttpResponse" || (bodyType?.kind === "primitive" && bodyType.name === "BYTES")))) return;
  return { status: data.status, version: String(data.version ?? ""), body: data.body,
    bytes: !(bodyType?.kind === "primitive" && bodyType.name === "TEXT"),
    headers: data.headers.filter(object).map(header => ({ name: String(header.name ?? ""), value: String(header.value ?? "") })) };
}
export const headerValue = (response: HttpResponse, name: string) => response.headers
  .filter(header => header.name.toLowerCase() === name).map(header => header.value).join(", ");
export interface BodyDisplay { readonly text?: string; readonly encoding?: string; readonly problem?: string; readonly bytes: number }
export function max_decoded_body():number { return budget("ui.http.body.bytes"); }

export function textBody(bytes: Uint8Array, contentType: string): BodyDisplay {
  const base = { bytes: bytes.length };
  if (bytes.length === 0) return { ...base, text: "", encoding: "utf-8" };
  const bom = bytes[0] === 0xff && bytes[1] === 0xfe ? "utf-16le"
    : bytes[0] === 0xfe && bytes[1] === 0xff ? "utf-16be"
    : bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf ? "utf-8" : undefined;
  const declared = /charset\s*=\s*["']?([^\s;"',]+)/i.exec(contentType)?.[1];
  // An HTML/XML declaration is evidence, not a guess that arbitrary bytes are Latin-1.
  const head = /html|xml/i.test(contentType) ? new TextDecoder("ascii").decode(bytes.slice(0, 4096)) : "";
  const embedded = /<meta\b[^>]*charset\s*=\s*["']?([\w-]+)/i.exec(head)?.[1]
    ?? /<\?xml\b[^>]*encoding\s*=\s*["']([\w-]+)/i.exec(head)?.[1];
  const encoding = bom ?? declared ?? embedded ?? "utf-8";
  const mime = contentType.split(";", 1)[0]?.trim().toLowerCase() ?? "";
  if (mime && !/^text\/|json|xml|javascript|x-www-form-urlencoded/.test(mime) && !bom) {
    return { ...base, problem: `binary content (${mime})` };
  }
  try {
    const decoder = new TextDecoder(encoding, { fatal: true });
    const text = decoder.decode(bytes);
    if (/[\u0000-\u0008\u000b\u000e-\u001f\u007f]/.test(text)) return { ...base, problem: "body contains binary control bytes" };
    return { ...base, text, encoding: decoder.encoding };
  } catch { return { ...base, encoding, problem: `body cannot be decoded as ${encoding}` }; }
}

export async function decodeBody(response: HttpResponse, signal?: AbortSignal): Promise<BodyDisplay> {
  if (!response.bytes) return { text: response.body, encoding: "text", bytes: new TextEncoder().encode(response.body).length };
  let bytes: Uint8Array<ArrayBuffer>;
  try { bytes = Uint8Array.from(atob(response.body), character => character.charCodeAt(0)); }
  catch { return { bytes: 0, problem: "invalid base64 body" }; }
  const original = bytes.length;
  if (original > max_decoded_body()) return { bytes: original, problem: "body exceeds the 16 MiB text-decoding limit; encoded data is available" };
  try {
    const encodings = headerValue(response, "content-encoding").toLowerCase().split(",").map(item => item.trim()).filter(item => item && item !== "identity");
    for (const encoding of encodings.reverse()) {
      if (encoding !== "gzip" && encoding !== "deflate") return { bytes: original, problem: `unsupported content encoding: ${encoding}` };
      signal?.throwIfAborted();
      const reader = new Blob([bytes]).stream().pipeThrough(new DecompressionStream(encoding)).getReader();
      const cancel = () => { void reader.cancel().catch(() => {}); };
      signal?.addEventListener("abort", cancel, { once: true });
      const chunks: Uint8Array[] = [];
      let size = 0;
      try {
        while (true) {
          signal?.throwIfAborted();
          const part = await reader.read();
          if (part.done) break;
          size += part.value.length;
          if (size > max_decoded_body()) throw new Error("decompressed body exceeds the 16 MiB text-decoding limit");
          chunks.push(part.value);
        }
        bytes = new Uint8Array(size);
        let at = 0;
        for (const chunk of chunks) { bytes.set(chunk, at); at += chunk.length; }
      } finally {
        signal?.removeEventListener("abort", cancel);
        await reader.cancel().catch(() => {});
        reader.releaseLock();
      }
    }
    signal?.throwIfAborted();
    return textBody(bytes, headerValue(response, "content-type"));
  } catch (error) {
    if (signal?.aborted) throw error;
    return { bytes: original, problem: error instanceof Error && error.message ? error.message : "body decompression failed" };
  }
}

export async function readableResult(value: StoredValue, signal?: AbortSignal): Promise<unknown> {
  const response = responseOf(value);
  const type: TypeShape = value.type ?? { kind: "unknown" };
  if (!response) return readableData(type, value.data);
  const body = await decodeBody(response, signal);
  return { ...(value.data as Record<string, unknown>),
    body: body.text ?? { bytes: body.bytes, note: body.problem } };
}
