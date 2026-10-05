import { expect, it } from "vitest";
import type { TypeShape } from "../protocol";
import { width } from "../presentation/columns";
import { compactType, typeOutline, typeOutlineSegments } from "./result-type";
import { typeStructure } from "../presentation/type-shape";

const text: TypeShape = { kind: "primitive", name: "TEXT" };
const record: TypeShape = { kind: "record", name: "", fields: ["example", "status", "counts"].map(name => ({ name, type: text })) };

it("colors semantic fields and atoms while retaining the full structure printer's exact text", () => {
  const named: TypeShape = {kind:"record",name:"Int",fields:[{name:"List",type:{kind:"option",element:{kind:"meta",name:"Type"}}},{name:"counts",type:text}]};
  const shape: TypeShape = {kind:"list",element:named};
  const segments = typeOutlineSegments(shape);
  expect(segments.map(segment=>segment.text).join("")).toBe(typeStructure(shape));
  expect(segments.find(segment=>segment.text==="Int ")?.role).toBe("mono-ref");
  expect(segments.find(segment=>segment.text==="List")?.role).toBe("mono-param");
  expect(segments.find(segment=>segment.text==="Text")?.role).toBe("mono-meta");
  const iter: TypeShape = {kind:"iter",element:named,contract:"GET Text -> HttpResponse"};
  expect(typeOutlineSegments(iter).map(segment=>segment.text).join("")).toBe(typeStructure(iter));
});

it("preserves complete wrappers and counted omissions across available widths", () => {
  const shape: TypeShape = { kind: "option", element: { kind: "iter", element: { kind: "list", element: record } } };
  for (let columns = 1; columns <= 40; columns++) {
    const shown = compactType(shape, columns);
    if (shown !== "type") {
      expect(width(shown)).toBeLessThanOrEqual(columns);
      expect(shown.startsWith("Option<Iter<List<")).toBe(true);
      expect(shown.endsWith(">>>")).toBe(true);
    }
  }
  expect(compactType(record)).toBe("{ example, status, counts }");
});

it("keeps actual meta and iterator contracts, and opens known named record fields", () => {
  expect(compactType({ kind: "list", element: { kind: "meta", name: "Type" } })).toBe("List<Type>");
  const iter: TypeShape = { kind: "iter", element: record, contract: "GET Text -> HttpResponse" };
  expect(typeOutline(iter)).toBe("Iter<GET Text -> HttpResponse>");
  expect(compactType(iter)).toBe(typeOutline(iter));
  const named = { ...record, name: "LongServiceInventoryResponseWithExtendedSchemaName" };
  expect(compactType({ kind: "option", element: { kind: "list", element: named } }, 26)).toMatch(/^Option<List<.*…>>$/);
  expect(typeOutline(named)).toContain("example: Text");
});

it("bounds a pathological type without freezing or hiding the display limit", () => {
  const recursive: TypeShape = { kind: "record", name: "", fields: [] };
  (recursive.fields as { name: string; type: TypeShape }[]).push({ name: "self", type: recursive });
  expect(typeOutline(recursive)).toContain("type display limit");
  expect(compactType(recursive).length).toBeLessThanOrEqual(40);
});

it("avoids repeating the record count already supplied as a data fact",()=>{
  const many:TypeShape={kind:"record",name:"",fields:Array.from({length:30},(_,n)=>({name:`field${n}`,type:text}))};
  expect(compactType(many,15,true)).toBe("{ field0, +29 }");
  expect(compactType(many,13,true)).toBe("{…}");
  expect(compactType(many,13)).toBe("{ 30 fields }");
});

it("should_NameAManagementPlanByItsMetaType_When_TheHeaderDescribesIt", () => {
  // Arrange
  const shapes = [{ kind: "meta" as const, name: "ImportPlan" }, { kind: "meta" as const, name: "WorkspaceDeletePlan" }];
  // Act
  const names = shapes.map(shape => compactType(shape));
  // Assert
  expect(names).toEqual(["ImportPlan", "WorkspaceDeletePlan"]);
});
