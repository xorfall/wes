import type { TypeShape } from "../protocol";
import { ELEMENT, OPTION, field as fieldSegment, fieldMeta, membersLabel, type ValueMeta } from "../value-meta";
import { ellipsizeEnd, width } from "../presentation/columns";

const WRAPPERS = { list: "List", option: "Option", iter: "Iter", dataset: "Dataset" } as const;

/** Keep outer containers visible; reduce record detail before reducing names. */
export function compactType(shape: TypeShape, columns = 40, omitFieldCount = false): string {
  const budget = Math.max(1, Math.min(40, columns));
  let visits = 2000;
  const print = (type: TypeShape, stage: number, room: number, recordDepth = 0, depth = 0, count = 40): string => {
    if (--visits < 0 || depth > 64) return "…";
    switch (type.kind) {
      case "list": case "option": case "iter": case "dataset": {
        const name = WRAPPERS[type.kind];
        const inner = type.kind === "iter" && type.contract ? (stage ? ellipsizeEnd(type.contract, Math.max(1, room - name.length - 2)) : type.contract)
          : print(type.element, stage, room - name.length - 2, recordDepth, depth + 1, count);
        return `${name}<${inner}>`;
      }
      case "record": {
        if (type.name) return stage ? ellipsizeEnd(type.name, Math.max(1, room)) : type.name;
        if (!type.fields.length) return "{}";
        if (stage >= 5 || (stage >= 1 && recordDepth)) return "{…}";
        if (stage === 4) return `{ ${type.fields.length} fields }`;
        const taken = type.fields.slice(0, stage === 3 ? count : 40);
        const fields = taken.map(field => stage >= 2 ? field.name : `${field.name}: ${print(field.type, stage, room, recordDepth + 1, depth + 1, count)}`);
        const omitted = type.fields.length - taken.length;
        return `{ ${fields.join(", ")}${omitted ? `${fields.length ? ", " : ""}+${omitted}` : ""} }`;
      }
      case "primitive": return type.name.charAt(0) + type.name.slice(1).toLowerCase();
      case "meta": return stage ? ellipsizeEnd(type.name, Math.max(1, room)) : type.name;
      default: return "Unknown";
    }
  };
  for (const stage of omitFieldCount ? [0, 1, 2, 3, 5] : [0, 1, 2, 3, 4, 5]) {
    for (let count = 40; count >= (stage === 3 ? 1 : 40); count--) {
      visits = 2000;
      const text = print(shape, stage, budget, 0, 0, count);
      if (width(text, budget) <= budget) return text;
    }
  }
  return "type";
}

/** Color the bounded printer at its semantic source, never by guessing words in its output. A
 *  field whose contract was captured with the value also names its alias and declared members. */
export function typeOutlineSegments(shape: TypeShape, meta?: ValueMeta): import("./MonoLine").Segment[] {
  const result: import("./MonoLine").Segment[] = [];
  let visits = 2000;
  const add = (text: string, role: import("./MonoLine").MonoRole = "mono-dim") => { result.push({ text, role }); };
  const declared = (path: string) => {
    const described = fieldMeta(meta, path);
    if (!described) return;
    add(" · ", "mono-faint"); add(described.contract.name, "mono-ref");
    const members = membersLabel(described);
    if (members) { add(" · ", "mono-faint"); add(members, "mono-dim"); }
  };
  // `path` is the declaration path for captured metadata; undefined inside a Dataset, whose
  // element declarations are not captured with the value.
  const walk = (type: TypeShape, depth: number, path: string | undefined) => {
    if (--visits < 0 || depth > 64) { add("… [type display limit]"); return; }
    switch (type.kind) {
      case "list": case "option": case "iter": case "dataset":
        add(`${WRAPPERS[type.kind]}<`);
        if (type.kind === "iter" && type.contract !== undefined) add(type.contract, "mono-ref");
        else walk(type.element, depth, type.kind === "dataset" || path === undefined ? undefined : path + (type.kind === "option" ? OPTION : ELEMENT));
        add(">"); break;
      case "record":
        if (type.name) add(`${type.name} `, "mono-ref");
        if (!type.fields.length) { add("{}"); break; }
        add("{\n");
        for (let at = 0; at < type.fields.length; at++) {
          if (at) add("\n");
          add("  ".repeat(depth + 1));
          if (visits <= 0) { add("… [type display limit]"); break; }
          const field = type.fields[at]!;
          add(field.name, "mono-param"); add(": "); walk(field.type, depth + 1, path === undefined ? undefined : path + fieldSegment(field.name));
        }
        add(`\n${"  ".repeat(depth)}}`); break;
      case "primitive": add(type.name.charAt(0) + type.name.slice(1).toLowerCase(), "mono-meta"); if (path !== undefined) declared(path); break;
      case "meta": add(type.name, "mono-ref"); break;
      default: add("Unknown");
    }
  };
  walk(shape, 0, "");
  return result;
}

/** Bound hostile/deep type descriptions; any omitted structure is explicitly labelled. */
export function typeOutline(shape: TypeShape, meta?: ValueMeta): string {
  return typeOutlineSegments(shape, meta).map(segment => segment.text).join("");
}
