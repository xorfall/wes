import { describe, expect, it } from "vitest";
import { importSpecCommand, importSpecProblems, readSchemaProvenance } from "../api-library";
import { evidenceSummary, formatBytes, inputsSummary, nullableParts, responseShape, returnShape } from "./spec-table";

/* Synthetic names and addresses only. */

describe("what a closed operation row says", () => {
  const generated = "Api_orders_create_201_83eb5419";
  const types = { [generated]: { base: "Record", fields: {} }, Order: { base: "Record" }, Api_long_alias_without_a_base_9f00aa11: { fields: {} } };

  it("should_DemoteLongNamedTypesToTheirKnownShape_When_TheDescriptorKnowsIt", () => {
    expect(returnShape(generated, types)).toBe("Record");
    expect(returnShape(`List<${generated}>`, types)).toBe("List<Record>");
    expect(returnShape("Order", types)).toBe("Order");
  });

  it("should_KeepUnknownShapesUnknown_And_UnresolvedNamesExact", () => {
    expect(returnShape("Api_long_alias_without_a_base_9f00aa11", types)).toBe("unknown");
    expect(returnShape("Api_not_in_this_descriptor_0011223344", types)).toBe("Api_not_in_this_descriptor_0011223344");
    expect(responseShape({ status: 204, mediaType: null, type: null }, types)).toBe("empty body");
    expect(responseShape({ status: 200, mediaType: null, type: undefined }, types)).toBe("unknown shape");
  });

  it("should_MarkOnlyKnownOptionalInputs_And_NameTheBodyOnce", () => {
    expect(inputsSummary([
      { name: "id", location: "path", type: "Text", required: true },
      { name: "since", location: "query", type: "Text", required: false },
      { name: "cursor", location: "query", type: "Text" },
      { name: "payload", location: "body", type: "Order", required: true },
    ])).toBe("id, since?, cursor, body");
    expect(inputsSummary([])).toBe("");
  });
});

describe("nullability", () => {
  it("should_SplitOptionOnlyWhenLossless", () => {
    expect(nullableParts("Option<Text>")).toEqual({ inner: "Text", nullable: true });
    expect(nullableParts("Option<List<Option<Item>>>")).toEqual({ inner: "List<Option<Item>>", nullable: true });
    for (const kept of ["List<Option<Text>>", "Option<A>|Option<B>", "Option<A> > B<", "Text"]) expect(nullableParts(kept)).toEqual({ inner: kept, nullable: false });
  });
});

describe("evidence and size summaries", () => {
  const entry = (target: string, basis: string) => ({ target, basis, source: `sha256:${"ab".repeat(32)}`, pointer: "#/components/schemas/Item", lines: [], reason: "synthetic" });
  it("should_CountBasesUnderATarget_And_SayWhenNothingOrHistory", () => {
    const current = readSchemaProvenance({ provenance: { version: 1, status: "current", entries: [entry("#/types/Item", "documented"), entry("#/types/Item/fields/id", "inferred"), entry("#/types/Items", "documented")] } });
    expect(evidenceSummary(current, "#/types/Item")).toBe("documented 1 · inferred 1");
    expect(evidenceSummary(current, "#/types/Other")).toBe("no provenance");
    expect(evidenceSummary(undefined, "#/types/Item")).toBe("no provenance");
    const stale = readSchemaProvenance({ provenance: { version: 1, status: "stale", entries: [entry("#/operations/0/auth", "unknown")] } });
    expect(evidenceSummary(stale, "#/operations/0")).toBe("unknown 1 · historical");
  });
  it("should_ReadBytesForAHeading", () => {
    expect(formatBytes(1)).toBe("1 byte");
    expect(formatBytes(80)).toBe("80 bytes");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(48 * 1024)).toBe("48 KB");
    expect(formatBytes(3 * 1024 * 1024)).toBe("3.0 MB");
  });
});

describe("import input validation", () => {
  it("should_ReportEachFieldLocally_FromTheSameRulesAsTheCommand", () => {
    expect(importSpecProblems("posts", "https://synthetic.invalid")).toEqual({});
    expect(importSpecProblems("_internal", "http://synthetic.invalid/v1")).toEqual({});
    expect(importSpecProblems("posts-v2", "https://synthetic.invalid/?v=1")).toEqual({
      alias: "Use letters, digits and _; start with a letter or _.",
      endpoint: "Remove the query part; an endpoint is scheme, host and path only.",
    });
    expect(importSpecProblems("", "").alias).toBe("Type an alias.");
    expect(importSpecProblems("a", "").endpoint).toBe("Type the endpoint the snapshot will call.");
    expect(importSpecProblems("a", "synthetic.invalid").endpoint).toContain("full address");
    expect(importSpecProblems("a", "ftp://synthetic.invalid").endpoint).toContain("http");
    expect(importSpecProblems("a", "https://user:secret@synthetic.invalid").endpoint).toContain("credentials are set up in /env");
    expect(importSpecProblems("a", "https://synthetic.invalid/#top").endpoint).toContain("fragment");
    expect(importSpecProblems("1a", "https://synthetic.invalid").alias).toBeDefined();
  });

  it("should_KeepTheCommandExact_And_RefuseWhatTheFormReports", () => {
    expect(importSpecCommand("/tmp/a.json", "_internal", "https://synthetic.invalid/v1", false)).toBe(':import spec file:"/tmp/a.json" as:_internal endpoint:"https://synthetic.invalid/v1" replace:false');
    expect(() => importSpecCommand("/tmp/a.json", "posts-v2", "https://synthetic.invalid", false)).toThrow("Use letters, digits and _");
    expect(() => importSpecCommand("/tmp/a.json", "posts", "https://synthetic.invalid/?v=1", false)).toThrow("Remove the query part");
  });
});
