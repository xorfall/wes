/**
 * A type the engine described as data.
 *
 * `:inspect`, the help catalogue and the type queries carry a value's type the way the wire types
 * every value — `{kind, name, element, fields}` — so a client recognises it by that shape alone,
 * never by the name of the field it sits in, and prints it with the one printer it uses for every
 * type: one line where it fits, the fields opened one per line where it is shown whole.
 */
import type { TypeShape } from "../protocol";

const KINDS = new Set(["meta", "primitive", "list", "option", "iter", "dataset", "record", "unknown"]);

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** The type this data describes, or undefined when it is not (strictly) one. */
export function typeShapeOf(data: unknown, depth = 0): TypeShape | undefined {
  if (!isObject(data) || typeof data.kind !== "string" || !KINDS.has(data.kind) || depth > 64) return undefined;
  switch (data.kind) {
    case "unknown": return Object.keys(data).length === 1 ? { kind: "unknown" } : undefined;
    case "meta": return typeof data.name === "string" && Object.keys(data).length === 2 ? { kind: "meta", name: data.name } : undefined;
    case "primitive": return typeof data.name === "string" && Object.keys(data).length === 2 ? { kind: "primitive", name: data.name } : undefined;
    case "list": case "option": case "iter": case "dataset": {
      const element = typeShapeOf(data.element, depth + 1);
      return element && Object.keys(data).length === 2 ? { kind: data.kind, element } : undefined;
    }
    case "record": {
      if (typeof data.name !== "string" || !Array.isArray(data.fields) || Object.keys(data).length !== 3) return undefined;
      const fields: { name: string; type: TypeShape }[] = [];
      for (const field of data.fields) {
        if (!isObject(field) || typeof field.name !== "string") return undefined;
        const type = typeShapeOf(field.type, depth + 1);
        if (!type) return undefined;
        fields.push({ name: field.name, type });
      }
      return { kind: "record", name: data.name, fields };
    }
    default: return undefined;
  }
}

const INDENT = "  ";

/** A type written out whole: every record opened field by field, nested shapes as the tree they are. */
export function typeStructure(shape: TypeShape | undefined, depth = 0): string {
  switch (shape?.kind) {
    case "meta": return shape.name;
    case "primitive": return shape.name.charAt(0) + shape.name.slice(1).toLowerCase();
    case "list": return `List<${typeStructure(shape.element, depth)}>`;
    case "option": return `Option<${typeStructure(shape.element, depth)}>`;
    case "iter": return `Iter<${shape.contract ?? typeStructure(shape.element, depth)}>`;
    case "dataset": return `Dataset<${typeStructure(shape.element, depth)}>`;
    case "record": {
      const head = shape.name === "" ? "" : `${shape.name} `;
      if (shape.fields.length === 0) return `${head}{}`;
      const fields = shape.fields.map((field) => `${INDENT.repeat(depth + 1)}${field.name}: ${typeStructure(field.type, depth + 1)}`);
      return `${head}{\n${fields.join("\n")}\n${INDENT.repeat(depth)}}`;
    }
    case "unknown":
    default: return "Unknown";
  }
}

/** A type on one line: `List<{ id: Int, name: Text }>`, named records by their name. */
export function typeLine(shape: TypeShape): string {
  switch (shape.kind) {
    case "meta": return shape.name;
    case "primitive": return shape.name.charAt(0) + shape.name.slice(1).toLowerCase();
    case "list": return `List<${typeLine(shape.element)}>`;
    case "option": return `Option<${typeLine(shape.element)}>`;
    case "iter": return `Iter<${shape.contract ?? typeLine(shape.element)}>`;
    case "dataset": return `Dataset<${typeLine(shape.element)}>`;
    case "record": return shape.name !== "" ? shape.name : shape.fields.length === 0 ? "{}" : `{ ${shape.fields.map((field) => `${field.name}: ${typeLine(field.type)}`).join(", ")} }`;
    default: return "Unknown";
  }
}
