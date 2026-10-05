/**
 * The design system's table (`uid.table`) drawn as a semantic table: a header row on the sunk ground,
 * thin lines between cells, the key column in the `table-key` role, numeric columns right-aligned,
 * the row under the pointer on the selection tint. Cells are drawn as given: what fits was decided
 * by tier 2, so nothing is cut or padded here.
 *
 * Every column is drawn. A table wider than its block scrolls sideways inside its own frame; the
 * key column stays put (unless unpinned) so a row keeps its identity while scrolling, and the frame
 * fades the edge that has more behind it, since the scrollbar itself may be hidden until touched.
 *
 * A column dragged by its header edge is drawn at the dragged width while the pointer moves; on
 * release the width is handed back as display columns, and `present()` re-cuts the cells to it.
 */
import { Fragment, type PointerEvent, type ReactNode, useLayoutEffect, useRef, useState } from "react";
import type { Segment } from "../MonoLine";
import { monoAdvance } from "./measure";

export interface GridColumn {
  readonly name: string;
  readonly numeric?: boolean;
  readonly key?: boolean;
  readonly width?: number;
}

export interface TableGridProps {
  readonly rowKeys?: readonly string[];
  readonly columns: readonly GridColumn[];
  /** One entry per row, one entry per column. */
  readonly rows: readonly (readonly (readonly Segment[])[])[];
  /** Rows the value has in all, for assistive technology; the rows drawn are a page of it. */
  readonly total?: number;
  readonly onSelect?: (row: number) => void;
  readonly selected?: number;
  readonly sort?:{readonly column:string;readonly descending:boolean};
  readonly onSort?: (column: number) => void;
  readonly renderCell?: (row: number, column: number, summary: ReactNode) => ReactNode;
  readonly details?: readonly ReactNode[];
  /** Header labels as drawn, when they differ from the names (a narrowed column). */
  readonly labels?: readonly string[];
  /** Controls in the toolbar above the scrolling header. */
  readonly toolbar?: ReactNode;
  /** Whether the key column stays put while the rest scrolls. */
  readonly pinned?: boolean;
  /** Drawn at the start of a header cell (the key column's pin). */
  readonly headerStart?: (column: number) => ReactNode;
  /** A column was dragged to this many display columns. */
  readonly onResize?: (column: number, columns: number) => void;
  /** Drawn under the frame, at the table's own indent. */
  readonly footer?: ReactNode;
}

/** Horizontal padding plus the right line of one cell, in pixels: what a width in columns adds. */
const CELL_CHROME = 17;

/** Display columns a column dragged to `pixels` wide holds. */
export function columnsFor(pixels: number, advance = monoAdvance()): number {
  return Math.max(1, Math.round((pixels - CELL_CHROME) / advance));
}

function cellClass(column: GridColumn | undefined, pinned: boolean): string {
  return `value-table-cell${column?.numeric ? " value-table-numeric" : ""}${column?.key ? ` table-key${pinned ? " value-table-key" : ""}` : ""}`;
}

function cellText(segments: readonly Segment[], key: boolean) {
  // The key column's text takes the column's role; every other cell keeps its runs' roles.
  return key
    ? segments.map((segment) => segment.text).join("")
    : segments.map((segment, at) => <span key={at} className={segment.role ?? "mono-ink"}>{segment.text}</span>);
}

/** Which edges of a scrolling box have content beyond them: "", "left", "right" or "left right". */
export function beyond(scrollLeft: number, scrollWidth: number, clientWidth: number): string {
  if (clientWidth <= 0 || scrollWidth <= clientWidth) return "";
  const edges: string[] = [];
  if (scrollLeft > 0) edges.push("left");
  if (scrollLeft + clientWidth < scrollWidth - 1) edges.push("right");
  return edges.join(" ");
}

/** The edges with more behind them, kept current as the box scrolls or resizes. */
function useBeyond(ref: React.RefObject<HTMLDivElement | null>): string {
  const [edges, setEdges] = useState("");
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measure = () => { setEdges(beyond(element.scrollLeft, element.scrollWidth, element.clientWidth)); element.style?.setProperty("--table-visible-width", `${element.clientWidth}px`); };
    measure();
    element.addEventListener("scroll", measure, { passive: true });
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(measure);
    observer?.observe(element);
    return () => { element.removeEventListener("scroll", measure); observer?.disconnect(); };
  });
  return edges;
}

export function TableGrid({ rowKeys, columns, rows, total, renderCell, details, labels, toolbar, pinned = true, headerStart, onResize, footer, onSelect, selected, onSort, sort }: TableGridProps) {
  const box = useRef<HTMLDivElement>(null);
  const elements=useRef(new Map<string,HTMLTableRowElement>()),userScroll=useRef(false),follow=useRef(true),anchor=useRef<{key:string;offset:number}>();
  const [anchorNotice,setAnchorNotice]=useState<string>();
  const captureAnchor=()=>{
    const viewport=box.current;if(!viewport || !rowKeys)return;
    const top=viewport.getBoundingClientRect().top;
    for(const key of rowKeys){const el=elements.current.get(key);if(el && el.getBoundingClientRect().bottom>top){anchor.current={key,offset:el.getBoundingClientRect().top-top};break;}}
  };
  useLayoutEffect(()=>{
    const viewport=box.current;if(!viewport || !rowKeys)return;
    if(follow.current){viewport.scrollTop=viewport.scrollHeight;return;}
    const at=anchor.current;if(!at)return;const el=elements.current.get(at.key);
    if(el)viewport.scrollTop+=el.getBoundingClientRect().top-viewport.getBoundingClientRect().top-at.offset;
    else {viewport.scrollTop=0;setAnchorNotice("The anchored row left the source window; showing the oldest available row.");}
    captureAnchor();
  },[rowKeys]);
  const edges = useBeyond(box);
  const [drag, setDrag] = useState<{ readonly column: number; readonly pixels: number }>();
  const grip = (column: number) => onResize && <span className="value-table-grip" aria-hidden="true" onPointerDown={(event: PointerEvent<HTMLSpanElement>) => {
    const cell = event.currentTarget.parentElement;
    if (!cell || event.button !== 0) return;
    event.preventDefault(); event.stopPropagation();
    const from = event.clientX;
    const start = cell.getBoundingClientRect().width;
    const target = event.currentTarget;
    target.setPointerCapture?.(event.pointerId);
    let pixels = start;
    const move = (e: globalThis.PointerEvent) => { pixels = Math.max(24, start + e.clientX - from); setDrag({ column, pixels }); };
    const up = () => {
      target.removeEventListener("pointermove", move); target.removeEventListener("pointerup", up); target.removeEventListener("pointercancel", up);
      setDrag(undefined);
      if (Math.abs(pixels - start) >= 2) onResize(column, columnsFor(pixels));
    };
    target.addEventListener("pointermove", move); target.addEventListener("pointerup", up); target.addEventListener("pointercancel", up);
  }} />;
  return <div className="value-table-block">
    {toolbar && <div className="value-table-tools">{toolbar}{rowKeys && <button className="cell-action" aria-label="Follow newest table rows" onClick={()=>{follow.current=true;anchor.current=undefined;setAnchorNotice(undefined);if(box.current)box.current.scrollTop=box.current.scrollHeight;}}>↓ newest</button>}</div>}
    {anchorNotice && <p className="mono-warn" role="status">{anchorNotice}</p>}
    <div className="value-table-frame" data-beyond={edges}>
      <div ref={box} className="value-table-scroll" tabIndex={0} aria-label="Table rows" onPointerDown={()=>{userScroll.current=true;}} onTouchStart={()=>{userScroll.current=true;}} onKeyDown={event=>{if(["ArrowUp","ArrowDown","PageUp","PageDown","Home","End"," "].includes(event.key))userScroll.current=true;}} onWheel={event=>{if(event.deltaY)userScroll.current=true;if(rowKeys && event.deltaY<0){follow.current=false;captureAnchor();}}} onScroll={()=>{if(follow.current && !userScroll.current)return;userScroll.current=false;if(rowKeys && box.current && box.current.scrollTop+box.current.clientHeight<box.current.scrollHeight-2){follow.current=false;captureAnchor();}}} style={{["--table-visible-width" as string]:"100%"}}>
        <table role="table" className="value-table" aria-rowcount={total ?? rows.length}>
          <thead><tr role="row" className="value-table-head">
            {columns.map((column, at) => <th role="columnheader" key={column.name} className={`${cellClass(column,pinned)}${onResize ? " value-table-sizable" : ""}`} scope="col" aria-sort={sort?.column===column.name ? sort.descending ? "descending" : "ascending" : undefined}
              style={{width:drag?.column === at ? drag.pixels : column.width ? `${column.width}ch` : undefined}}>
              {headerStart?.(at)}{onSort ? <button className="table-sort" onClick={()=>onSort(at)}>{labels?.[at] ?? column.name}{sort?.column===column.name ? sort.descending ? " ↓" : " ↑" : ""}</button> : labels?.[at] ?? column.name}{grip(at)}
            </th>)}
          </tr></thead>
          <tbody>{rows.map((row, at) => <Fragment key={rowKeys?.[at] ?? at}><tr data-item-key={rowKeys?.[at]} ref={el=>{const key=rowKeys?.[at];if(key){if(el)elements.current.set(key,el);else elements.current.delete(key);}}} role="row" className={`value-table-row${selected===at ? " table-row-selected" : ""}`} onClick={()=>onSelect?.(at)}>
            {row.map((cell,index)=><td role="cell" key={index} className={cellClass(columns[index],pinned)}>{renderCell ? renderCell(at,index,cellText(cell,columns[index]?.key ?? false)) : cellText(cell,columns[index]?.key ?? false)}</td>)}
          </tr>{details?.[at] && <tr role="row" className="value-table-detail-row"><td role="cell" colSpan={Math.max(1,columns.length)} className="value-table-detail"><div className="table-detail-width">{details[at]}</div></td></tr>}</Fragment>)}</tbody>
        </table>
      </div>
    </div>
    {footer}
  </div>;

}
