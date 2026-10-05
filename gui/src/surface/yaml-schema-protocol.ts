/** Versioned editor metadata published by the engine; no declaration grammar lives here. */
export interface SchemaCondition {
  readonly field: string;
  readonly values?: readonly string[];
  readonly prefix?: string;
  readonly allowed: readonly string[];
  readonly required?: readonly string[];
}
interface Metadata { readonly hint?: string; readonly constraints?: readonly string[]; }
export type SchemaNode = Metadata & (
  | { readonly kind: "any" }
  | { readonly kind: "scalar"; readonly type: string; readonly choices?: readonly string[] }
  | { readonly kind: "object"; readonly fields: Readonly<Record<string, { readonly schema: string; readonly required: boolean }>>; readonly conditions?: readonly SchemaCondition[]; readonly exclusive?: readonly { readonly fields: readonly string[]; readonly min: number; readonly max: number }[] }
  | { readonly kind: "map"; readonly values: string }
  | { readonly kind: "list"; readonly items: string }
  | { readonly kind: "union"; readonly variants: readonly string[] }
  | { readonly kind: "discriminated"; readonly field: string; readonly variants: Readonly<Record<string, string>> }
);
export interface YamlSchema {
  readonly language: "yaml";
  readonly version: 1;
  readonly roots: Readonly<Record<"env" | "types", string>>;
  readonly definitions: Readonly<Record<string, SchemaNode>>;
  readonly typeConstructors: readonly { readonly name: string; readonly parameters: readonly string[] }[];
  readonly constraints?: Readonly<Record<string, { readonly description: string; readonly validation: "semantic" }>>;
}

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
const string = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= 4096;
const choices = (value: unknown): value is string[] => Array.isArray(value) && value.length <= 4096 && value.every(item => typeof item === "string" && item.length <= 4096);
const strings = (value: unknown): value is string[] => Array.isArray(value) && value.length <= 4096 && value.every(string);
const valid = (condition: unknown): void => { if (!condition) throw new Error("Invalid YAML completion schema"); };

/** Reject unsupported/corrupt graphs before the tolerant source reader sees them. */
export function parseYamlSchema(value: unknown): YamlSchema {
  valid(object(value));
  const root = value as Record<string, unknown>;
  valid(root.language === "yaml" && root.version === 1 && object(root.roots) && object(root.definitions));
  const definitions = root.definitions as Record<string, unknown>;
  const edges = new Map<string, string[]>();
  valid(Object.keys(definitions).length > 0 && Object.keys(definitions).length <= 4096);
  for (const [id, unknown] of Object.entries(definitions)) {
    valid(string(id) && object(unknown));
    const node = unknown as Record<string, unknown>;
    valid(node.hint === undefined || string(node.hint));
    valid(node.constraints === undefined || strings(node.constraints));
    let children: string[] = [];
    switch (node.kind) {
      case "any": break;
      case "scalar": valid(string(node.type) && (node.choices === undefined || choices(node.choices))); break;
      case "map": valid(string(node.values)); children = [node.values as string]; break;
      case "list": valid(string(node.items)); children = [node.items as string]; break;
      case "union": valid(strings(node.variants) && node.variants.length > 0); children = node.variants as string[]; break;
      case "discriminated":
        valid(string(node.field) && object(node.variants));
        children = Object.values(node.variants as Record<string, string>);
        valid(strings(children) && children.length > 0); break;
      case "object": {
        valid(object(node.fields));
        for (const [name, field] of Object.entries(node.fields as Record<string, unknown>)) {
          valid(string(name) && object(field) && string(field.schema) && typeof field.required === "boolean");
          children.push((field as { schema: string }).schema);
        }
        valid(node.conditions === undefined || Array.isArray(node.conditions));
        for (const condition of (node.conditions ?? []) as unknown[]) {
          valid(object(condition));
          const c = condition as Record<string, unknown>;
          valid(string(c.field) && strings(c.allowed) && (c.values === undefined || strings(c.values)) &&
            (c.prefix === undefined || string(c.prefix)) && (c.values !== undefined || c.prefix !== undefined) &&
            (c.required === undefined || strings(c.required)));
        }
        valid(node.exclusive === undefined || Array.isArray(node.exclusive));
        for (const group of (node.exclusive ?? []) as unknown[]) {
          valid(object(group) && strings(group.fields) && Number.isInteger(group.min) && Number.isInteger(group.max) &&
            (group.min as number) >= 0 && (group.max as number) >= (group.min as number));
        }
        break;
      }
      default: throw new Error("Unsupported YAML completion schema node");
    }
    edges.set(id, children);
  }
  const roots = root.roots as Record<string, unknown>;
  for (const context of ["env", "types"]) valid(string(roots[context]) && edges.has(roots[context] as string));
  // This protocol describes finite declaration shapes. Workspace recursive types
  // are names in TypeExpression values, never recursive schema object graphs.
  const visited = new Set<string>(), active = new Set<string>();
  function visit(id: string, depth: number) {
    valid(edges.has(id) && !active.has(id) && depth <= 128);
    if (visited.has(id)) return;
    active.add(id);
    for (const child of edges.get(id)!) visit(child, depth + 1);
    active.delete(id); visited.add(id);
  }
  for (const id of edges.keys()) visit(id, 0);
  valid(Array.isArray(root.typeConstructors));
  for (const constructor of root.typeConstructors as unknown[]) {
    valid(object(constructor) && string(constructor.name) && strings(constructor.parameters));
  }
  if (root.constraints !== undefined) {
    valid(object(root.constraints));
    for (const constraint of Object.values(root.constraints as Record<string, unknown>)) {
      valid(object(constraint) && string(constraint.description) && constraint.validation === "semantic");
    }
  }
  return value as YamlSchema;
}
