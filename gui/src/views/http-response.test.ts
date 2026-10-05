import { describe, expect, it } from "vitest";
import { deflateSync, gzipSync } from "node:zlib";
import { httpValue } from "../testing/http-response";
import { decodeBody, headerValue, max_decoded_body, readableResult, responseOf, type HttpResponse } from "./http-response";
const response = (bytes: Uint8Array, type = "text/plain; charset=utf-8", encoding?: string): HttpResponse => ({
  status: 200, version: "HTTP/1.1", bytes: true, body: Buffer.from(bytes).toString("base64"),
  headers: [{ name: "Content-Type", value: type }, ...(encoding ? [{ name: "Content-Encoding", value: encoding }] : [])],
});
describe("HTTP body inspection", () => {
  it("reads typed response bytes and retains duplicate headers without changing stored data", async () => {
    const value = httpValue(undefined, [{ name: "x-test", value: "first" }, { name: "X-Test", value: "second" }]);
    const original = JSON.stringify(value);
    const parsed = responseOf(value)!;
    expect(parsed.headers).toHaveLength(3);
    expect(headerValue(parsed, "x-test")).toBe("first, second");
    expect(await decodeBody(parsed)).toMatchObject({ text: '<html lang="tr"><body>Merhaba dünya</body></html>', encoding: "utf-8" });
    expect(await readableResult(value)).toMatchObject({ body: '<html lang="tr"><body>Merhaba dünya</body></html>' });
    expect(JSON.stringify(value)).toBe(original);
  });
  it.each([
    [Buffer.from([0xdd, 0xfe]), 'text/plain; CHARSET="windows-1254"', "İş"],
    [Buffer.from([0x63, 0x61, 0x66, 0xe9]), "text/plain; charset=iso-8859-1", "café"],
    [Buffer.from([0xff, 0xfe, 0x49, 0x01]), "text/plain; charset=utf-8", "ŉ"],
    [Buffer.from([0xfe, 0xff, 0x01, 0x49]), "text/plain", "ŉ"],
    [Buffer.from('<meta charset="windows-1252">caf\xe9', "latin1"), "text/html", '<meta charset="windows-1252">café'],
    [Buffer.from('<?xml version="1.0" encoding="iso-8859-1"?><p>caf\xe9</p>', "latin1"), "application/xml", '<?xml version="1.0" encoding="iso-8859-1"?><p>café</p>'],
  ])("honors declared charset, embedded encoding and BOM: %s", async (bytes, type, text) => {
    expect(await decodeBody(response(bytes, type))).toMatchObject({ text });
  });
  it.each(["gzip", "deflate", "gzip, deflate"])("decodes %s in reverse application order", async encoding => {
    let bytes = Buffer.from("sentetik gövde\n".repeat(100));
    if (encoding.includes("gzip")) bytes = gzipSync(bytes);
    if (encoding.includes("deflate")) bytes = deflateSync(bytes);
    expect(await decodeBody(response(bytes, "text/plain", encoding))).toMatchObject({ text: "sentetik gövde\n".repeat(100) });
  });
  it("does not guess a charset, render binary as text or silently swallow invalid data", async () => {
    const binary = await decodeBody(response(Buffer.from("not an image"), "image/png"));
    expect(binary.text).toBeUndefined(); expect(binary.problem).toContain("binary content");
    expect(await decodeBody(response(Buffer.from([0xe9])))).toMatchObject({ problem: "body cannot be decoded as utf-8" });
    expect(await decodeBody(response(Buffer.from([0, 1])))).toMatchObject({ problem: "body contains binary control bytes" });
    expect(await decodeBody({ ...response(Buffer.alloc(0)), body: "@@@" })).toMatchObject({ problem: "invalid base64 body" });
    expect(await decodeBody(response(Buffer.from("x"), "text/plain", "br"))).toMatchObject({ problem: "unsupported content encoding: br" });
    expect((await decodeBody(response(Buffer.from("bad gzip"), "text/plain", "gzip"))).problem).toBeTruthy();
    expect(await decodeBody(response(Buffer.alloc(0)))).toMatchObject({ text: "", bytes: 0 });
  });
  it("bounds decompression and cancels an abandoned inspection", async () => {
    const bytes = gzipSync(Buffer.alloc(max_decoded_body() + 1, 65));
    expect((await decodeBody(response(bytes, "text/plain", "gzip"))).problem).toContain("16 MiB");
    const controller = new AbortController(); controller.abort();
    await expect(decodeBody(response(bytes, "text/plain", "gzip"), controller.signal)).rejects.toThrow();
  });
  it("preserves plain text that happens to resemble base64 and only decodes typed bytes", async () => {
    const data = "aGVsbG8=";
    expect(await readableResult({ type: { kind: "primitive", name: "TEXT" }, provenance: {}, data })).toBe(data);
    expect(await readableResult({ type: { kind: "primitive", name: "BYTES" }, provenance: {}, data })).toBe("hello");
    const malformed = httpValue();
    (malformed.data as { body: string }).body = "@@@";
    expect(await readableResult(malformed)).toMatchObject({ body: { bytes: 0, note: "invalid base64 body" } });
    expect(responseOf({ ...malformed, type: { kind: "unknown" } })).toBeUndefined();
  });
});
