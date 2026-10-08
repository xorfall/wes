import { budget } from "../limits/policy";
/**
 * The presentation tree: what tier 2 decides a value looks like, and nothing about drawing it.
 *
 * `present()` turns a prepared value, the node's facts and a measured context into one tree of
 * typed nodes. Renderers (tier 3) draw each kind and never look at `TypeShape`; they get runs of
 * text with a tone, never a role, so the design system stays theirs. Every node carries `more` —
 * what the budget left out — and `offers`, the other ways it can be read. Nothing here truncates on
 * the renderer's behalf: what is in the tree is exactly what fits the context.
 */

/** Where the value is being shown. The same tree, three budgets. */
export type Mode = "preview" | "expanded" | "window";

/** Rows per page wherever a list is paged: the expanded viewport and the result window. */
export function pageSize():number { return budget("ui.presentation.page"); }
/** Lines a preview may spend on blocks; the tail line is not one of them. */
export const PREVIEW_LINES = 6;
/** Lines the expanded viewport may hold before its own scroll takes over. */
export function expanded_lines():number { return budget("ui.presentation.lines"); }
/** Lines the window may hold per page. Finite, like every budget. */
export function window_lines():number { return budget("ui.presentation.window"); }
/** Nesting opened by default when expanded; deeper records and lists stay `▸`. */
export const EXPANDED_DEPTH = 3;
/** Nesting opened in preview. */
export const PREVIEW_DEPTH = 2;

/** What tier 3 measured and what the person chose. */
export interface Context {
  readonly mode: Mode;
  /** Display columns that fit the block, measured from the mono advance. */
  readonly columns: number;
  /** Lines available. Always finite. */
  readonly lines: number;
  readonly density: "normal" | "dense";
  readonly locale: string;
  readonly timeZone: string;
  /** Rows of each list asked for so far (`show 50 more` raises it by a page). */
  readonly rows?: number;
  /** Explicit page index for nested lists; pages stay bounded and independent. */
  readonly pages?: ReadonlyMap<string, number>;
  /** Paths (`a/b`) the person opened or closed by hand, overriding the depth budget. */
  readonly open?: ReadonlySet<string>;
  readonly closed?: ReadonlySet<string>;
  /** Rows a table at a path was asked to show by hand, past its budget (`show 20 more`). */
  readonly shown?: ReadonlyMap<string, number>;
  /** Text a table at a path is filtered by; its rows are matched before they are paged. */
  readonly filters?: ReadonlyMap<string, string>;
  /** How each row type's tables are arranged: hidden columns, widths, the key column's pin. */
  readonly tables?: import("./table-views").TableViews;
  readonly sorts?: ReadonlyMap<string,{readonly column:string;readonly descending:boolean}>;
}

/** Contract metadata and the declaration path where a nested tree starts, for its declared tones. */
export interface Declared { readonly meta: import("../value-meta").ValueMeta; readonly at: string }

/** The colour a run means. Renderers map tones to design-system roles. */
export type Tone = "ink" | "dim" | "faint" | "literal" | "param" | "ref" | "ok" | "warn" | "bad" | "meta" | "provider";

export interface Run {
  readonly text: string;
  readonly tone: Tone;
}

/** What the budget left out, each count kept apart. */
export interface More {
  readonly rows?: number;
  /** Explicit page index for nested lists; pages stay bounded and independent. */
  readonly pages?: ReadonlyMap<string, number>;
  readonly fields?: number;
  readonly lines?: number;
  readonly items?: number;
  readonly headers?: number;
  /** Display columns a single-line value lost to its line; the value opens whole in place. */
  readonly chars?: number;
  /** Columns a table dropped, named. */
  readonly columns?: readonly string[];
  /** Whether the counts are totals of a value read whole. Partial reads never state a total. */
  readonly exact: boolean;
}

/** Other ways to read this node. An offer grants nothing and runs nothing by itself. */
export type OfferName = "http" | "source" | "follow" | "copy" | "trace" | (string & {});

export interface ChartBinding {
  readonly as: "line" | "spark";
  readonly x?: string;
  readonly y: string;
}

/**
 * What the one disclosure control before a node does: `opens` draws `▸` and opens the node whole
 * in place; `folds` draws `▾` and folds it back. Tier 2 sets it; the renderer only draws it, in the
 * value column, never in the name column.
 */
export type Disclosure = "opens" | "folds";

interface Base {
  /** Stable address of this node inside the value (`body/value`), for open/close by hand. */
  readonly path: string;
  readonly more?: More;
  readonly offers?: readonly OfferName[];
  readonly disclosure?: Disclosure;
  readonly pagination?: { readonly offset: number; readonly shown: number; readonly total: number };
}

export interface FieldRow {
  /** The field's name, or empty for a row that is its node alone (packed scalars). */
  readonly name: string;
  readonly node: PresentationNode;
}

export interface Column {
  readonly name: string;
  /** Display columns this column is drawn in, header included. */
  readonly width: number;
  /** Right-aligned numbers read down the column. */
  readonly numeric: boolean;
  /** The first shown column: the one a row is known by, drawn in the `table-key` role. */
  readonly key: boolean;
  /** The header as drawn: the name, cut to the column's width when the person narrowed it. */
  readonly label: string;
  /** Whether the person set this width by hand. */
  readonly sized: boolean;
}

/** What a table offers to be arranged by hand, and how it currently is. */
export interface TableArrangement {
  /** The key its arrangement is kept under (`type:<name>`, else `columns:<names>`). */
  readonly key: string;
  /** Every column the table could show, in order; the first is the key column. */
  readonly columns: readonly string[];
  /** Columns the person hid. */
  readonly hidden: readonly string[];
  /** Whether the key column scrolls with the rest. */
  readonly unpinned: boolean;
  /** Rows one `show more` adds. */
  readonly step: number;
  /** The filter in force, with the rows it kept out of all rows. */
  readonly filter?: { readonly query: string; readonly matched: number; readonly of: number };
}

export type PresentationNode =
  | (Base & { readonly kind: "line"; readonly runs: readonly Run[] })
  | (Base & { readonly kind: "fields"; readonly rows: readonly FieldRow[]; readonly nameWidth: number; readonly tree?: {readonly type: import("../protocol").TypeShape; readonly data: unknown; readonly mode: Mode; readonly declared?: Declared} })
  | (Base & {
      readonly kind: "table"; readonly columns: readonly Column[];
      /** One entry per row, one entry per shown column: the cell's runs, already cut to the column's width. */
      readonly rows: readonly (readonly (readonly Run[])[])[];
      /** Per cell, a declared style; per row, a declared background tint. Absent when none is declared. */
      readonly styles?: readonly (readonly ("badge" | undefined)[])[];
      readonly tints?: readonly (Tone | undefined)[];
      /** Structural cell disclosures; the compact runs above remain the table summary. */
      readonly details?: readonly (readonly (PresentationNode | undefined)[])[];
      readonly offset: number; readonly total: number;
      readonly inspection?: {readonly type: import("../protocol").TypeShape; readonly mode:Mode; readonly whole:boolean; readonly declared?: Declared; readonly rows:readonly {readonly index:number;readonly value:unknown}[]};
      readonly arrangement: TableArrangement;
      readonly sort?:{readonly column:string;readonly descending:boolean};
    })
  | (Base & { readonly kind: "items"; readonly items: readonly Run[]; /** One line per element, in the expanded and window modes; absent in the packed preview. */ readonly lines?: readonly Run[][] })
  | (Base & { readonly kind: "text"; readonly lines: readonly (readonly Run[])[]; readonly newline: boolean })
  | (Base & { readonly kind: "bytes"; readonly size: number; readonly note: string })
  | (Base & { readonly kind: "empty"; readonly text: "empty" | "no items" | "none" | "null" })
  | (Base & { readonly kind: "process"; readonly exit?: number; readonly stdout: PresentationNode; readonly stderr: PresentationNode })
  | (Base & {
      readonly kind: "view"; readonly view: string; readonly model: unknown;
      readonly waiting?: string;
      readonly children: readonly PresentationNode[]; readonly ownLines: number;
      readonly summary: readonly Run[];
      readonly fallback: { readonly viewModules?: readonly import("../value-views/contract").ValueViewModule[]; readonly type: import("../protocol").TypeShape; readonly data: unknown; readonly context: Context };
    })
  | (Base & { readonly kind: "error"; readonly code: string; readonly message: string; readonly span?: string })
  | (Base & { readonly kind: "notice"; readonly lines: readonly string[]; readonly severity: "info" | "warning" })
  | (Base & { readonly kind: "stream"; readonly live: boolean; readonly body?: PresentationNode; readonly note: string })
  | (Base & { readonly kind: "custom"; readonly name: string; readonly data: unknown })
  /** A nested record or list: its summary line, and its body under it when open. */
  | (Base & { readonly kind: "nested"; readonly summary: string; readonly body?: PresentationNode });

export type Kind = PresentationNode["kind"];

/** The node's facts that the presentation may use: never a hint about how to display. */
export interface Facts {
  readonly state?: "ready" | "running" | "failed" | "stale" | "skipped" | "cancelled" | "planned";
  readonly stopped?: boolean;
  /** Whether the value was read whole (true) or a window of it (false). */
  readonly whole?: boolean;
}

/** One line the verdict can say about the value: `exit 1`, `HTTP 404`, `213`. */
export interface Summary {
  readonly facts: readonly Run[];
}
