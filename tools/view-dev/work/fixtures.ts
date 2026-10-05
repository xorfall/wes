import type { StoredValue, TypeShape } from "../../../gui/src/protocol";

const text: TypeShape = { kind: "primitive", name: "TEXT" };
const int: TypeShape = { kind: "primitive", name: "INT" };
const bytes: TypeShape = { kind: "primitive", name: "BYTES" };
const headers: TypeShape = { kind: "list", element: { kind: "record", name: "HttpHeader", fields: [{ name: "name", type: text }, { name: "value", type: text }] } };
export function response(status: number, contentType: string, body: string | Uint8Array): StoredValue {
  const raw = typeof body === "string" ? new TextEncoder().encode(body) : body;
  const encoded = Array.from(raw, it => String.fromCharCode(it)).join("");
  return { type: { kind: "record", name: "HttpResponse", fields: [{ name: "status", type: int }, { name: "version", type: text }, { name: "headers", type: headers }, { name: "body", type: bytes }] }, provenance: {},
    data: { status, version: "HTTP/2.0", headers: [{ name: "content-type", value: contentType }, { name: "x-request-id", value: "synthetic-001" }, { name: "set-cookie", value: "demo=one" }, { name: "set-cookie", value: "demo=two" }], body: btoa(encoded) } };
}
const structured = response(200, "application/json", "");
export const fixtures: readonly { id: string; label: string; value: StoredValue }[] = [
  { id: "ok", label: "200 · JSON", value: response(200, "application/json", JSON.stringify({ item: "BIN-42", inventory: { available: 72, reserved: 6, location: "AISLE-C" } })) },
  { id: "rejected", label: "400 · JSON", value: response(400, "application/problem+json", JSON.stringify({ code: 400, message: "The requested storage bin is unavailable. Choose an active bin and submit a new request.", details: [{ field: "bin", reason: "Inactive bin" }] })) },
  { id: "html", label: "500 · HTML", value: response(500, "text/html", '<html><body><h1>Unavailable</h1><script>neverExecute()</script><img src="https://example.invalid/no-request"></body></html>') },
  { id: "empty", label: "204 · Empty", value: response(204, "", "") },
  { id: "binary", label: "200 · Binary", value: response(200, "image/png", new Uint8Array([137, 80, 78, 71, 0, 1, 2])) },
  { id: "invalid", label: "200 · Invalid JSON", value: response(200, "application/json", '{"message": incomplete') },
  { id: "structured", label: "Typed body · no original", value: { ...structured, type: { ...(structured.type as Extract<TypeShape, { kind: "record" }>), fields: (structured.type as Extract<TypeShape, { kind: "record" }>).fields.map(f => f.name === "body" ? { ...f, type: { kind: "unknown" } as TypeShape } : f) }, data: { ...(structured.data as object), body: { item: "BIN-42", available: 72 } } } },
  { id: "module", label: "Another view module", value: { type: { kind: "record", name: "Elapsed", fields: [{ name: "milliseconds", type: int }] }, provenance: {}, data: { milliseconds: 1840 } } },
];
