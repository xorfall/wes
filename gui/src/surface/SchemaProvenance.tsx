/**
 * Schema provenance on the /spec screen: the final descriptor's types and fields as rows, and for the
 * one target the reader picks — a field, a type, an operation's auth — what the describe step recorded
 * about it: basis, source identifier and location, the original pointer, the cited lines and the reason.
 *
 * Everything here is text. A reason or a pointer is a model's string and is never turned into a link.
 * Nothing is inferred from silence: a field with no entry says "no provenance", an unknown basis stays
 * unknown, and the notice above the rows says which revision the metadata describes.
 */
import { useEffect, useRef, useState, type ReactNode } from "react";
import { SpecDocumentation, documentationSummary } from "./SpecDocumentation";
import { CopyButton, evidenceSummary, nullableParts } from "./spec-table";
import "./spec-reader.css";
import { NO_PROVENANCE, basisLabel, provenanceEntriesFor, pointerToken, weakestBasis, type ProvenanceBasis, type ProvenanceEntry, type SchemaProvenance } from "../api-library";
import { describeLineRanges } from "./DescribeFailureDetails";

export type Optionality = "optional" | "required" | "unknown";
export interface SchemaRow { target: string; type: string; field?: string; expression: string; optionality?: Optionality }

const UNKNOWN_SHAPE = "unknown shape";
const CONSTRAINT_PREVIEW = 40;
const BASIS_ROLE: Record<ProvenanceBasis, string> = { documented: "mono-ok", example: "mono-dim", inferred: "mono-warn", unknown: "mono-warn" };

function isRecord(value: unknown): value is Record<string, unknown> { return !!value && typeof value === "object" && !Array.isArray(value); }
function preview(value: unknown): string {
  const text = JSON.stringify(value) ?? String(value);
  return text.length > CONSTRAINT_PREVIEW ? `${text.slice(0, CONSTRAINT_PREVIEW)}…` : text;
}

/** The types as rows: one for the type itself (its base and constraints), then one per field. */
export function schemaRows(types: Record<string, unknown>): SchemaRow[] {
  return Object.entries(types).flatMap(([name, definition]) => {
    const target = `#/types/${pointerToken(name)}`;
    if (!isRecord(definition)) return [{ target, type: name, expression: typeof definition === "string" ? definition : UNKNOWN_SHAPE }];
    const constraints = Object.entries(definition).filter(([key]) => key !== "base" && key !== "fields" && key !== "description").map(([key, value]) => `${key} ${preview(value)}`);
    const base = typeof definition.base === "string" ? definition.base : UNKNOWN_SHAPE;
    const head: SchemaRow = { target, type: name, expression: [base, ...constraints].join(" · ") };
    const fields = isRecord(definition.fields) ? Object.entries(definition.fields) : [];
    return [head, ...fields.map(([field, shape]): SchemaRow => ({
      target: `${target}/fields/${pointerToken(field)}`, type: name, field,
      expression: isRecord(shape) && typeof shape.type === "string" ? shape.type : typeof shape === "string" ? shape : UNKNOWN_SHAPE,
      optionality: !isRecord(shape) || typeof shape.optional !== "boolean" ? "unknown" : shape.optional ? "optional" : "required",
    }))];
  });
}

const DOCUMENTATION_UNKNOWN = "documentation unknown";

/**
 * What the optional column says. The descriptor's own flag is what the runtime enforces and is always
 * shown; when the current record says the documentation never settled it, that is added beside the
 * flag, not in place of it. A stale record is history and says nothing about this text's flag.
 */
export function optionalityLabel(row: SchemaRow, provenance: SchemaProvenance | undefined): string {
  if (row.optionality === undefined) return "";
  const recorded = provenance?.status === "current" ? provenance.entries.find(entry => entry.target === `${row.target}/optional`) : undefined;
  return recorded?.basis === "unknown" && row.optionality !== "unknown" ? `${row.optionality} · ${DOCUMENTATION_UNKNOWN}` : row.optionality;
}

/** The referenced named definition, through List/Option/Map wrappers. */
export function referencedType(expression: string, types: Record<string, unknown>): string | undefined {
  return expression.match(/[A-Za-z_][A-Za-z0-9_]*/g)?.find(name => Object.hasOwn(types, name));
}

/** The deepest nested level shown in place; deeper or repeated references stay links. */
const NESTED_DEPTH = 3;
const PREVIEW_FIELDS = 5;
/** The types table's columns, which an opened type's detail spans. */
const TYPE_COLUMNS = 5;
const FIELD_OWN_KEYS = ["type", "optional", "description"];
/** Where a followed reference came from, and whether following it opened the target type. */
interface ReturnPoint { target: string; name: string; to: string; opened: boolean }

const REQUIRED_TEXT: Record<Optionality, string> = { required: "yes", optional: "no", unknown: "unknown" };
const REQUIRED_TONE: Record<Optionality, string> = { required: "", optional: "mono-dim", unknown: "mono-warn" };

/**
 * Progressive schema disclosure as tables: one row per type, and inside an opened type one row per
 * field — name, type, requiredness, nullability and documentation in their own columns. Nested
 * records open in place, indented under their field; a type reference opens its type and offers a
 * way back. Expansion never changes the schema or provenance.
 */
export function SchemaFieldsTable({ types, provenance, selected, onSelect, requestedType, problems = [], stale = false }: { types: Record<string, unknown>; provenance: SchemaProvenance | undefined; selected?: string; onSelect: (target: string) => void; requestedType?: { name: string }; problems?: readonly { target: string; severity: string }[]; stale?: boolean }) {
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [evidenceOpen, setEvidenceOpen] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState("");
  const [returnTo, setReturnTo] = useState<ReturnPoint>();
  const [focus, setFocus] = useState<string>();
  const root = useRef<HTMLDivElement>(null);
  const flip = (set: (update: (old: Set<string>) => Set<string>) => void, key: string) => set(old => { const next = new Set(old); if (!next.delete(key)) next.add(key); return next; });
  const toggle = (key: string) => flip(setExpanded, key);
  const openType = (expression: string, from?: { target: string; name: string }) => {
    const name = Object.hasOwn(types, expression) ? expression : referencedType(expression, types);
    if (!name) return;
    const target = `#/types/${pointerToken(name)}`;
    setFilter(""); setExpanded(old => new Set([...old, target]));
    setReturnTo(from && { ...from, to: name, opened: !expanded.has(target) }); setFocus(target);
  };
  const goBack = (point: ReturnPoint) => {
    const target = `#/types/${pointerToken(point.to)}`;
    // Close the type only if following the reference opened it, and never around the field we return to.
    if (point.opened && !point.target.startsWith(`${target}/`)) setExpanded(old => { const next = new Set(old); next.delete(target); return next; });
    setFocus(point.target); setReturnTo(undefined);
  };
  useEffect(() => { if (requestedType) openType(requestedType.name); }, [requestedType]);
  useEffect(() => {
    if (!focus) return;
    const button = [...(root.current?.querySelectorAll<HTMLButtonElement>("button[data-schema-target]") ?? [])].find(node => node.dataset.schemaTarget === focus);
    button?.focus(); button?.scrollIntoView?.({ block: "nearest" });
  }, [focus]);
  const rows = schemaRows(types);
  const definitions = Object.entries(types).filter(([name, value]) => `${name} ${isRecord(value) ? value.description ?? "" : ""}`.toLowerCase().includes(filter.toLowerCase()));

  /** One type's fields as flat table rows; an opened nested record's rows follow its field, indented. */
  function fieldRows(name: string, parent: string, depth: number, ancestors: string[]): ReactNode[] {
    const definition = types[name];
    if (!isRecord(definition) || !isRecord(definition.fields)) return [];
    return Object.entries(definition.fields).flatMap(([field, raw]) => {
      const value = isRecord(raw) ? raw : {};
      const expression = typeof raw === "string" ? raw : typeof value.type === "string" ? value.type : UNKNOWN_SHAPE;
      const { inner, nullable } = nullableParts(expression);
      const sourceTarget = `#/types/${pointerToken(name)}/fields/${pointerToken(field)}`;
      const key = `${parent}/fields/${pointerToken(field)}`;
      const ref = referencedType(expression, types);
      const refDef = ref && isRecord(types[ref]) ? types[ref] as Record<string, unknown> : undefined;
      const nested = refDef && isRecord(refDef.fields) ? Object.keys(refDef.fields) : [];
      const canExpand = !!ref && nested.length > 0 && depth < NESTED_DEPTH && !ancestors.includes(ref);
      const open = canExpand && expanded.has(key);
      const row = rows.find(r => r.target === sourceTarget);
      const optionality = row?.optionality ?? "unknown";
      const presence = row ? optionalityLabel(row, provenance) : "unknown";
      const description = typeof value.description === "string" ? value.description : undefined;
      const constraints = Object.entries(value).filter(([k]) => !FIELD_OWN_KEYS.includes(k));
      const basis = weakestBasis(provenanceEntriesFor(provenance, sourceTarget));
      // A field's basis is said only when it is a caution: not documented, or recorded before an edit.
      // Silence stays silent here; the name opens the field's full evidence, including "no provenance".
      const caution = !!basis && (basis !== "documented" || provenance?.status === "stale");
      const line = <div role="row" key={key} className={`spec-t-row spec-fields-row ${selected === sourceTarget || focus === key ? "spec-schema-selected" : ""}`}>
        <div role="cell" className="spec-t-tdw spec-c-field">
          <span className="spec-field-cell" style={{ paddingLeft: `calc(${depth} * var(--field-indent))` }}>
            {canExpand
              ? <button type="button" className="spec-field-chev" aria-expanded={open} aria-label={`${open ? "Collapse" : "Expand"} ${field}`} onClick={() => toggle(key)}>{open ? "▾" : "▸"}</button>
              : <span className="spec-field-mark" aria-hidden="true">{depth > 0 ? "·" : ""}</span>}
            <button type="button" className="spec-field-name" aria-label={`Evidence for ${field}`} aria-description={field} onClick={() => onSelect(sourceTarget)}>{field}</button>
          </span>
        </div>
        <div role="cell" className="spec-t-tdw spec-c-type">
          {ref ? <button type="button" className="spec-type-link" data-schema-target={key} onClick={() => openType(ref, { target: key, name: `${name}.${field}` })}>{inner} ›</button> : <code>{inner}</code>}
          {nullable && <span className="spec-t-narrow mono-meta"> · nullable</span>}
        </div>
        <div role="cell" className={`spec-t-td spec-c-req ${REQUIRED_TONE[optionality]}`} aria-description={presence}>{REQUIRED_TEXT[optionality]}</div>
        <div role="cell" className="spec-t-td spec-c-null mono-meta">{nullable ? "yes" : ""}</div>
        <div role="cell" className="spec-t-tdw spec-c-desc">
          <SpecDocumentation text={description} />
          {constraints.map(([k, v]) => <span key={k} className="spec-field-note"><span className="spec-t-label">{`${k} `}</span><code>{preview(v)}</code></span>)}
          {nullable && <span className="spec-field-note"><span className="spec-t-label">source type </span><code>{expression}</code></span>}
          {presence.includes(DOCUMENTATION_UNKNOWN) && <span className="spec-field-note"><span className="spec-t-label">presence </span>{presence}</span>}
          {canExpand && !open && <span className="spec-field-note spec-field-preview">fields: {nested.slice(0, PREVIEW_FIELDS).join(", ")}{nested.length > PREVIEW_FIELDS ? ` · +${nested.length - PREVIEW_FIELDS}` : ""}</span>}
          {caution && <span className="spec-field-note spec-field-basis"><span className="spec-t-label">evidence </span>{basisLabel(basis!, provenance)}</span>}
        </div>
      </div>;
      return open ? [line, ...fieldRows(ref!, key, depth + 1, [...ancestors, ref!])] : [line];
    });
  }

  if (!rows.length) return <p className="mono-dim">No types in this revision.</p>;
  return <div ref={root} className="spec-reader" aria-label="Schema fields">
    <div className="spec-reader-tools"><input className="settings-field" aria-label="Filter schema types" placeholder="Filter types" value={filter} onChange={e => setFilter(e.target.value)}/><span className="spec-reader-count">{definitions.length} types</span><button type="button" className="spec-reader-link" onClick={() => { setExpanded(new Set()); setReturnTo(undefined); }}>collapse all</button></div>
    {!definitions.length ? <p>No matching types.</p> : <div role="table" aria-label="Types" className="spec-t spec-types">
      <div role="row" className="spec-t-row spec-types-row spec-t-head">
        <div role="columnheader" className="spec-t-th spec-c-chev" /><div role="columnheader" className="spec-t-th spec-c-type">type</div>
        <div role="columnheader" className="spec-t-th spec-c-shape">shape</div><div role="columnheader" className="spec-t-th spec-c-count">fields</div>
        <div role="columnheader" className="spec-t-th spec-c-desc">description</div>
      </div>
      {definitions.map(([name, raw]) => {
        const definition = isRecord(raw) ? raw : {};
        const target = `#/types/${pointerToken(name)}`;
        const opened = expanded.has(target);
        const own = problems.filter(p => p.target === target || p.target.startsWith(`${target}/`));
        const blocking = own.filter(p => p.severity === "error").length;
        const description = typeof definition.description === "string" ? definition.description : undefined;
        const count = isRecord(definition.fields) ? Object.keys(definition.fields).length : undefined;
        const base = typeof definition.base === "string" ? definition.base : typeof raw === "string" ? raw : UNKNOWN_SHAPE;
        const constraints = Object.entries(definition).filter(([key]) => !["base", "fields", "description"].includes(key));
        const fields = fieldRows(name, target, 0, [name]);
        const summary = documentationSummary(undefined, description);
        return <div role="rowgroup" key={name} className={`spec-t-group ${opened ? "spec-t-open" : ""}`}>
          <div role="row" className="spec-t-row spec-types-row">
            <div role="cell" className="spec-t-td spec-c-chev"><button type="button" className="spec-t-chev" data-schema-target={target} aria-expanded={opened} aria-label={`${opened ? "Collapse" : "Expand"} type ${name}`} onClick={() => toggle(target)}>{opened ? "▾" : "▸"}</button></div>
            <div role="cell" className="spec-t-td spec-c-type" aria-description={name}><button type="button" tabIndex={-1} className="spec-t-rowbtn" onClick={() => toggle(target)}>{name}</button></div>
            <div role="cell" className="spec-t-td spec-c-shape mono-dim" aria-description={base}>{base}</div>
            <div role="cell" className="spec-t-td spec-c-count mono-dim">{count ?? ""}</div>
            <div role="cell" className="spec-t-td spec-c-desc" aria-description={opened ? undefined : summary || undefined}>
              {own.length > 0 && <span className={`spec-row-status ${stale ? "mono-dim" : blocking ? "mono-bad" : "mono-warn"}`}>{`${blocking ? `${blocking} blocking` : `${own.length} advisory`}${stale ? " · stale" : ""}`}</span>}
              <span className="spec-doc-summary">{opened ? "" : summary}</span>
            </div>
          </div>
          {opened && <div role="row" className="spec-t-detail-row"><div role="cell" aria-colspan={TYPE_COLUMNS} className="spec-t-detail">
            {returnTo?.to === name && <><span className="spec-t-label" /><div className="spec-reader-return"><span className="mono-meta">{`Opened from ${returnTo.name}`}</span><span className="spec-t-fill" /><button type="button" className="spec-t-action" onClick={() => goBack(returnTo)}>{`← back to ${returnTo.name}`}</button></div></>}
            <span className="spec-t-label">name</span><span className="spec-t-value"><code>{name}</code> <CopyButton text={name} label={`Copy type name ${name}`} /></span>
            {!!description?.trim() && <><span className="spec-t-label">description</span><SpecDocumentation text={description}/></>}
            {constraints.length > 0 && <><span className="spec-t-label">constraints</span><dl className="spec-constraints">{constraints.map(([key, value]) => <div key={key}><dt>{key}</dt><dd><code>{JSON.stringify(value)}</code></dd></div>)}</dl></>}
            {referencedType(base, types) && <><span className="spec-t-label">base</span><span><button type="button" className="spec-type-link" data-schema-target={`${target}/base`} onClick={() => openType(base, { target: `${target}/base`, name })}>{base} ›</button></span></>}
            {fields.length > 0 && <><span className="spec-t-label">fields</span>
              <div role="table" aria-label={`Fields of ${name}`} className="spec-t spec-t-sub spec-fields">
                <div role="row" className="spec-t-row spec-fields-row spec-t-head">
                  <div role="columnheader" className="spec-t-th spec-c-field">field</div><div role="columnheader" className="spec-t-th spec-c-type">type</div>
                  <div role="columnheader" className="spec-t-th spec-c-req"><span className="spec-t-wide">required</span><span className="spec-t-narrow">req.</span></div>
                  <div role="columnheader" className="spec-t-th spec-c-null">nullable</div><div role="columnheader" className="spec-t-th spec-c-desc">description</div>
                </div>
                {fields}
              </div></>}
            <span className="spec-t-label">evidence</span>
            <div className="spec-t-evidence">
              <button type="button" className="spec-t-layer" aria-expanded={evidenceOpen.has(target)} onClick={() => flip(setEvidenceOpen, target)}><span className="mono-ref">{evidenceOpen.has(target) ? "▾" : "▸"}</span>Evidence<span className="mono-dim">{` · ${evidenceSummary(provenance, target)}`}</span></button>
              {evidenceOpen.has(target) && <ProvenanceDetail target={target} provenance={provenance}/>}
            </div>
          </div></div>}
        </div>;
      })}
    </div>}
    <p className="spec-t-footnote">required = the field can’t be left out (unknown when the source doesn’t say) · nullable = its type is Option&lt;…&gt;, shown in full under the description.</p>
  </div>;
}

const dot = <span className="mono-faint"> · </span>;

/** One entry: its basis and target on a head line, then source, pointer, lines and the reason as text. */
function EntryRow({ entry, provenance }: { entry: ProvenanceEntry; provenance: SchemaProvenance }) {
  return <li className="spec-provenance-entry">
    <p className="spec-provenance-head">
      <span className={BASIS_ROLE[entry.basis]}>{basisLabel(entry.basis, provenance)}</span>{dot}<span className="mono-ink">{entry.target}</span>
    </p>
    <p className="spec-provenance-fact"><span className="mono-faint">source </span><span className="mono-dim">{entry.source}</span>
      {provenance.location && <>{dot}<span className="mono-faint">location </span><span className="mono-dim">{provenance.location}</span></>}</p>
    <p className="spec-provenance-fact"><span className="mono-faint">pointer </span><span className="mono-dim">{entry.pointer}</span>
      {entry.lines.length > 0 && <>{dot}<span className="mono-faint">lines </span><span className="mono-dim">{describeLineRanges(entry.lines)}</span><span className="mono-faint"> of the supplied document</span></>}</p>
    <p className="mono-ink spec-provenance-reason">{entry.reason}</p>
  </li>;
}

/** Everything recorded about the picked target, or the plain fact that nothing was. */
export function ProvenanceDetail({ target, provenance }: { target: string; provenance: SchemaProvenance | undefined }) {
  const entries = provenanceEntriesFor(provenance, target);
  return <section className="spec-provenance" aria-label="Schema provenance">
    <p className="spec-provenance-target"><span className="mono-faint">target </span><span className="mono-ref">{target}</span>
      {provenance?.status === "stale" && <>{dot}<span className="mono-warn">stale · recorded before the saved text was edited; it does not describe the current descriptor</span></>}</p>
    {entries.length === 0 || !provenance
      ? <p className="mono-dim">{`${NO_PROVENANCE} recorded for this target. Nothing is implied by its absence.`}</p>
      : <ol className="spec-provenance-entries">{entries.map((entry, index) => <EntryRow key={`${entry.target}:${index}`} entry={entry} provenance={provenance} />)}</ol>}
    <p className="settings-note">A source match locates a claim; it does not prove the interpretation. Inferred and example bases are the describe step's reading, not documentation.</p>
  </section>;
}

/** Which revision the metadata describes, said before any row is read. */
export function ProvenanceNotice({ provenance, dirty, revision }: { provenance: SchemaProvenance | undefined; dirty: boolean; revision: string }) {
  const saved = `saved revision ${revision.slice(0, 12)}`;
  if (dirty) return <p role="status" className="mono-warn spec-provenance-notice">{`Provenance describes the ${saved}, not the unsaved edits in the source tab.`}</p>;
  if (!provenance) return <p role="status" className="mono-dim spec-provenance-notice">No provenance metadata in this revision. Fields are shown without a basis; none is implied.</p>;
  if (provenance.status === "stale") return <p role="status" className="mono-warn spec-provenance-notice">{`Provenance is stale: it was recorded before the ${saved} was edited, so targets may no longer match this text.`}</p>;
  return <p role="status" className="mono-dim spec-provenance-notice">{`Provenance describes the ${saved}. Line numbers count the complete supplied document, not a rendered page.`}</p>;
}
