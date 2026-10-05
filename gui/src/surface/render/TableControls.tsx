/**
 * The controls a presented table offers: filter its rows, choose its columns, pin its key column,
 * and show more of its rows under itself. Buttons are labelled controls with names for assistive
 * technology and the hint layer; they carry no text of their own.
 *
 * What the table is arranged by lives in two places, by who would miss it: the filter and the rows
 * shown are this view's (they go with the cell's view); hidden columns, widths and the pin are the
 * row type's, kept in the settings through `tableViewStore`.
 */
import { useEffect, useRef, useState, type ReactNode } from "react";
import { tableViewStore, type TableView } from "../../presentation/table-views";
import type { TableArrangement } from "../../presentation/types";
import { MonoLine } from "../MonoLine";

const Icon = ({ children }: { readonly children: ReactNode }) =>
  <svg className="value-table-icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{children}</svg>;
const FilterIcon = () => <Icon><circle cx="10.5" cy="10.5" r="6.5" /><path d="M15.5 15.5 21 21" /></Icon>;
const ColumnsIcon = () => <Icon><rect x="3" y="4" width="18" height="16" rx="1.5" /><path d="M9 4v16M15 4v16" /></Icon>;
const PinIcon = () => <Icon><path d="M12 17v5M9 3h6l-1 6 3 3H7l3-3z" /></Icon>;

function arranged(arrangement: TableArrangement): TableView {
  return tableViewStore.get()[arrangement.key] ?? {};
}

function keep(arrangement: TableArrangement, change: (view: TableView) => TableView) {
  tableViewStore.change(arrangement.key, change(arranged(arrangement)));
}

/** The key column's pin: pressed while the key column stays put. */
export function PinButton({ arrangement }: { readonly arrangement: TableArrangement }) {
  const pinned = !arrangement.unpinned;
  return <button type="button" className={`value-table-pin${pinned ? " value-table-on" : ""}`} aria-pressed={pinned}
    aria-label={pinned ? "Unpin the key column" : "Pin the key column"} aria-description={pinned ? "unpin: scroll this column with the rest" : "pin: keep this column in place"}
    onClick={() => keep(arrangement, (view) => ({ ...view, unpinned: pinned ? true : false }))}><PinIcon /></button>;
}

/** Filter and column buttons, drawn in the table's own toolbar. */
export function TableToolbar({ path, arrangement, onFilter }: {
  readonly path: string; readonly arrangement: TableArrangement; readonly onFilter?: (path: string, text: string) => void;
}) {
  const [filtering, setFiltering] = useState(arrangement.filter !== undefined);
  const [text, setText] = useState(arrangement.filter?.query ?? "");
  const [menu, setMenu] = useState(false);
  const field = useRef<HTMLInputElement>(null);
  const holder = useRef<HTMLDivElement>(null);
  useEffect(() => { if (filtering) field.current?.focus(); }, [filtering]);
  useEffect(() => {
    if (!menu || typeof document === "undefined") return;
    const away = (event: MouseEvent) => { if (!holder.current?.contains(event.target as Node)) setMenu(false); };
    document.addEventListener("mousedown", away);
    return () => document.removeEventListener("mousedown", away);
  }, [menu]);
  const filter = (next: string) => { setText(next); onFilter?.(path, next); };
  const close = () => { filter(""); setFiltering(false); };
  const hidden = arrangement.hidden;
  const shownCount = arrangement.columns.length - hidden.length;
  const active = filtering || menu || hidden.length > 0;
  return <div ref={holder} className={`value-table-toolbar${active ? " value-table-toolbar-active" : ""}`}
    onKeyDown={(event) => {
      // Escape closes what is open here first; with nothing open it leaves the screen as usual.
      if (event.key !== "Escape" || (!menu && !filtering)) return;
      event.stopPropagation();
      if (menu) setMenu(false); else close();
    }}>
    {filtering
      ? <span className="value-table-filter">
        <FilterIcon />
        <input ref={field} aria-label="Filter rows" value={text} placeholder="filter rows" onChange={(event) => filter(event.target.value)} />
        {arrangement.filter && <span className="mono-faint">{`${arrangement.filter.matched} of ${arrangement.filter.of}`}</span>}
        <button type="button" className="value-table-clear" aria-label="Clear the filter" onClick={close}>×</button>
      </span>
      : onFilter && <button type="button" className="value-table-button" aria-label="Filter rows" aria-description="filter rows" onClick={() => setFiltering(true)}><FilterIcon /></button>}
    <button type="button" className={`value-table-button${hidden.length ? " value-table-on" : ""}`} aria-haspopup="true" aria-expanded={menu}
      aria-label={`Columns, ${shownCount} of ${arrangement.columns.length} shown`} aria-description={`columns · ${shownCount}/${arrangement.columns.length}`}
      onClick={() => setMenu((was) => !was)}><ColumnsIcon /></button>
    {menu && <div className="value-table-menu option" role="menu" aria-label="Columns">
      <span className="screen-label">Columns · this type</span>
      {arrangement.columns.map((name, at) => {
        const isKey = at === 0;
        const on = !hidden.includes(name);
        return <label key={name} className="value-table-menu-row">
          <input type="checkbox" checked={on} disabled={isKey} onChange={() => keep(arrangement, (view) => ({
            ...view, hidden: on ? [...(view.hidden ?? []), name] : (view.hidden ?? []).filter((it) => it !== name),
          }))} />
          <span className={on ? "mono-ink" : "mono-faint"}>{name}</span>
          {isKey && <span className="mono-faint value-table-menu-note">key</span>}
        </label>;
      })}
      <div className="value-table-menu-actions">
        <button type="button" className="mono-ref" onClick={() => keep(arrangement, (view) => ({ ...view, hidden: undefined }))}>show all</button>
        <button type="button" className="mono-ref" onClick={() => keep(arrangement, (view) => ({ ...view, widths: undefined }))}>reset widths</button>
      </div>
    </div>}
  </div>;
}

/** Sets one column's width in display columns for every table of this row type. */
export function resizeColumn(arrangement: TableArrangement, name: string, columns: number) {
  keep(arrangement, (view) => ({ ...view, widths: { ...(view.widths ?? {}), [name]: columns } }));
}

/** `+n rows · show k more` under its own table; each press adds the table's step. */
export function MoreRows({ path, shown, left, exact, step, onShowRows }: {
  readonly path: string; readonly shown: number; readonly left: number; readonly exact: boolean; readonly step: number;
  readonly onShowRows?: (path: string, rows: number) => void;
}) {
  const next = Math.min(step, left);
  const count = `${exact ? "+" : "+≥"}${left} ${left === 1 ? "row" : "rows"}`;
  if (!onShowRows) return <MonoLine segments={[{ text: count, role: "mono-faint" }]} className="value-table-more" />;
  return <button type="button" className="value-table-more" aria-label={`${count}, show ${next} more`} onClick={() => onShowRows(path, shown + next)}>
    <MonoLine segments={[{ text: `${count} · `, role: "mono-faint" }, { text: `show ${next} more`, role: "mono-ref" }]} />
  </button>;
}
