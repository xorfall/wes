import { useContext } from "react";
import { StreamItemsContext } from "./stream-items";
/**
 * Tier 3: one renderer per presentation kind, registered by name.
 *
 * A renderer receives a presentation node and draws it with the design system — mono lines, roles,
 * the sparkline — and knows nothing about `TypeShape`: what to draw, how much of it and what was
 * left out were decided in `present()`. Renderers never truncate. Adding a way to draw something is
 * one entry in `RENDERERS`; adding a way to recognise something is one rule in tier 2 or an entry in
 * the registry.
 */
import { Component, useState, type ReactNode } from "react";
import { InteractiveModule, type SharedInteraction } from "../../value-views/interactive";
import { valueViewModules } from "../../value-views/registry";
import { registryStore } from "../../presentation/registry-store";
import { present } from "../../presentation/present";
import { pageSize, type FieldRow, type Kind, type PresentationNode, type Run, type Tone } from "../../presentation/types";
import { GAP, INDENT, OPENER, rowLines } from "../../presentation/present";
import { width } from "../../presentation/columns";
import { MonoLine, type MonoRole, type Segment } from "../MonoLine";
import { CustomDrawing } from "./custom";
import { TableGrid } from "./TableGrid";
import { ViewLayout } from "./ViewLayout";
import { JsonTree } from "./JsonTree";
import { MoreRows, PinButton, resizeColumn, TableToolbar } from "./TableControls";
import "./presentation.css";

const ROLE: Record<Tone, MonoRole> = {
  ink: "mono-ink", dim: "mono-dim", faint: "mono-faint", literal: "mono-literal", param: "mono-param",
  ref: "mono-ref", ok: "mono-ok", warn: "mono-warn", bad: "mono-bad", meta: "mono-meta", provider: "mono-provider",
};

export function segments(runs: readonly Run[]): Segment[] {
  return runs.map((run) => ({ text: run.text, role: ROLE[run.tone] }));
}

/** Everything a renderer may be told besides its node. */
export interface DrawProps {
  readonly onSort?:(path:string,column:string)=>void;
  /** Opens or closes a nested value by its path; absent where nothing can be toggled. */
  readonly onToggle?: (path: string) => void;
  readonly onPage?: (path: string, page: number) => void;
  /** Shows `rows` rows of the table at a path. */
  readonly onShowRows?: (path: string, rows: number) => void;
  /** Filters the table at a path; empty text clears it. */
  readonly onFilter?: (path: string, text: string) => void;
}

type Renderer<K extends Kind> = (node: Extract<PresentationNode, { kind: K }>, props: DrawProps) => ReactNode;

const spaces = (count: number): Segment => ({ text: " ".repeat(Math.max(0, count)) });

function Line({ runs, lead, before }: { readonly runs: readonly Run[]; readonly lead?: readonly Segment[]; readonly before?: ReactNode }) {
  if (!before) return <MonoLine segments={[...(lead ?? []), ...segments(runs)]} className="value-line" />;
  return <div className="value-line value-line-disclosed">
    {lead && lead.length > 0 && <MonoLine segments={lead} className="value-line" />}
    {before}
    <MonoLine segments={segments(runs)} className="value-line" />
  </div>;
}

/** The control’s accessible description: what opening shows, or what folding returns to. */
function disclosureTitle(node: PresentationNode): string {
  if (node.disclosure === "folds") return node.kind === "text" ? "fold back to one line" : "fold to its summary";
  if (node.kind === "line" && node.more?.lines) return `open the whole value (+${node.more.lines} lines)`;
  if (node.kind === "line" && node.more?.chars) return `open the whole value (+${node.more.chars} columns)`;
  return "open in place";
}

/**
 * The one disclosure control: a `▸` or `▾` drawn at the start of the value column, before the node
 * it opens or folds in place. Tier 2 decides which nodes carry one; nothing here decides what opens.
 */
function disclosure(node: PresentationNode, props: DrawProps): ReactNode {
  if (!node.disclosure) return undefined;
  const glyph = node.disclosure === "folds" ? "▾" : "▸";
  return <button type="button" className="value-toggle value-toggle-inline" aria-expanded={node.disclosure === "folds"} aria-description={disclosureTitle(node)} onClick={() => props.onToggle?.(node.path)}>
    <MonoLine segments={[{ text: `${glyph} `, role: "mono-ref" }]} className="value-line" />
  </button>;
}

/** A nested value's body: one nesting level in from its name's column, whatever the name column's width. */
function Body({ node, indent, props }: { readonly node: PresentationNode; readonly indent: number; readonly props: DrawProps }) {
  return <div className="value-nested" style={{ paddingInlineStart: `${indent}ch` }}>{draw(node, props)}<Pagination node={node} props={props} /></div>;
}

/** A field name, padded so every value in the record starts in the same column. */
function nameSegment(name: string, nameWidth: number): Segment[] {
  return [{ text: name, role: "mono-param" }, spaces(nameWidth - width(name) + 2)];
}

function Row({ row, nameWidth, props }: { readonly row: FieldRow; readonly nameWidth: number; readonly props: DrawProps }) {
  const node = row.node;
  if (row.name === "") return <div className="value-nested">{draw(node, props)}</div>;
  const control = disclosure(node, props);
  if (node.kind === "text") {
    const [first, ...rest] = node.lines;
    return <>
      <Line lead={nameSegment(row.name, nameWidth)} runs={[...(first ?? []), ...(rest.length === 0 && node.newline ? [{ text: " ⏎", tone: "faint" as const }] : [])]} before={control} />
      {rest.map((line, at) => <Line key={at} lead={[spaces(nameWidth + GAP + (control ? OPENER : 0))]} runs={[...line, ...(at === rest.length - 1 && node.newline ? [{ text: " ⏎", tone: "faint" as const }] : [])]} />)}
    </>;
  }
  if (node.kind === "nested") {
    return <>
      <Line lead={nameSegment(row.name, nameWidth)} runs={[{ text: node.summary, tone: "faint" }]} before={control} />
      {node.body && <Body node={node.body} indent={INDENT} props={props} />}
    </>;
  }
  if (control) return <Line lead={nameSegment(row.name, nameWidth)} runs={inline(node)} before={control} />;
  if (rowLines(row) === 1) {
    return <MonoLine segments={[...nameSegment(row.name, nameWidth), ...segments(inline(node))]} className="value-line" />;
  }
  return <>
    <MonoLine segments={[{ text: row.name, role: "mono-param" }]} className="value-line" />
    <div className="value-nested" style={{ paddingInlineStart: "4ch" }}>{draw(node, props)}</div>
  </>;
}

/** A node that fits beside a name on one line, as runs; its disclosure, if any, is drawn apart. */
function inline(node: PresentationNode): readonly Run[] {
  switch (node.kind) {
    case "line": return node.runs;
    case "empty": return [{ text: node.text, tone: "faint" }];
    case "bytes": return [{ text: `${node.size} B`, tone: "literal" }, { text: ` · ${node.note}`, tone: "faint" }];
    case "nested": return [{ text: node.summary, tone: "faint" }];
    case "items": return node.items;
    default: return [];
  }
}

function Table({ node, props }: { node: Extract<PresentationNode, { kind: "table" }>; props: DrawProps }) {
  const sourceKeys=useContext(StreamItemsContext);
  const itemKeys=node.path==="" ? sourceKeys : undefined;
  const rowKeys=node.inspection?.rows.map(row=>itemKeys?.get(row.index));
  const [selectedIndex,setSelected]=useState<number>();
  const [selectedKey,setSelectedKey]=useState<string>();
  const selected=itemKeys && selectedKey!==undefined ? [...itemKeys].find(([,key])=>key===selectedKey)?.[0] : selectedIndex;
  const select=(index:number)=>{setSelected(index);setSelectedKey(itemKeys?.get(index));};
  const [tab,setTab]=useState("rows");
  const inspected=node.inspection?.rows.find(row=>row.index===selected);
  const details = node.details?.map((row) => {
    const expanded = row.flatMap((detail, column) => detail?.kind === "nested" && detail.body ? [{ detail, column }] : []);
    return expanded.length === 0 ? null : expanded.map(({ detail, column }) => <div key={detail.path} className="value-table-detail-section">
      <MonoLine segments={[{ text: `${node.columns[column]!.name} · ${detail.summary} · ${detail.path}`, role: "mono-param" }]} />
      <div className="value-table-detail-body">{draw(detail.body!, props)}</div>
      <Pagination node={detail.body!} props={props} />
    </div>);
  });
  const { arrangement } = node;
  const left = node.pagination ? 0 : node.more?.rows ?? 0;
  return <div className="table-view">
    {node.inspection?.mode==="window" && <div className="table-inspection-tabs" role="tablist" aria-label="Table inspection">{["rows","row","columns"].map(name=><button key={name} role="tab" aria-selected={tab===name} className="cell-action" onClick={()=>setTab(name)}>{name}</button>)}</div>}
    <div hidden={tab!=="rows"}>
    <TableGrid rowKeys={rowKeys?.every(key=>key!==undefined) ? rowKeys as string[] : undefined} columns={node.columns} rows={node.rows.map((row) => row.map(segments))} styles={node.styles} tints={node.tints} total={node.total}
    sort={node.sort} onSelect={node.inspection ? row=>select(node.inspection!.rows[row]!.index) : undefined} selected={node.inspection?.rows.findIndex(row=>row.index===selected)}
    onSort={props.onSort ? column=>props.onSort!(node.path,node.columns[column]!.name) : undefined}
    labels={node.columns.map((column) => column.label)} pinned={!arrangement.unpinned}
    toolbar={<TableToolbar key={arrangement.key} path={node.path} arrangement={arrangement} {...(props.onFilter ? { onFilter: props.onFilter } : {})} />}
    headerStart={(column) => column === 0 ? <PinButton arrangement={arrangement} /> : null}
    onResize={(column, columns) => resizeColumn(arrangement, node.columns[column]!.name, columns)}
    footer={left > 0 ? <MoreRows path={node.path} shown={node.rows.length} left={left} exact={node.more?.exact !== false} step={arrangement.step}
      {...(props.onShowRows ? { onShowRows: props.onShowRows } : {})} /> : undefined}
    details={details} renderCell={(row, column, summary) => {
      const detail = node.details?.[row]?.[column];
      const label=node.inspection?.rows[row];
      const rowSelect=column===0 && label ? <button className="table-row-select" aria-label={`Select row ${label.index+1}`} aria-pressed={selected===label.index} onClick={()=>{select(label.index);if(node.inspection?.mode==="window")setTab("row");}}>↳</button> : null;
      return detail ? <>{rowSelect}<button type="button" className="value-toggle value-toggle-inline"
        aria-label={`Open ${detail.path}`}
        aria-expanded={detail.disclosure === "folds"} aria-description={disclosureTitle(detail)}
        onClick={() => props.onToggle?.(detail.path)}>{detail.disclosure === "folds" ? "▾ " : "▸ "}</button>{summary}</> : <>{rowSelect}{summary}</>;
    }} />
    </div>
    {node.inspection && <div hidden={tab!=="row"} className="table-row-inspector" role="tabpanel" aria-label="Selected table row">
      {inspected ? <><p className="mono-dim">{node.path}/{inspected.index} · row {inspected.index+1}{node.inspection.whole ? "" : " · in the read part"}</p><JsonTree data={inspected.value} type={node.inspection.type} mode="window" declared={node.inspection.declared}/></> : <p className="mono-dim">Select a row in rows.</p>}
    </div>}
    <div hidden={tab!=="columns"} role="tabpanel" aria-label="Table columns">{arrangement.columns.map(name=><p key={name}><span className="json-key">{name}</span> · {arrangement.hidden.includes(name) ? "hidden" : "shown"}</p>)}</div>
  </div>;
}

function Pagination({ node, props }: { node: PresentationNode; props: DrawProps }) {
  const page = node.pagination;
  if (!page || page.total <= pageSize()) return null;
  return <div className="value-nested-pages" aria-label={`Pages of ${node.path || "root"}`}>
    <button type="button" disabled={page.offset === 0} onClick={() => props.onPage?.(node.path, page.offset / pageSize() - 1)}>previous</button>
    <span>{page.offset + 1}–{page.offset + page.shown} of {page.total}</span>
    <button type="button" disabled={page.offset + page.shown >= page.total} onClick={() => props.onPage?.(node.path, page.offset / pageSize() + 1)}>next</button>
  </div>;
}

const RENDERERS: { readonly [K in Kind]: Renderer<K> } = {
  line: (node, props) => <Line runs={node.runs} before={disclosure(node, props)} />,
  fields: (node, props) => node.tree ? <JsonTree {...node.tree}/> : <div className="value-fields">
    {node.rows.map((row, at) => <Row key={at} row={row} nameWidth={node.nameWidth} props={props} />)}
  </div>,
  table: (node, props) => <Table node={node} props={props} />,
  items: (node) => node.lines && node.lines.length > 1
    ? <div className="value-text">{node.lines.map((line, at) => <Line key={at} runs={line} />)}</div>
    : <Line runs={node.items} />,
  text: (node, props) => <div className="value-text">
    {node.lines.map((line, at) => <Line key={at} runs={[...line, ...(at === node.lines.length - 1 && node.newline ? [{ text: " ⏎", tone: "faint" as const }] : [])]}
      before={at === 0 ? disclosure(node, props) : undefined} />)}
  </div>,
  bytes: (node) => <MonoLine segments={segments(inline(node))} className="value-line" />,
  empty: (node) => <MonoLine segments={segments(inline(node))} className="value-line" />,
  nested: (node, props) => <div className="value-fields">
    <Line runs={inline(node)} before={disclosure(node, props)} />
    {node.body && <Body node={node.body} indent={INDENT} props={props} />}
  </div>,
  process: (node, props) => <div className="value-fields">
    <Row row={{ name: "stdout", node: node.stdout }} nameWidth={6} props={props} />
    <Row row={{ name: "stderr", node: node.stderr }} nameWidth={6} props={props} />
  </div>,
  view: (node, props) => <ModuleView node={node} drawProps={props} />,
  error: (node) => <div className="value-error">
    <MonoLine segments={[{ text: node.code, role: "mono-bad" }, { text: " · ", role: "mono-faint" }, { text: node.message, role: "mono-bad" }]} className="value-line" />
    {node.span && <MonoLine segments={[{ text: node.span, role: "mono-faint" }]} className="value-line" />}
  </div>,
  notice: (node) => <div className="value-notice">
    {node.lines.map((line, at) => <MonoLine key={at} segments={[
      ...(at === 0 ? [{ text: "ⓘ ", role: node.severity === "warning" ? "mono-warn" as const : "mono-meta" as const }] : []),
      { text: line, role: node.severity === "warning" ? "mono-warn" : "mono-dim" },
    ]} className="value-line" />)}
  </div>,
  stream: (node, props) => <div className="value-stream">
    {node.body ? draw(node.body, props) : <MonoLine segments={[{ text: node.note, role: node.live ? "mono-meta" : "mono-warn" }]} className="value-line" />}
  </div>,
  custom: (node) => <CustomDrawing name={node.name} data={node.data} />,
};

/** Draws one presentation node with the renderer registered for its kind. */
export function draw(node: PresentationNode, props: DrawProps = {}): ReactNode {
  const renderer = RENDERERS[node.kind] as Renderer<typeof node.kind>;
  return renderer(node as never, props);
}

export function Presented({ node, ...props }: { readonly node: PresentationNode } & DrawProps) {
  return <div className="value-presented" onKeyDown={event => {
    if ((event.metaKey || event.ctrlKey) && (event.key === "Enter" || event.key.toLowerCase() === "r")) { event.preventDefault(); event.stopPropagation(); }
  }}>{draw(node, props)}</div>;
}


/** A failed optional renderer cannot take down the result surface. */
class ViewBoundary extends Component<{ fallback: () => ReactNode; value: unknown; children: ReactNode }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  componentDidUpdate(previous: Readonly<{ value: unknown }>) { if (previous.value !== this.props.value && this.state.failed) this.setState({ failed: false }); }
  render() { return this.state.failed ? this.props.fallback() : this.props.children; }
}
function ModuleView({ node, drawProps, shared, nested=false }: { node: Extract<PresentationNode, { kind: "view" }>; drawProps: DrawProps; shared?: SharedInteraction; nested?:boolean }) {
  if(node.waiting)return <p className="mono-warn" role="status">{node.waiting}</p>;
  const module = valueViewModules.named(node.view);
  const fallback = () => {
    const generic = present({ prepared: { viewModules: node.fallback.viewModules, type: node.fallback.type, data: node.fallback.data, provenance: {}, pending: false },
      context: node.fallback.context, registry: registryStore.get(), skipViews: new Set([node.view]) });
    return <div><p className="mono-warn" role="status">View unavailable · showing data</p>{draw(generic.root, drawProps)}</div>;
  };
  if (!module) return fallback();
  const renderChild = (controller?: SharedInteraction): import("../../value-views/contract").ViewComponentProps["renderChild"] => (child, options) => {
    if (!node.children.includes(child)) throw new Error("Undeclared view child");
    const target = child.kind === "view" ? valueViewModules.named(child.view) : undefined;
    const inherited = options?.interaction === "inherit" && controller && target?.interaction?.protocol === controller.definition.protocol ? controller : undefined;
    return <div key={child.path}>{child.kind === "view"
      ? <ModuleView nested node={child} drawProps={drawProps} {...(inherited ? { shared: inherited } : {})} />
      : draw(child, drawProps)}<Pagination node={child} props={drawProps} /></div>;
  };
  const rendered = <ViewBoundary key={`${node.view}:${node.path}`} fallback={fallback} value={node.fallback.data}>
    {module.interaction
      ? <InteractiveModule module={module} model={node.model} path={node.path} {...(shared ? { shared } : {})}
          render={(interaction, controller) => <module.Component model={node.model} children={node.children} interaction={interaction} renderChild={renderChild(controller)} />} />
      : <module.Component model={node.model} children={node.children} renderChild={renderChild()} />}
  </ViewBoundary>;
  return module.definition ? <ViewLayout nested={nested} layout={module.definition.layout} mode={node.fallback.context.mode}>{rendered}</ViewLayout> : rendered;
}
