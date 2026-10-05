/**
 * The dashboard's presentation: one flat, absolutely positioned layer of cards over a layer of
 * group frames, both placed by `layoutDashboard`.
 *
 * Cards are direct siblings keyed by member id and rendered in member order, so reflow, reorder
 * and regrouping only move rectangles — a view inside a card is never recreated by layout.
 * Width flows in (measured from the canvas); height flows out (each card's natural content height,
 * measured from its contents, never from the allocated frame).
 */
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode, type RefObject } from "react";
import { monoAdvance, invalidateMonoMeasurements } from "../surface/render/measure";
import { layoutDashboard, type DashboardBox } from "./geometry";
import { dashboardNodes, type Dashboard, type DashboardMember, type DashboardNode, type DashboardSource } from "./model";
import "./dashboard.css";

/** What a card body may use: its content width in pixels and in display columns. */
export interface DashboardAllocation { readonly width: number; readonly columns: number }
export type RenderDashboardMember = (member: DashboardMember, allocation: DashboardAllocation) => ReactNode;

export interface DashboardCanvasProps {
  readonly board: Dashboard;
  readonly sources: readonly DashboardSource[];
  readonly renderMember: RenderDashboardMember;
  /** The selected layout node id (a leaf or a group). */
  readonly selected?: string;
  /** Present only while editing; enables the per-card and per-group select buttons. */
  readonly onSelect?: (node: string) => void;
}

/** Width assumed when nothing can be measured (tests, a hidden pane). */
export const FALLBACK_DASHBOARD_WIDTH = 780;
/** A card's frame: 1 px border and 12 px padding each side — the 26 px chrome geometry reserves. */
const CARD_BORDER = 1;
const CARD_PADDING = 12;
const CARD_CHROME = 2 * (CARD_BORDER + CARD_PADDING);

const NODE_LABEL: Record<DashboardNode["kind"], string> = { member: "Result", row: "Row", column: "Column" };

/** The measured content width of an element, re-measured on resize; the fallback where none exists. */
export function useAvailableWidth(ref: RefObject<HTMLElement | null>, fallback = FALLBACK_DASHBOARD_WIDTH): number {
  const [width, setWidth] = useState(fallback);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measure = () => {
      const next = element.clientWidth;
      if (next > 0) setWidth(previous => previous === next ? previous : next);
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref]);
  return width;
}

type CardRef = (element: HTMLElement | null) => void;

/**
 * Natural card heights keyed by member id. One observer watches every card's contents; changes are
 * coalesced into one frame and only a changed value produces a new map, so a stable layout settles.
 * A detached card drops its ref and, at the next flush, its height.
 */
function useNaturalHeights(): readonly [ReadonlyMap<string, number>, (member: string) => CardRef, () => void] {
  const [heights, setHeights] = useState<ReadonlyMap<string, number>>(() => new Map());
  const watched = useRef(new Map<HTMLElement, string>());
  const refs = useRef(new Map<string, CardRef>());
  const observer = useRef<ResizeObserver | null>(null);
  const cancel = useRef<(() => void) | null>(null);
  const alive = useRef(false);

  const flush = useCallback(() => {
    cancel.current = null;
    setHeights(previous => {
      const present = new Set(watched.current.values());
      let next: Map<string, number> | undefined;
      for (const member of previous.keys()) if (!present.has(member)) (next ??= new Map(previous)).delete(member);
      for (const [element, member] of watched.current) {
        const content = Math.ceil(element.getBoundingClientRect().height);
        if (content <= 0) continue;
        const height = content + 2 * CARD_BORDER;
        if ((next ?? previous).get(member) !== height) (next ??= new Map(previous)).set(member, height);
      }
      return next ?? previous;
    });
  }, []);
  const schedule = useCallback(() => {
    if (cancel.current || !alive.current) return;
    if (typeof requestAnimationFrame !== "undefined") {
      const frame = requestAnimationFrame(flush);
      cancel.current = () => cancelAnimationFrame(frame);
    } else {
      const timer = setTimeout(flush, 0);
      cancel.current = () => clearTimeout(timer);
    }
  }, [flush]);
  const observe = useCallback((element: HTMLElement) => {
    if (typeof ResizeObserver === "undefined") return;
    observer.current ??= new ResizeObserver(schedule);
    observer.current.observe(element);
  }, [schedule]);

  useEffect(() => {
    // Refs attach before effects run; a remount (StrictMode) must re-observe what is already watched.
    alive.current = true;
    for (const element of watched.current.keys()) observe(element);
    schedule();
    return () => {
      alive.current = false;
      observer.current?.disconnect();
      observer.current = null;
      cancel.current?.();
      cancel.current = null;
    };
  }, [observe, schedule]);

  const refFor = useCallback((member: string) => {
    const existing = refs.current.get(member);
    if (existing) return existing;
    let current: HTMLElement | null = null;
    const ref: CardRef = element => {
      if (current) { observer.current?.unobserve(current); watched.current.delete(current); }
      current = element;
      if (!element) {
        if (refs.current.get(member) === ref) refs.current.delete(member);
        schedule();
        return;
      }
      refs.current.set(member, ref);
      watched.current.set(element, member);
      observe(element);
      schedule();
    };
    refs.current.set(member, ref);
    return ref;
  }, [observe, schedule]);

  return [heights, refFor, schedule];
}

/** The mono advance of the canvas's own theme font, re-read on resize and when web fonts load. */
function useCanvasAdvance(sizer: RefObject<HTMLElement | null>, remeasure: () => void): number {
  const [advance, setAdvance] = useState(() => monoAdvance());
  const read = useCallback(() => {
    const next = monoAdvance(sizer.current);
    setAdvance(previous => previous === next ? previous : next);
  }, [sizer]);
  useLayoutEffect(read);
  useEffect(() => {
    const fonts = typeof document === "undefined" ? undefined : document.fonts;
    if (!fonts) return;
    let live = true;
    const refresh = () => { if (live) { invalidateMonoMeasurements(); read(); remeasure(); } };
    fonts.addEventListener?.("loadingdone", refresh);
    void fonts.ready?.then(refresh);
    return () => { live = false; fonts.removeEventListener?.("loadingdone", refresh); };
  }, [read, remeasure]);
  return advance;
}

/** Sources keyed by board member id, matched on the member's node and generation, never its id. */
export function sourcesByMember(members: readonly DashboardMember[], sources: readonly DashboardSource[]): ReadonlyMap<string, DashboardSource> {
  const byReference = new Map(sources.map(source => [JSON.stringify([source.node, source.generation]), source] as const));
  const result = new Map<string, DashboardSource>();
  for (const member of members) {
    const source = byReference.get(JSON.stringify([member.node, member.generation]));
    if (source) result.set(member.id, source);
  }
  return result;
}

/** Rows a body may show before it scrolls internally; undefined when the view declares no finite bound. */
function maximumRows(source: DashboardSource | undefined): number | undefined {
  const rows = source?.sizing.max.rows;
  return rows !== undefined && Number.isFinite(rows) && rows > 0 ? rows : undefined;
}

function place(box: DashboardBox): CSSProperties {
  return { left: box.x, top: box.y, width: box.width, height: box.height };
}

/**
 * Renders a board at the width it is given.
 *
 * @param props the board, the sources its members resolve to, the body renderer and, while
 *   editing, the selection and its setter
 * @returns the canvas element
 */
export function DashboardCanvas({ board, sources, renderMember, selected, onSelect }: DashboardCanvasProps) {
  const sizer = useRef<HTMLDivElement>(null);
  const width = useAvailableWidth(sizer);
  const [heights, refFor, remeasure] = useNaturalHeights();
  const advance = useCanvasAdvance(sizer, remeasure);
  // Geometry sees only authoritative source metadata; an unmatched member falls back to its defaults.
  const sourceMap = useMemo(() => sourcesByMember(board.members, sources), [board.members, sources]);
  const layout = useMemo(() => layoutDashboard(board.layout, width, sourceMap, heights, advance), [board.layout, width, sourceMap, heights, advance]);
  const preorder = useMemo(() => dashboardNodes(board.layout), [board.layout]);
  const nodes = useMemo(() => new Map(preorder.map((node, index) => [node.id, { node, index }] as const)), [preorder]);
  const leafBoxes = useMemo(() => {
    const boxes = new Map<string, DashboardBox>();
    for (const box of layout.boxes) if (box.member !== undefined) boxes.set(box.member, box);
    return boxes;
  }, [layout.boxes]);
  // Geometry emits boxes in postorder; frames paint parent first so a child frame stays visible above it.
  const groupBoxes = useMemo(() => layout.boxes.filter(box => box.member === undefined)
    .sort((a, b) => (nodes.get(a.id)?.index ?? 0) - (nodes.get(b.id)?.index ?? 0)), [layout.boxes, nodes]);
  const root = groupBoxes.find(box => box.id === board.layout.id);
  const editing = onSelect !== undefined;

  return (
    <section className={`dashboard-canvas${editing ? " editing" : ""}`} aria-label={board.title || board.name}
      style={{ ["--dashboard-card-border" as string]: `${CARD_BORDER}px`, ["--dashboard-card-padding" as string]: `${CARD_PADDING}px` }}>
      <div ref={sizer} className="dashboard-sizer" aria-hidden="true" />
      {board.members.length === 0 ? <p className="dashboard-empty">No results on this board.</p> : (
        <div className="dashboard-stage" style={{ width: root?.width ?? width, height: layout.height }}>
          <div className="dashboard-groups">
            {groupBoxes.map(box => {
              const node = nodes.get(box.id)?.node, isRoot = box.id === board.layout.id, isSelected = box.id === selected;
              if (!node || (isRoot && !isSelected)) return null;
              const label = NODE_LABEL[node.kind];
              return (
                <div key={box.id} className={`dashboard-group${isRoot ? " root" : ""}${isSelected ? " selected" : ""}`} style={place(box)}>
                  {editing && !isRoot && (
                    <button type="button" className="dashboard-group-select" aria-pressed={isSelected} aria-label={`Select ${label.toLowerCase()}`}
                      onClick={() => onSelect?.(box.id)}>{label}</button>
                  )}
                </div>
              );
            })}
          </div>
          {board.members.map(member => {
            const box = leafBoxes.get(member.id);
            if (!box) return null;
            const source = sourceMap.get(member.id);
            const contentWidth = Math.max(0, box.width - CARD_CHROME);
            const allocation = { width: contentWidth, columns: Math.max(1, Math.floor(contentWidth / advance)) };
            const rows = maximumRows(source), isSelected = box.id === selected;
            // A live body takes its preferred rows (never past its maximum) and its result fills them.
            const fill = rows !== undefined && source?.live ? Math.min(source.sizing.preferred.rows, rows) : undefined;
            return (
              <article key={member.id} className={`dashboard-card${isSelected ? " selected" : ""}${source ? "" : " missing"}`} style={place(box)} aria-label={member.label}>
                <div ref={refFor(member.id)} className="dashboard-card-content">
                  <header className="dashboard-card-head">
                    <h3 className="dashboard-card-label">{member.label}</h3>
                    {editing && (
                      <button type="button" className="dashboard-card-select" aria-pressed={isSelected} aria-label={`Select ${member.label}`}
                        onClick={() => onSelect?.(box.id)}>Select</button>
                    )}
                  </header>
                  <div className={`dashboard-card-body${rows === undefined ? "" : " bounded"}${fill === undefined ? "" : " stream-fill"}`}
                    style={rows === undefined ? undefined : { ["--dashboard-rows" as string]: rows, ...(fill === undefined ? {} : { height: `${fill}lh` }) }}>
                    {/* The integration host resolves authority and explains an unreadable reference. */}
                    {renderMember(member, allocation)}
                  </div>
                </div>
              </article>
            );
          })}
        </div>
      )}
    </section>
  );
}
