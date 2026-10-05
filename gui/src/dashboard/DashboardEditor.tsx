import { budget } from "../limits/policy";
/**
 * Shows a board and, while editing, arranges a local draft beside (or, when narrow, below) it.
 *
 * The canvas keeps one tree position whether or not the panel is open, so opening, saving or
 * cancelling never remounts the result views on the board. Outside editing the canvas shows the
 * board as given; entering editing clones it once into the draft, and source publications and
 * reflow never reset that draft. Nothing is stored until Save hands the draft to `onSave`.
 */
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { DashboardCanvas, sourcesByMember, useAvailableWidth, type RenderDashboardMember } from "./DashboardCanvas";
import {
  addDashboardMember, changeDashboardNode, dashboardName, dashboardNodes, moveDashboardNode, readDashboard, removeDashboardMember,
  max_dashboard_members, type Alignment, type Dashboard, type DashboardMember, type DashboardNode, type DashboardSource, type WidthPolicy,
} from "./model";
import "./dashboard.css";

export interface DashboardEditorProps {
  readonly board: Dashboard;
  readonly sources: readonly DashboardSource[];
  readonly renderMember: RenderDashboardMember;
  /** Whether the arrangement panel and selection are shown; false shows the board read-only. */
  readonly editing?: boolean;
  /** Receives the draft; the caller persists it and reports `busy` and `problem`. */
  readonly onSave: (board: Dashboard) => void | Promise<void>;
  readonly onCancel: () => void;
  readonly busy?: boolean;
  readonly problem?: string;
}

type Leaf = Extract<DashboardNode, { kind: "member" }>;
type Group = Extract<DashboardNode, { kind: "row" | "column" }>;

/** Panel width beside the canvas, and the canvas width that must remain for the panel to sit beside it. */
const PANEL_WIDTH = 288;
const PANEL_GAP = 12;
const CANVAS_BESIDE_MIN = 440;
const WEIGHT_MIN = 1;
const WEIGHT_MAX = 16;
const BASIS_MAX = 512;
const TEMPLATE_SIZE = 3;
const TEMPLATE_BASIS = 56;
/** Limits `readDashboard` enforces on Save, mirrored here so the panel can explain them first. */
const TITLE_MAX = 2048;
const DEPTH_MAX = 8;
function nodes_max(){return budget("ui.dashboard.nodes");}
function children_max(){return budget("ui.dashboard.members");}

const WIDTHS: readonly { readonly value: WidthPolicy; readonly label: string }[] = [
  { value: "auto", label: "Auto" }, { value: "fill", label: "Fill" }, { value: "preferred", label: "Preferred" },
];
const ALIGNMENTS: readonly { readonly value: Alignment; readonly label: string }[] = [
  { value: "auto", label: "Auto" }, { value: "start", label: "Start" }, { value: "center", label: "Center" }, { value: "end", label: "End" },
];
const KINDS: readonly { readonly value: Group["kind"]; readonly label: string }[] = [
  { value: "row", label: "Row" }, { value: "column", label: "Column" },
];

function isLeaf(node: DashboardNode): node is Leaf { return node.kind === "member"; }

/** The group holding a node and the node's index among its children. */
function locate(root: DashboardNode, id: string): { readonly parent: Group; readonly index: number } | undefined {
  if (isLeaf(root)) return undefined;
  const index = root.children.findIndex(child => child.id === id);
  if (index >= 0) return { parent: root, index };
  for (const child of root.children) {
    const found = locate(child, id);
    if (found) return found;
  }
  return undefined;
}

function group(kind: Group["kind"], children: readonly DashboardNode[], basis?: number): Group {
  return { kind, id: crypto.randomUUID(), children, weight: 1, ...(basis === undefined ? {} : { basis }) };
}

/** Root row of columns, each `column(row(first, second), third)`, keeping every leaf (and its id). */
function groupInThrees(board: Dashboard): Dashboard {
  const leaves = dashboardNodes(board.layout).filter(isLeaf);
  const columns: Group[] = [];
  for (let start = 0; start < leaves.length; start += TEMPLATE_SIZE) {
    const set = leaves.slice(start, start + TEMPLATE_SIZE);
    const children = set.length > 1 ? [group("row", set.slice(0, 2)), ...set.slice(2)] : set;
    columns.push(group("column", children, TEMPLATE_BASIS));
  }
  return { ...board, layout: { kind: "row", id: board.layout.id, children: columns, weight: board.layout.weight } };
}

function onBoard(board: Dashboard, source: DashboardSource): boolean {
  return board.members.some(member => member.node === source.node && member.generation === source.generation);
}

/** Each node's nesting depth; the root is depth 0. */
function depths(root: DashboardNode): ReadonlyMap<string, number> {
  const result = new Map<string, number>();
  const visit = (node: DashboardNode, depth: number) => {
    result.set(node.id, depth);
    if (!isLeaf(node)) for (const child of node.children) visit(child, depth + 1);
  };
  visit(root, 0);
  return result;
}

/** The layout without the subtree rooted at `id`. */
function prune(node: DashboardNode, id: string): DashboardNode {
  return isLeaf(node) ? node : { ...node, children: node.children.filter(child => child.id !== id).map(child => prune(child, id)) };
}

/** Removes a group and every result inside it; removing the root clears the board but keeps the root group. */
function removeGroup(board: Dashboard, id: string): Dashboard {
  const target = dashboardNodes(board.layout).find(node => node.id === id);
  if (!target || isLeaf(target)) return board;
  const gone = new Set(dashboardNodes(target).filter(isLeaf).map(leaf => leaf.member));
  const layout = id === board.layout.id ? { ...target, children: [] } : prune(board.layout, id);
  return { ...board, members: board.members.filter(member => !gone.has(member.id)), layout };
}

/** Why the draft cannot be saved, or undefined when Save may try. The host still validates. */
function draftProblem(draft: Dashboard, nodeDepths: ReadonlyMap<string, number>): string | undefined {
  if (!dashboardName(draft.name)) return "Name needs 1–96 characters without spaces, $, / or \\.";
  if (draft.title.trim() === "" || draft.title.length > TITLE_MAX) return `Title needs 1–${TITLE_MAX} characters and cannot be blank.`;
  const nodes = dashboardNodes(draft.layout);
  if (nodes.length > nodes_max()) return `A board holds at most ${nodes_max()} results and groups. Remove a group to save.`;
  if (Math.max(...nodeDepths.values()) > DEPTH_MAX) return `Groups nest more than ${DEPTH_MAX} levels deep. Remove a group to save.`;
  if (nodes.some(node => !isLeaf(node) && node.children.length > children_max())) return `A group holds at most ${children_max()} items. Move some into another group.`;
  if (!readDashboard(draft)) return "This board cannot be saved. Check its title and result labels.";
  return undefined;
}

function nodeName(node: DashboardNode, members: ReadonlyMap<string, DashboardMember>): string {
  return isLeaf(node) ? members.get(node.member)?.label ?? node.member : `${node.kind === "row" ? "Row" : "Column"} · ${node.children.length}`;
}

function Choice<T extends string>({ label, options, value, disabled, onChoose }: {
  readonly label: string; readonly options: readonly { readonly value: T; readonly label: string }[];
  readonly value: T | undefined; readonly disabled: boolean; readonly onChoose: (value: T) => void;
}) {
  const id = useId();
  return (
    <div className="dashboard-editor-field">
      <span id={id} className="dashboard-editor-label">{label}</span>
      <span className="dashboard-segments" role="group" aria-labelledby={id}>
        {options.map(option => (
          <button key={option.value} type="button" className="dashboard-button" aria-pressed={option.value === value} disabled={disabled}
            onClick={() => onChoose(option.value)}>{option.label}</button>
        ))}
      </span>
    </div>
  );
}

/** An integer field that commits only valid values; an invalid entry stays visible but is not applied. */
function IntegerField({ label, value, min, max, hint, disabled, onCommit }: {
  readonly label: string; readonly value: number; readonly min: number; readonly max: number; readonly hint?: string;
  readonly disabled: boolean; readonly onCommit: (value: number) => void;
}) {
  const id = useId();
  const [text, setText] = useState(String(value));
  useEffect(() => setText(String(value)), [value]);
  const parsed = Number(text), valid = text.trim() !== "" && Number.isInteger(parsed) && parsed >= min && parsed <= max;
  return (
    <div className="dashboard-editor-field">
      <label htmlFor={id} className="dashboard-editor-label">{label}</label>
      <input id={id} className="dashboard-input dashboard-number" type="number" inputMode="numeric" step={1} min={min} max={max}
        value={text} disabled={disabled} aria-invalid={!valid}
        onChange={event => {
          const next = event.target.value, number = Number(next);
          setText(next);
          if (next.trim() !== "" && Number.isInteger(number) && number >= min && number <= max) onCommit(number);
        }}
        onBlur={() => setText(String(value))} />
      {hint && <span className="dashboard-editor-hint">{hint}</span>}
    </div>
  );
}

/**
 * The board editor: canvas plus arrangement panel over one local draft.
 *
 * @param props the board to edit, the available sources, the body renderer, save/cancel handlers
 *   and the caller's busy state and problem text
 * @returns the editor element
 */
export function DashboardEditor({ board, sources, renderMember, editing = true, onSave, onCancel, busy = false, problem }: DashboardEditorProps) {
  const [draft, setDraft] = useState(board);
  const [opened, setOpened] = useState(editing);
  const [selection, setSelection] = useState<string>();
  const [choice, setChoice] = useState("");
  const wrapper = useRef<HTMLDivElement>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const width = useAvailableWidth(wrapper);
  const beside = width >= PANEL_WIDTH + PANEL_GAP + CANVAS_BESIDE_MIN;
  const nameId = useId(), titleId = useId(), sourceId = useId();

  // Entering editing clones the board once; leaving drops selection. Board updates while editing are ignored.
  if (opened !== editing) {
    setOpened(editing);
    if (editing) setDraft(structuredClone(board));
    setSelection(undefined);
    setChoice("");
  }
  const shown = editing ? draft : board;

  useEffect(() => { if (editing) heading.current?.focus({ preventScroll: true }); }, [editing]);

  const nodes = dashboardNodes(draft.layout);
  const nodeDepths = useMemo(() => depths(draft.layout), [draft.layout]);
  const members = new Map(draft.members.map(member => [member.id, member] as const));
  const memberSources = useMemo(() => sourcesByMember(draft.members, sources), [draft.members, sources]);
  const selected = nodes.find(node => node.id === selection);
  const location = selected ? locate(draft.layout, selected.id) : undefined;
  const full = draft.members.length >= max_dashboard_members();
  const chosen = sources.find(source => source.id === choice);
  const canAdd = !busy && !full && chosen !== undefined && !onBoard(draft, chosen);
  const isRoot = selected?.id === draft.layout.id;
  const saveProblem = draftProblem(draft, nodeDepths);
  const selectedSource = selected && isLeaf(selected) ? memberSources.get(selected.member) : undefined;
  const canWrap = selected !== undefined && (nodeDepths.get(selected.id) ?? 0) < DEPTH_MAX && nodes.length < nodes_max();
  const leavesIn = selected && !isLeaf(selected) ? dashboardNodes(selected).filter(isLeaf).length : 0;

  const changeLayout = (change: (layout: DashboardNode) => DashboardNode) => {
    if (!busy) setDraft(current => ({ ...current, layout: change(current.layout) }));
  };
  const changeSelected = (change: (node: DashboardNode) => DashboardNode) => {
    if (selected) changeLayout(layout => changeDashboardNode(layout, selected.id, change));
  };

  const add = () => {
    if (!canAdd || !chosen) return;
    // Imported members keep arbitrary ids; a colliding one must not block adding a different reference.
    const id = members.has(chosen.id) ? crypto.randomUUID() : chosen.id;
    const member: DashboardMember = { id, node: chosen.node, generation: chosen.generation, label: chosen.label };
    const after = selected && isLeaf(selected) ? location : undefined;
    const target = selected && !isLeaf(selected) ? selected.id : after?.parent.id ?? draft.layout.id;
    let next = addDashboardMember(draft, member, target);
    if (next === draft) return;
    const leaf = dashboardNodes(next.layout).find(node => isLeaf(node) && node.member === member.id);
    if (leaf && after) {
      for (let step = after.parent.children.length - (after.index + 1); step > 0; step -= 1) {
        next = { ...next, layout: moveDashboardNode(next.layout, leaf.id, -1) };
      }
    }
    setDraft(next);
    setChoice("");
    if (leaf) setSelection(leaf.id);
  };
  const remove = (leaf: Leaf) => {
    if (busy) return;
    setDraft(current => removeDashboardMember(current, leaf.member));
    setSelection(location?.parent.id);
  };
  const removeSelectedGroup = (target: Group) => {
    if (busy) return;
    setDraft(current => removeGroup(current, target.id));
    setSelection(location?.parent.id ?? draft.layout.id);
  };
  const keys = (event: KeyboardEvent) => {
    // A value popup that handles its own Escape prevents or stops it before it reaches here.
    if (!editing || event.key !== "Escape" || event.defaultPrevented || busy) return;
    event.preventDefault();
    onCancel();
  };

  const tree = (node: DashboardNode): ReactNode => (
    <li key={node.id}>
      <button type="button" className="dashboard-tree-node" aria-pressed={node.id === selection} disabled={busy}
        onClick={() => setSelection(node.id)}>{node.id === draft.layout.id ? `Board · ${nodeName(node, members)}` : nodeName(node, members)}</button>
      {!isLeaf(node) && node.children.length > 0 && <ul className="dashboard-tree">{node.children.map(tree)}</ul>}
    </li>
  );

  return (
    <div ref={wrapper} className={`dashboard-editor-frame${!editing ? " viewing" : beside ? " beside" : " below"}`} onKeyDown={keys}>
      <div className="dashboard-editor-canvas">
        <DashboardCanvas board={shown} sources={sources} renderMember={renderMember}
          selected={editing ? selection : undefined} onSelect={editing ? id => { if (!busy) setSelection(id); } : undefined} />
      </div>
      {editing && <aside className="dashboard-editor" aria-labelledby={`${nameId}-heading`} aria-busy={busy}>
        <header className="dashboard-editor-head">
          <h2 id={`${nameId}-heading`} ref={heading} tabIndex={-1}>Arrange board</h2>
          <span className="dashboard-editor-status">Draft · not saved</span>
        </header>

        <section className="dashboard-editor-section">
          <div className="dashboard-editor-field">
            <label htmlFor={nameId} className="dashboard-editor-label">Name</label>
            <input id={nameId} className="dashboard-input" value={draft.name} disabled={busy} spellCheck={false} aria-invalid={!dashboardName(draft.name)}
              onChange={event => { const name = event.target.value; setDraft(current => ({ ...current, name })); }} />
          </div>
          <div className="dashboard-editor-field">
            <label htmlFor={titleId} className="dashboard-editor-label">Title</label>
            <input id={titleId} className="dashboard-input" value={draft.title} disabled={busy} maxLength={TITLE_MAX}
              aria-invalid={draft.title.trim() === "" || draft.title.length > TITLE_MAX}
              onChange={event => { const title = event.target.value; setDraft(current => ({ ...current, title })); }} />
          </div>
        </section>

        <section className="dashboard-editor-section">
          <h3 className="dashboard-editor-title">Add result</h3>
          <div className="dashboard-editor-field">
            <label htmlFor={sourceId} className="dashboard-editor-label">Result</label>
            <select id={sourceId} className="dashboard-input" value={choice} disabled={busy || full || sources.length === 0}
              onChange={event => setChoice(event.target.value)}>
              <option value="">{sources.length === 0 ? "No results available" : "Choose a result"}</option>
              {sources.map(source => {
                const taken = onBoard(draft, source);
                return <option key={source.id} value={source.id} disabled={taken}>{taken ? `${source.label} (on board)` : source.label}</option>;
              })}
            </select>
            <button type="button" className="dashboard-button" disabled={!canAdd} onClick={add}>Add</button>
          </div>
          <p className="dashboard-editor-hint">
            {full ? `A board holds at most ${max_dashboard_members()} results.`
              : selected && isLeaf(selected) ? "Adds after the selected result."
              : selected && !isRoot ? "Adds to the end of the selected group." : "Adds to the end of the board."}
          </p>
        </section>

        <section className="dashboard-editor-section">
          <h3 className="dashboard-editor-title">Board</h3>
          <Choice label="Root" options={KINDS} value={isLeaf(draft.layout) ? undefined : draft.layout.kind} disabled={busy || isLeaf(draft.layout)}
            onChoose={kind => changeLayout(layout => isLeaf(layout) ? layout : { ...layout, kind })} />
          <div className="dashboard-editor-field">
            <button type="button" className="dashboard-button" disabled={busy || !nodes.some(isLeaf)}
              onClick={() => setDraft(current => groupInThrees(current))}>Group in threes</button>
            <span className="dashboard-editor-hint">Two side by side above a third, per set</span>
          </div>
          <ul className="dashboard-tree" aria-label="Layout">{tree(draft.layout)}</ul>
        </section>

        <section className="dashboard-editor-section">
          <h3 className="dashboard-editor-title">{selected ? `Selected · ${nodeName(selected, members)}` : "Selected"}</h3>
          {!selected ? <p className="dashboard-editor-hint">Select a result or group on the board or in the layout list.</p> : (
            <>
              {isLeaf(selected) ? (
                <>
                  <Choice label="Width" options={WIDTHS} value={selected.width} disabled={busy}
                    onChoose={value => changeSelected(node => isLeaf(node) ? { ...node, width: value } : node)} />
                  {selected.width === "auto" && (
                    <p className="dashboard-editor-hint">
                      {selectedSource ? `Auto uses the view's ${selectedSource.width === "fill" ? "fill" : "preferred"} width.` : "Auto fills its slot while the view is unknown."}
                    </p>
                  )}
                  <Choice label="Align" options={ALIGNMENTS} value={selected.align} disabled={busy}
                    onChoose={value => changeSelected(node => isLeaf(node) ? { ...node, align: value } : node)} />
                  <p className="dashboard-editor-hint">
                    {selected.align === "auto"
                      ? (selectedSource ? `Auto uses the view's ${selectedSource.align} alignment. ` : "Auto follows the view's alignment once it is known. ")
                      : selectedSource ? `View default · ${ALIGNMENTS.find(option => option.value === selectedSource.align)?.label ?? selectedSource.align}. ` : ""}
                    Alignment places the result within spare slot space, including beside a fill width at its maximum.
                  </p>
                  <div className="dashboard-editor-field">
                    <span className="dashboard-editor-label">Wrap in</span>
                    {KINDS.map(kind => (
                      <button key={kind.value} type="button" className="dashboard-button" disabled={busy || !canWrap}
                        onClick={() => changeSelected(node => ({ kind: kind.value, id: crypto.randomUUID(), children: [node], weight: node.weight }))}>{kind.label}</button>
                    ))}
                    {!canWrap && <span className="dashboard-editor-hint">Nesting limit reached</span>}
                  </div>
                </>
              ) : (
                <>
                  <Choice label="Kind" options={KINDS} value={selected.kind} disabled={busy}
                    onChoose={kind => changeSelected(node => isLeaf(node) ? node : { ...node, kind })} />
                  <IntegerField key={`basis-${selected.id}`} label="Basis" value={selected.basis ?? 0} min={0} max={BASIS_MAX} hint="columns · 0 auto" disabled={busy}
                    onCommit={basis => changeSelected(node => isLeaf(node) ? node
                      : { kind: node.kind, id: node.id, children: node.children, weight: node.weight, ...(basis > 0 ? { basis } : {}) })} />
                </>
              )}
              {!isRoot && (
                <IntegerField key={`weight-${selected.id}`} label="Weight" value={selected.weight} min={WEIGHT_MIN} max={WEIGHT_MAX} disabled={busy}
                  onCommit={weight => changeSelected(node => ({ ...node, weight }))} />
              )}
              {location && (
                <div className="dashboard-editor-field">
                  <span className="dashboard-editor-label">Order</span>
                  <button type="button" className="dashboard-button" disabled={busy || location.index === 0}
                    onClick={() => changeLayout(layout => moveDashboardNode(layout, selected.id, -1))}>Before</button>
                  <button type="button" className="dashboard-button" disabled={busy || location.index === location.parent.children.length - 1}
                    onClick={() => changeLayout(layout => moveDashboardNode(layout, selected.id, 1))}>After</button>
                  {isLeaf(selected) && <button type="button" className="dashboard-button" disabled={busy} onClick={() => remove(selected)}>Remove</button>}
                </div>
              )}
              {!isLeaf(selected) && (
                <div className="dashboard-editor-field">
                  <button type="button" className="dashboard-button danger" disabled={busy || (isRoot && draft.members.length === 0)}
                    onClick={() => removeSelectedGroup(selected)}>{isRoot ? "Clear board" : "Remove group"}</button>
                  <span className="dashboard-editor-hint">
                    {leavesIn === 0 ? "No results inside." : `Takes ${leavesIn} result${leavesIn === 1 ? "" : "s"} off the board. Results are not affected.`}
                  </span>
                </div>
              )}
            </>
          )}
        </section>

        <footer className="dashboard-editor-foot">
          {saveProblem && <p className="dashboard-editor-problem" role="status">{saveProblem}</p>}
          {problem && <p className="dashboard-editor-problem" role="alert">{problem}</p>}
          <div className="dashboard-editor-actions">
            <button type="button" className="dashboard-button" disabled={busy} onClick={onCancel}>Cancel</button>
            <button type="button" className="dashboard-button primary" disabled={busy || saveProblem !== undefined}
              onClick={() => { void onSave(draft); }}>{busy ? "Saving…" : "Save"}</button>
          </div>
        </footer>
      </aside>}
    </div>
  );
}
