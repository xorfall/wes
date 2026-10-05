/** Synthetic data only; never a captured service response. */
import type { StoredValue } from "../protocol";
export function httpValue(body = '<html lang="tr"><body>Merhaba dünya</body></html>', headers: readonly { name: string; value: string }[] = []): StoredValue {
  return {
    type: { kind: "record", name: "HttpResponse", fields: [
      { name: "status", type: { kind: "primitive", name: "INT" } },
      { name: "version", type: { kind: "primitive", name: "TEXT" } },
      { name: "headers", type: { kind: "list", element: { kind: "record", name: "HttpHeader", fields: [
        { name: "name", type: { kind: "primitive", name: "TEXT" } }, { name: "value", type: { kind: "primitive", name: "TEXT" } },
      ] } } },
      { name: "body", type: { kind: "primitive", name: "BYTES" } },
    ] },
    provenance: {},
    data: { status: 200, version: "HTTP/1.1", headers: [
      { name: "Content-Type", value: "text/html; charset=utf-8" }, ...headers,
    ], body: Buffer.from(body).toString("base64") },
  };
}
