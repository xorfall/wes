import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { describe, expect, it } from "vitest";
import { authProvenanceLabel, isCanonicalPointer, operationTarget, provenanceEntriesFor, readSchemaProvenance, type SchemaProvenance } from "../api-library";
import { ProvenanceDetail, ProvenanceNotice, SchemaFieldsTable, optionalityLabel, schemaRows } from "./SchemaProvenance";

/* Synthetic descriptors only: invented types, pointers and reasons, never a user's. */

const DIGEST = "ab".repeat(32);
const entry = (target: string, basis: string, over: Record<string, unknown> = {}) => ({ target, basis, source: `sha256:${DIGEST}`, pointer: "#/components/schemas/Item", lines: [{ start: 12, end: 14 }], reason: `${basis} reason`, ...over });
const source = (entries: unknown[], over: Record<string, unknown> = {}) => ({ sha256: DIGEST, format: "openapi", location: "https://docs.example.test/openapi.json", provenance: { version: 1, status: "current", entries }, ...over });
const stale = (entries: unknown[]) => source(entries, { provenance: { version: 1, status: "stale", entries } });
const types = { Item: { base: "Record", fields: { id: { type: "Text", optional: false }, note: { type: "Option<Text>", optional: true }, odd: "Int" }, minItems: 1 }, Tags: { base: "List<Text>" }, Broken: 7 };
const shown = (tree: ReactTestRenderer) => JSON.stringify(tree.toJSON());

describe("reading source.provenance", () => {
  it("should_ReadVersionOneEntries_When_EveryFieldIsWhatTheContractSays", () => {
    // Arrange / Act
    const read = readSchemaProvenance(source([entry("#/types/Item/fields/id/type", "documented", { lines: [] }), entry("#/operations/0/responses/2XX~0", "example", { pointer: "#/paths/~1items/get/responses/2XX~0" })]));
    // Assert
    expect(read?.status).toBe("current");
    expect(read?.location).toBe("https://docs.example.test/openapi.json");
    expect(read?.entries).toEqual([
      { target: "#/types/Item/fields/id/type", basis: "documented", source: `sha256:${DIGEST}`, pointer: "#/components/schemas/Item", lines: [], reason: "documented reason" },
      { target: "#/operations/0/responses/2XX~0", basis: "example", source: `sha256:${DIGEST}`, pointer: "#/paths/~1items/get/responses/2XX~0", lines: [{ start: 12, end: 14 }], reason: "example reason" },
    ]);
  });

  it("should_ReadAsNoProvenance_When_TheBlockIsLegacyMalformedOrAnotherVersion", () => {
    for (const bad of [undefined, null, "review", 4, [], {}, { review: { claims: [] } }, { provenance: null }, { provenance: { version: 2, status: "current", entries: [] } }, { provenance: { version: 1, status: "draft", entries: [] } }, { provenance: { version: 1, status: "current", entries: {} } }]) {
      expect(readSchemaProvenance(bad)).toBeUndefined();
    }
  });

  it("should_DropAMalformedRecordWhole_So_ItNeverSurfacesAsADocumentedClaim", () => {
    // Arrange: every record below is documented but broken in exactly one way
    const broken = [
      5, entry("", "documented"), entry("#", "documented"), entry("#/a/~2", "documented"), entry("/types/Item", "documented"), entry("#/types/Item//", "documented", { target: "#/types/Item/~" }),
      entry("#/types/Item", "proven"), entry("#/types/Item", "documented", { pointer: "components/schemas/Item" }), entry("#/types/Item", "documented", { pointer: "" }),
      entry("#/types/Item", "documented", { source: "" }), entry("#/types/Item", "documented", { source: "  " }), entry("#/types/Item", "documented", { source: undefined }),
      entry("#/types/Item", "documented", { reason: "" }), entry("#/types/Item", "documented", { reason: 9 }),
      entry("#/types/Item", "documented", { lines: undefined }), entry("#/types/Item", "documented", { lines: {} }), entry("#/types/Item", "documented", { lines: [{ start: 9, end: 2 }] }),
      entry("#/types/Item", "documented", { lines: [{ start: 0, end: 2 }] }), entry("#/types/Item", "documented", { lines: [{ start: 1.5, end: 2 }] }), entry("#/types/Item", "documented", { lines: [{ start: 3, end: 3 }, "x"] }),
    ];
    // Act
    const read = readSchemaProvenance(source([...broken, entry("#/types/Item", "unknown")]));
    // Assert
    expect(read?.entries.map(e => e.basis)).toEqual(["unknown"]);
    expect(authProvenanceLabel([], readSchemaProvenance(source([entry("#/operations/0/auth", "documented", { lines: [{ start: 2, end: 1 }] })])), 0)).toBe("no credentials attached · no provenance");
  });

  it("should_AcceptOnlyCanonicalFragmentPointers", () => {
    for (const ok of ["#/types/Item", "#/operations/0/auth", "#/paths/~1items/get", "#/a/~0~1/b", "#/x/"]) expect(isCanonicalPointer(ok)).toBe(true);
    for (const bad of ["#", "", "/types", "types", "#types", "#/a/~", "#/a/~2", "#/a/~x", 3, null]) expect(isCanonicalPointer(bad)).toBe(false);
    expect(operationTarget.response(3, "2XX/~")).toBe("#/operations/3/responses/2XX~1~0");
    expect(operationTarget.parameter(3, 1)).toBe("#/operations/3/parameters/1");
  });

  it("should_GatherATypeOwnFactsButNotItsFields_When_LookingUpATypeTarget", () => {
    // Arrange
    const read = readSchemaProvenance(source([entry("#/types/Item", "documented"), entry("#/types/Item/minItems", "example"), entry("#/types/Item/fields/id/type", "documented"), entry("#/types/Items", "inferred"), entry("#/operations/1/parameters/1/required", "unknown"), entry("#/operations/1/parameters/10", "documented")]));
    // Act / Assert
    expect(provenanceEntriesFor(read, "#/types/Item").map(e => e.target)).toEqual(["#/types/Item", "#/types/Item/minItems"]);
    expect(provenanceEntriesFor(read, "#/types/Item/fields/id").map(e => e.target)).toEqual(["#/types/Item/fields/id/type"]);
    expect(provenanceEntriesFor(read, "#/operations/1/parameters/1").map(e => e.target)).toEqual(["#/operations/1/parameters/1/required"]);
    expect(provenanceEntriesFor(undefined, "#/types/Item")).toEqual([]);
  });
});

describe("the auth column", () => {
  const scheme = [{ scheme: "bearer", secret: "api_token" }];
  it("should_NeverAdvertiseDocumentedNoAuth_When_NoEntryCoversTheOperation", () => {
    expect(authProvenanceLabel([], undefined, 0)).toBe("no credentials attached · no provenance");
    expect(authProvenanceLabel([], readSchemaProvenance(source([entry("#/operations/1/auth", "documented")])), 0)).toBe("no credentials attached · no provenance");
    expect(authProvenanceLabel(scheme, undefined, 0)).toBe("api_token · no provenance");
  });
  it("should_SayUnknownNotPublic_When_TheBasisIsUnknown", () => {
    const read = readSchemaProvenance(source([entry("#/operations/2/auth", "unknown"), entry("#/operations/2/auth", "documented")]));
    expect(authProvenanceLabel([], read, 2)).toBe("unknown · no credentials attached");
    expect(authProvenanceLabel(scheme, read, 2)).toBe("api_token · unknown");
  });
  it("should_SayDocumentedNoAuthOrTheBasis_When_Current", () => {
    expect(authProvenanceLabel([], readSchemaProvenance(source([entry("#/operations/0/auth", "documented")])), 0)).toBe("documented · no auth");
    expect(authProvenanceLabel([], readSchemaProvenance(source([entry("#/operations/0/auth", "inferred")])), 0)).toBe("inferred · no credentials attached");
  });
  it("should_KeepConfiguredMappingsVisible_And_MarkEveryBasisHistorical_When_TheBlockIsStale", () => {
    expect(authProvenanceLabel(scheme, readSchemaProvenance(stale([entry("#/operations/0/auth", "documented")])), 0)).toBe("api_token · documented · stale");
    expect(authProvenanceLabel([], readSchemaProvenance(stale([entry("#/operations/0/auth", "documented")])), 0)).toBe("documented · no auth · stale");
    expect(authProvenanceLabel([], readSchemaProvenance(stale([entry("#/operations/0/auth", "unknown")])), 0)).toBe("unknown · stale · no credentials attached");
    expect(authProvenanceLabel([], readSchemaProvenance(stale([])), 0)).toBe("no credentials attached · no provenance · stale");
  });
});

describe("schema rows", () => {
  it("should_ListEachTypeThenItsFields_With_UnknownShapesAndOptionalityKeptUnknown", () => {
    // Act
    const rows = schemaRows(types);
    // Assert
    expect(rows.map(r => r.target)).toEqual(["#/types/Item", "#/types/Item/fields/id", "#/types/Item/fields/note", "#/types/Item/fields/odd", "#/types/Tags", "#/types/Broken"]);
    expect(rows[0]).toEqual({ target: "#/types/Item", type: "Item", expression: "Record · minItems 1" });
    expect(rows[1]).toMatchObject({ field: "id", expression: "Text", optionality: "required" });
    expect(rows[2]).toMatchObject({ field: "note", expression: "Option<Text>", optionality: "optional" });
    expect(rows[3]).toMatchObject({ field: "odd", expression: "Int", optionality: "unknown" });
    expect(rows[5]).toEqual({ target: "#/types/Broken", type: "Broken", expression: "unknown shape" });
    expect(schemaRows({ "a/b~c": { base: "Text" } })[0]!.target).toBe("#/types/a~1b~0c");
  });

  it("should_KeepTheRuntimeFlag_And_AddDocumentationUnknownOnlyFromACurrentRecord", () => {
    const [head, id, note, odd] = schemaRows(types);
    const unknownNote = [entry("#/types/Item/fields/note/optional", "unknown"), entry("#/types/Item/fields/odd/optional", "unknown")];
    // current: the flag stays, the documentation's silence is added beside it
    expect(optionalityLabel(note!, readSchemaProvenance(source(unknownNote)))).toBe("optional · documentation unknown");
    expect(optionalityLabel(id!, readSchemaProvenance(source(unknownNote)))).toBe("required");
    expect(optionalityLabel(odd!, readSchemaProvenance(source(unknownNote)))).toBe("unknown");
    expect(optionalityLabel(head!, readSchemaProvenance(source(unknownNote)))).toBe("");
    // stale: history says nothing about the current flag, in either direction
    expect(optionalityLabel(note!, readSchemaProvenance(stale(unknownNote)))).toBe("optional");
    expect(optionalityLabel(note!, readSchemaProvenance(stale([entry("#/types/Item/fields/note/optional", "documented")])))).toBe("optional");
    expect(optionalityLabel(note!, undefined)).toBe("optional");
  });
});

describe("the rows and the detail as a screen", () => {
  const entries = [
    entry("#/types/Item/fields/id/type", "documented", { reason: "<script>not markup</script> https://not-a-link.example.test" }),
    entry("#/types/Item/fields/note/optional", "unknown", { reason: "requiredness is not stated", lines: [] }),
  ];
  const read = readSchemaProvenance(source(entries)) as SchemaProvenance;

  it("should_PickATargetFromItsRow_And_ShowItsBasisColumn", () => {
    // Arrange
    let tree!: ReactTestRenderer;
    const picked: string[] = [];
    act(() => { tree = create(<SchemaFieldsTable types={types} provenance={read} onSelect={t => picked.push(t)} />); });
    // Collapsed types retain their identity; fields and evidence appear on demand.
    expect(tree.root.findAllByProps({ "aria-label": "Evidence for id" })).toHaveLength(0);
    act(() => tree.root.findByProps({ "aria-label": "Expand type Item" }).props.onClick());
    const textOf = (node: ReactTestInstance | string): string => typeof node === "string" ? node : node.children.map(textOf).join("");
    const rowText = (field: string) => textOf(tree.root.findByProps({ "aria-label": `Evidence for ${field}` }).parent!.parent!.parent!);
    // The type's folded evidence counts its bases; a field only adds a line when its basis is a caution.
    expect(shown(tree)).toContain("documented 1 · unknown 1");
    expect(shown(tree)).toContain("optional · documentation unknown");
    expect(rowText("note")).toContain("evidence unknown");
    expect(rowText("id")).not.toContain("evidence");
    expect(rowText("odd")).not.toContain("evidence");
    expect(shown(tree)).not.toContain("no provenance");
    act(() => tree.root.findByProps({ "aria-label": "Evidence for id" }).props.onClick());
    expect(picked).toEqual(["#/types/Item/fields/id"]);
    act(() => tree.update(<SchemaFieldsTable types={types} provenance={readSchemaProvenance(stale(entries))} onSelect={() => {}} />));
    expect(shown(tree)).toContain("documented · stale");
    expect(shown(tree)).toContain("unknown · stale");
    expect(shown(tree)).not.toContain("optional · documentation unknown");
    act(() => tree.unmount());
  });

  it("should_RenderEveryFactAsText_When_ATargetHasEntries_And_SayNothingIsImpliedOtherwise", () => {
    // Arrange / Act
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<ProvenanceDetail target="#/types/Item/fields/id" provenance={read} />); });
    // Assert
    expect(tree.root.findAllByType("a")).toHaveLength(0);
    expect(tree.root.findAllByType("script")).toHaveLength(0);
    expect(tree.root.findByProps({ className: "mono-ink spec-provenance-reason" }).children.join("")).toBe("<script>not markup</script> https://not-a-link.example.test");
    for (const fact of ["documented", "#/types/Item/fields/id/type", `sha256:${DIGEST}`, "https://docs.example.test/openapi.json", "#/components/schemas/Item", "12–14", "does not prove the interpretation"]) expect(shown(tree)).toContain(fact);
    expect(shown(tree)).not.toContain("stale");
    // Act
    act(() => tree.update(<ProvenanceDetail target="#/types/Tags" provenance={read} />));
    // Assert
    expect(shown(tree)).toContain("no provenance recorded for this target");
    // Act: stale record
    act(() => tree.update(<ProvenanceDetail target="#/types/Item/fields/id" provenance={readSchemaProvenance(stale(entries))} />));
    // Assert: the head labels the basis as history and the target line says what stale means
    expect(tree.root.findByProps({ className: "spec-provenance-head" }).children.map(c => typeof c === "string" ? c : c.children.join("")).join("")).toBe("documented · stale · #/types/Item/fields/id/type");
    expect(shown(tree)).toContain("does not describe the current descriptor");
    act(() => tree.unmount());
  });

  it("should_SayWhichRevisionTheMetadataDescribes", () => {
    const notice = (props: { provenance: SchemaProvenance | undefined; dirty: boolean }) => {
      let tree!: ReactTestRenderer;
      act(() => { tree = create(<ProvenanceNotice {...props} revision={"c".repeat(64)} />); });
      const text = tree.root.findByType("p");
      const result = { text: text.children.join(""), className: text.props.className as string };
      act(() => tree.unmount());
      return result;
    };
    expect(notice({ provenance: read, dirty: true })).toEqual({ text: `Provenance describes the saved revision ${"c".repeat(12)}, not the unsaved edits in the source tab.`, className: "mono-warn spec-provenance-notice" });
    expect(notice({ provenance: undefined, dirty: false }).text).toContain("No provenance metadata in this revision");
    expect(notice({ provenance: { ...read, status: "stale" }, dirty: false })).toMatchObject({ className: "mono-warn spec-provenance-notice" });
    expect(notice({ provenance: { ...read, status: "stale" }, dirty: false }).text).toContain("stale");
    expect(notice({ provenance: read, dirty: false }).text).toContain("complete supplied document");
  });
});
