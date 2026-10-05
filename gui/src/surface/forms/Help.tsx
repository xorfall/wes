/** Help is a document, identified by its declared engine type, not guessed record keys. */
import type { FormValue } from "./form";
import "./help.css";

export const MAX_HELP_ROWS = 256;
export const MAX_HELP_CHARS = 64 * 1024;
const MAX_DEPTH = 6;
interface HelpRow { readonly label: string; readonly text: string; readonly code?: boolean }
export interface HelpModel { readonly rows: readonly HelpRow[]; readonly truncated: boolean }
const object = (value: unknown): Record<string, unknown> | undefined =>
  typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : undefined;
const string = (value: unknown): string => typeof value === "string" ? value : "";
const absent = (value: unknown): boolean => value === null || value === undefined || object(value)?.kind === "none";
const labelOf = (key: string): string => key.replace(/([a-z])([A-Z])/g, "$1 $2").replace(/^./, c => c.toUpperCase());

export function isHelp(value: FormValue): boolean {
  const data = object(value.data);
  return value.type?.kind === "record" && value.type.name === "wes.Help"
    && typeof data?.path === "string" && Array.isArray(data.children);
}

/** One work/size bound covers provider metadata, malformed values and future help fields. */
export function readHelp(value: FormValue): HelpModel {
  const rows: HelpRow[] = [];
  let chars = 0, work = 0, truncated = false;
  const step = () => {
    if (++work > MAX_HELP_ROWS * 8 || rows.length >= MAX_HELP_ROWS || chars >= MAX_HELP_CHARS) {
      truncated = true; return false;
    }
    return true;
  };
  const add = (label: string, text: string, code = false) => {
    if (!text || !step()) return;
    const room = Math.max(0, MAX_HELP_CHARS - chars);
    const clippedLabel = label.slice(0, Math.min(256, room));
    const clipped = text.slice(0, room - clippedLabel.length);
    if (clipped.length !== text.length || clippedLabel.length !== label.length) truncated = true;
    rows.push({ label: clippedLabel, text: clipped, code });
    chars += clippedLabel.length + clipped.length;
  };
  const detail = (label: string, data: unknown, depth = 0): void => {
    if (!step() || absent(data)) return;
    if (typeof data === "string" || typeof data === "number" || typeof data === "boolean") {
      add(label, String(data), /example|usage/i.test(label)); return;
    }
    if (depth >= MAX_DEPTH) { truncated = true; return; }
    if (Array.isArray(data)) {
      // Group scalar lists under one label. Preserve runnable examples as separate lines.
      const scalars: string[] = [];
      let scalar = true, scalarChars = 0;
      for (const item of data) {
        if (!step()) break;
        if (!["string", "number", "boolean"].includes(typeof item)) { scalar = false; break; }
        const part = String(item).slice(0, MAX_HELP_CHARS);
        scalars.push(part); scalarChars += part.length;
        if (scalarChars > MAX_HELP_CHARS) { truncated = true; break; }
      }
      if (scalar) add(label, scalars.join(/example/i.test(label) ? "\n" : ", "), /example/i.test(label));
      else for (const item of data) { if (!step()) break; detail(label, item, depth + 1); }
    } else if (object(data)) {
      for (const key in data as Record<string, unknown>) {
        if (!Object.hasOwn(data as object, key)) continue;
        if (!step()) break;
        detail(label ? `${label} · ${labelOf(key)}` : labelOf(key), object(data)![key], depth + 1);
      }
    }
  };
  const typeName = (data: unknown, depth = 0): string => {
    if (!step()) return "…";
    if (typeof data === "string") return data.slice(0, 256);
    const shape = object(data);
    if (!shape) return "Unknown";
    if (depth >= MAX_DEPTH) { truncated = true; return "…"; }
    const kind = string(shape.kind);
    if (["list", "option", "iter"].includes(kind)) {
      return `${labelOf(kind)}<${typeName(shape.element, depth + 1)}>`;
    }
    if (kind === "record" || kind === "meta") return string(shape.name).slice(0, 256) || "Record";
    if (kind === "primitive") {
      const names: Record<string, string> = { INT: "Int", DECIMAL: "Decimal", TEXT: "Text", BOOL: "Bool", INSTANT: "Instant", DURATION: "Duration", INTERVAL: "Interval", BYTES: "Bytes" };
      const name = string(shape.name);
      return Object.hasOwn(names, name) ? names[name]! : (name.slice(0, 256) || "Unknown");
    }
    return "Unknown";
  };
  const resultType = (data: unknown, label = "Result", depth = 0): void => {
    if (!step()) return;
    add(label, typeName(data), true);
    const shape = object(data);
    if (shape?.kind !== "record" || !Array.isArray(shape.fields)) return;
    if (depth >= MAX_DEPTH) { truncated = true; return; }
    for (const field of shape.fields) {
      if (!step()) break;
      const row = object(field), name = string(row?.name);
      if (name) resultType(row?.type, `${label}.${name.slice(0, 256)}`, depth + 1);
    }
  };
  const root = object(value.data) ?? {};
  const provider = string(root.provider), path = string(root.path);
  add("Help", `:help ${[provider, path].filter(Boolean).join(" ")}`.trim(), true);
  const invocation = absent(root.invocation) ? undefined : object(root.invocation);
  add("Summary", string(invocation?.summary) || string(root.summary));
  if (invocation) {
    const command = string(invocation.command) || [provider, path].filter(Boolean).join(" ");
    add("Usage", string(invocation.usage) || command, true);
    add("Short form", string(invocation.shortForm), true);
    if (Array.isArray(invocation.parameters)) {
      for (const item of invocation.parameters) {
        if (!step()) break;
        const p = object(item);
        if (!p || !string(p.name)) continue;
        add(`${string(p.name)}:`, `${typeName(p.type)} · ${p.required === true ? "required" : "optional"}`, true);
        detail("Allowed values", p.choices);
        detail("Constraints", p.constraints);
      }
    }
    for (const key in invocation) {
      if (!Object.hasOwn(invocation, key)) continue;
      if (!step()) break;
      // These fields have already been expressed as prose or source-facing usage.
      if (["command", "provider", "capability", "summary", "usage", "shortForm", "parameters", "operands", "takes"].includes(key)) continue;
      if (key === "result") { resultType(invocation[key]); continue; }
      if (key === "implemented") { if (invocation[key] === false) add("Status", "Reserved; not implemented."); continue; }
      if (key === "otherArguments") { if (invocation[key] === true) add("Arguments", "Additional arguments depend on the selected operation."); continue; }
      if (key === "producesValue") { if (invocation[key] === true) add("Result", "Produces a value that can be named with > result."); continue; }
      detail(labelOf(key), invocation[key]);
    }
  }
  if (Array.isArray(root.children)) {
    for (const child of root.children) {
      if (!step()) break;
      const item = object(child), name = string(item?.name);
      if (!name) continue;
      const childPath = [provider, path, name].filter(Boolean).join(" ");
      add(`${provider ? "" : ":"}${childPath}`, string(item?.summary) || "Command group.");
    }
    if (root.children.length) add("More help", `:help ${[provider, path, "<subcommand>"].filter(Boolean).join(" ")}`, true);
  }
  return { rows, truncated };
}

export function HelpPreview({ model }: { readonly model: HelpModel }) {
  return <section className="help-document" aria-label="Command help" tabIndex={0}>
    <dl>{model.rows.map((row, index) => <div className="help-row" key={index}>
      {row.label && <dt>{row.label}</dt>}
      <dd>{row.code ? <code>{row.text}</code> : row.text}</dd>
    </div>)}</dl>
    {model.truncated && <p role="status">Help display limit reached. Read a specific command or select the remaining metadata with MCP.</p>}
  </section>;
}
