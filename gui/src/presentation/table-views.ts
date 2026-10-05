import { budget } from "../limits/policy";
/**
 * How a person has arranged the tables of one row type: which columns they hid, how wide they made
 * them, whether the key column stays put. A client preference like the palette, so it lives in
 * `Settings.tables` and is persisted by the session window; every window reads it from this store.
 *
 * `present()` takes the arrangement as an input, so a width is still a width budget and the `…` is
 * still `ellipsizeEnd`'s: nothing here clips text.
 */
import type { TypeShape } from "../protocol";

export interface TableView {
  /** Columns the person hid. The key column is never among them. */
  readonly hidden?: readonly string[];
  /** Display columns a column was dragged to. */
  readonly widths?: Readonly<Record<string, number>>;
  /** The key column scrolls with the rest instead of staying put. */
  readonly unpinned?: boolean;
}
export type TableViews = Readonly<Record<string, TableView>>;

/** Narrowest and widest a dragged column may be, in display columns. */
export const MIN_WIDTH = 3;
export const MAX_WIDTH = 400;
/** Types remembered at most; the oldest arrangement goes first. */
function max_tables():number { return budget("ui.table.saved"); }
function max_names():number { return budget("ui.table.columns"); }
const MAX_NAME_LENGTH = 200;

const isName = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= MAX_NAME_LENGTH;

/** The identity a table's arrangement is kept under: its row type's name, else its column set. */
export function tableKey(element: TypeShape, columns: readonly string[]): string {
  return element.kind === "record" && element.name
    ? `type:${element.name}`
    : `columns:${[...columns].sort().join(",")}`;
}

export function clampWidth(width: number): number {
  return Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, Math.round(width)));
}

/** A stored arrangement read back: unknown keys dropped, sizes bounded, empty entries forgotten. */
export function restoreTableViews(value: unknown): TableViews {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return {};
  const out: Record<string, TableView> = {};
  for (const [key, raw] of Object.entries(value).slice(-max_tables())) {
    if (!isName(key) || typeof raw !== "object" || raw === null || Array.isArray(raw)) continue;
    const view = normalized(raw as Record<string, unknown>);
    if (view) out[key] = view;
  }
  return out;
}

function normalized(raw: Record<string, unknown>): TableView | undefined {
  const hidden = Array.isArray(raw.hidden) ? [...new Set(raw.hidden.filter(isName))].slice(0, max_names()) : [];
  const widths: Record<string, number> = {};
  if (typeof raw.widths === "object" && raw.widths !== null && !Array.isArray(raw.widths)) {
    for (const [name, width] of Object.entries(raw.widths).slice(0, max_names())) {
      if (isName(name) && typeof width === "number" && Number.isFinite(width)) widths[name] = clampWidth(width);
    }
  }
  const view: TableView = {
    ...(hidden.length ? { hidden } : {}),
    ...(Object.keys(widths).length ? { widths } : {}),
    ...(typeof raw.unpinned === "boolean" ? { unpinned: raw.unpinned } : {}),
  };
  return Object.keys(view).length ? view : undefined;
}

/** One table's arrangement replaced; an arrangement back to the defaults is forgotten. */
export function withTableView(views: TableViews, key: string, view: TableView): TableViews {
  const { [key]: _was, ...rest } = views;
  const next = normalized(view as Record<string, unknown>);
  return next ? { ...rest, [key]: next } : rest;
}

type Writer = (views: TableViews) => void;
let current: TableViews = {};
let writer: Writer | undefined;
const listeners = new Set<() => void>();

/**
 * The arrangements in force for this window. The window that owns the settings seeds it and
 * says how a change is kept; a table only reads it and asks for changes.
 */
export const tableViewStore = {
  get(): TableViews {
    return current;
  },
  subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  },
  /** The owner's settings changed (its own write, another window's, a reload). */
  seed(views: TableViews) {
    if (views === current) return;
    current = views;
    for (const listener of listeners) listener();
  },
  /** How this window keeps a change: the session persists it, other windows announce it. */
  keepWith(next: Writer | undefined) {
    writer = next;
  },
  change(key: string, view: TableView) {
    const next = withTableView(current, key, view);
    this.seed(next);
    writer?.(next);
  },
};
