import { describe, expect, it } from "vitest";
import { ExactNumber } from "./exact-json";
import { decodeMeta, declaredTone, field, fieldMeta, membersLabel, withValidMeta, ELEMENT, OPTION } from "./value-meta";
import { typeOutline } from "./surface/result-type";
import type { TypeShape } from "./protocol";

const digest = (n: number) => `sha256:${n.toString(16).padStart(64, "0")}`;
const status = { contract: { name: "ServiceStatus", digest: digest(2) }, kind: "text", source: "validated", members: ["ready", "failed", "unknown"], total: 3, complete: true, tones: { ready: "ok", failed: "bad", unknown: "dim" } };
const wire = { version: 1, contract: { name: "List<ServiceRow>", digest: digest(1) }, truncated: false, fields: { [`${ELEMENT}${field("status")}`]: status } };

describe("value metadata", () => {
  it("decodes declared members and tones at declaration paths", () => {
    const meta = decodeMeta(wire)!;
    expect(fieldMeta(meta, "/e/f:status")?.members).toEqual(["ready", "failed", "unknown"]);
    expect(declaredTone(fieldMeta(meta, "/e/f:status"), "failed")).toBe("bad");
    expect(declaredTone(fieldMeta(meta, "/e/f:status"), "constructor")).toBeUndefined();
    expect(fieldMeta(meta, "toString")).toBeUndefined();
  });

  it("escapes field names into path segments", () => {
    expect(field("a/b~c")).toBe("/f:a~1b~0c");
    expect(decodeMeta({ ...wire, fields: { [`${field("a/b")}${OPTION}`]: status } })).toBeDefined();
  });

  it("matches numbers and booleans by exact spelling", () => {
    const code = { ...status, kind: "int", members: ["200", "90071992547409930"], total: 2, tones: { "90071992547409930": "warn", true: "ok" } };
    const meta = decodeMeta({ ...wire, fields: { "/f:code": code } })!;
    expect(declaredTone(fieldMeta(meta, "/f:code"), new ExactNumber("90071992547409930"))).toBe("warn");
    expect(declaredTone(fieldMeta(meta, "/f:code"), true)).toBe("ok");
    expect(declaredTone(fieldMeta(meta, "/f:code"), 200)).toBeUndefined();
    expect(declaredTone(fieldMeta(meta, "/f:code"), new ExactNumber("90071992547409930.0"))).toBeUndefined();
  });

  it("matches a Decimal member by exact value, as the contract validates it", () => {
    const rate = { ...status, kind: "decimal", members: ["1.5", "0.10000000000000000001"], total: 2, tones: { "1.5": "warn", "0.10000000000000000001": "bad" } };
    const meta = fieldMeta(decodeMeta({ ...wire, fields: { "/f:rate": rate } })!, "/f:rate");
    expect(declaredTone(meta, new ExactNumber("1.50"))).toBe("warn");
    expect(declaredTone(meta, 1.5)).toBe("warn");
    expect(declaredTone(meta, new ExactNumber("0.100000000000000000010"))).toBe("bad");
    expect(declaredTone(meta, new ExactNumber("0.1"))).toBeUndefined();
    expect(declaredTone({ ...meta!, kind: "text" }, new ExactNumber("1.50"))).toBeUndefined();
  });

  const decimal = (tones: Record<string, string>) =>
    fieldMeta(decodeMeta({ ...wire, fields: { "/f:rate": { ...status, kind: "decimal", members: Object.keys(tones), total: Object.keys(tones).length, tones } } })!, "/f:rate");

  it("draws a Decimal neutral when equal members declare different tones, whatever its spelling", () => {
    const meta = decimal({ "1.5": "warn", "1.50": "bad", "2": "ok" });
    expect(declaredTone(meta, new ExactNumber("1.5"))).toBeUndefined();
    expect(declaredTone(meta, new ExactNumber("1.50"))).toBeUndefined();
    expect(declaredTone(meta, 1.5)).toBeUndefined();
    expect(declaredTone(meta, new ExactNumber("2.0"))).toBe("ok");
  });

  it("keeps a Decimal tone when every equal member declares it, including a value read as a safe number", () => {
    const meta = decimal({ "1.5": "warn", "1.50": "warn" });
    expect(declaredTone(meta, 1.5)).toBe("warn");
    expect(declaredTone(meta, new ExactNumber("1.500"))).toBe("warn");
    expect(declaredTone(meta, new ExactNumber("15e-1"))).toBe("warn");
  });

  it("matches precise and scientific Decimal members exactly, never through a float", () => {
    const meta = decimal({ "1.23000000000000000001": "meta", "1e400": "bad" });
    expect(declaredTone(meta, new ExactNumber("123000000000000000001e-20"))).toBe("meta");
    expect(declaredTone(meta, new ExactNumber("1.23"))).toBeUndefined();
    expect(declaredTone(meta, new ExactNumber("10E+399"))).toBe("bad");
    expect(declaredTone(meta, new ExactNumber("1e401"))).toBeUndefined();
    expect(declaredTone(decimal({ "1.5": "warn", [`1e${"9".repeat(4097)}`]: "ok" }), 1.5)).toBeUndefined();
  });

  it("refuses malformed or oversized metadata as a whole, leaving the value unknown", () => {
    for (const bad of [
      { ...wire, version: 2 },
      { ...wire, contract: { name: "X", digest: "md5:1" } },
      { ...wire, fields: { "/x": status } },
      { ...wire, fields: { "/f:a~2": status } },
      { ...wire, fields: { "": { ...status, kind: "date" } } },
      { ...wire, fields: { "": { ...status, tones: { ready: "#ff0000" } } } },
      { ...wire, fields: { "": { ...status, members: ["a", "a"], total: 2 } } },
      { ...wire, fields: { "": { ...status, total: 2 } } },
      { ...wire, fields: { "": { ...status, complete: false } } },
      { ...wire, fields: Object.fromEntries(Array.from({ length: 129 }, (_, i) => [field(`f${i}`), status])) },
      { ...wire, fields: { [Array.from({ length: 65 }, () => "/o").join("")]: status } },
    ]) expect(decodeMeta(bad)).toBeUndefined();
    const read = withValidMeta({ type: { kind: "primitive", name: "TEXT" } as TypeShape, provenance: {}, data: "x", meta: { version: 9 } });
    expect("meta" in read).toBe(false);
    expect(read.data).toBe("x");
  });

  it("says when a preview holds only some of the declared members", () => {
    const many = { ...status, members: ["a", "b", "c", "d", "e", "f", "g"], total: 200, complete: false, tones: undefined };
    expect(membersLabel(decodeMeta({ ...wire, fields: { "": many } })!.fields[""]!)).toBe("a | b | c | d | e | f | … (6 of 200 declared)");
  });

  it("keeps each member on one bounded, unambiguous piece of the label", () => {
    const odd = { ...status, members: ["two\nlines", "x | y", "", "z".repeat(100)], total: 4, tones: undefined };
    const label = membersLabel(decodeMeta({ ...wire, fields: { "": odd } })!.fields[""]!)!;
    expect(label).toBe(`"two\\nlines" | "x | y" | "" | ${"z".repeat(31)}…`);
    expect(label).not.toContain("\n");
  });

  it("names a field's alias and members in the type outline, and nothing without metadata", () => {
    const shape: TypeShape = { kind: "list", element: { kind: "record", name: "ServiceRow", fields: [
      { name: "id", type: { kind: "primitive", name: "TEXT" } },
      { name: "status", type: { kind: "primitive", name: "TEXT" } },
    ] } };
    const text = typeOutline(shape, decodeMeta(wire));
    expect(text).toContain("status: Text · ServiceStatus · ready | failed | unknown");
    expect(text).toContain("id: Text\n");
    expect(typeOutline(shape)).not.toContain("ServiceStatus");
  });
});
