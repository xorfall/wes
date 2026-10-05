/**
 * `/settings limits` — the operating budgets the parent supplies, in one list.
 *
 * The panel is presentation only. It does not discover budgets or check that the list is complete:
 * `editable`, `min`, `max`, `restart` and `reason` are taken from the parent as given. Saved values
 * are app-wide, shared by every workspace, and a `restart` entry applies from the next launch.
 *
 * A budget is a number with a unit. The panel shows what is running now, what the next launch will
 * use when the two differ, and the default, so a change can be judged before it is made. Editable
 * entries take an exact value in a unit the person picks; the panel works in base units (bytes,
 * milliseconds, counts) and refuses anything that does not land on a whole base unit rather than
 * rounding it. Changing the unit rewrites the same base value exactly — `64 KiB` is `0.0625 MiB` —
 * or keeps text and unit as they are and says why when the new unit has no finite decimal for it.
 * An entry the parent marks fixed shows its reason instead of a control that does nothing.
 *
 * The parent owns the values. The panel holds only drafts: they survive a refresh and a failed
 * save, are flagged when the saved value moved underneath them, and are cleared only by an explicit
 * reload, a revert, or a save the parent confirmed by resolving. While the parent is reading or busy,
 * or a save is out, nothing can be edited, reverted, reset or reloaded.
 */
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import type { LimitEntry } from "./model";
import { composing } from "../platform-keys";
import "./limits.css";

export interface LimitsPanelProps {
  readonly entries: readonly LimitEntry[];
  /** The entries are being read; actions wait for them. */
  readonly loading?: boolean;
  /** Why the entries could not be read or written, said by the parent. */
  readonly problem?: string;
  /** The parent is doing something with the limits; actions wait for it. */
  readonly busy?: boolean;
  /** Receives only changed, valid values in base units. Rejecting keeps every draft. */
  readonly onSave: (values: Readonly<Record<string, number>>) => void | Promise<void>;
  /** Read the limits again. The panel discards its drafts first, because the person asked for it. */
  readonly onReload: () => void;
}

/** One way of writing a limit's number: the name shown beside it and how many base units it is. */
export interface LimitUnit {
  readonly name: string;
  readonly factor: number;
}

const KIB = 1024;
const SECOND_MS = 1000;
const MINUTE_MS = 60 * SECOND_MS;
/** How many decimal places a value may show before the panel writes it in a smaller unit. */
const DISPLAY_PLACES = 3;
/**
 * How many decimal places a unit change may write. The largest power-of-two factor, GiB, is 2^30,
 * and any whole number of bytes divided by it ends within 30 places, so no byte value is refused.
 */
const EXACT_PLACES = 30;
const ALL_GROUPS = "";
const FALLBACK_REASON = "This budget cannot be changed here.";
const UNIT_NEEDS_VALUE = "Correct the value before changing its unit.";
const unitHasNoExact = (from: LimitUnit, to: LimitUnit) =>
  `This value has no exact decimal in ${to.name}; it stays in ${from.name}.`;
const NEXT_LAUNCH = "Saved budgets apply the next time WesDesk starts.";
const SCOPE_NOTICE = "Budgets are saved for the app and shared by every workspace. "
  + "Lowering a history budget does not delete saved journals, but can prevent recovering a workspace larger than it.";

/** The units each kind of limit can be written in, smallest (the stored base unit) first. */
export const LIMIT_UNITS: Readonly<Record<LimitEntry["unit"], readonly LimitUnit[]>> = {
  count: [{ name: "", factor: 1 }],
  bytes: [
    { name: "B", factor: 1 },
    { name: "KiB", factor: KIB },
    { name: "MiB", factor: KIB ** 2 },
    { name: "GiB", factor: KIB ** 3 },
  ],
  milliseconds: [
    { name: "ms", factor: 1 },
    { name: "s", factor: SECOND_MS },
    { name: "min", factor: MINUTE_MS },
  ],
};

const BASE_NAMES: Readonly<Record<LimitEntry["unit"], string>> = {
  count: "a whole number",
  bytes: "a whole number of bytes",
  milliseconds: "a whole number of milliseconds",
};

/** A contract value this client does not know is drawn as a plain count, never guessed at. */
export function unitsFor(kind: LimitEntry["unit"]): readonly LimitUnit[] {
  return LIMIT_UNITS[kind] ?? LIMIT_UNITS.count;
}

/** Exactly `value / factor` with at most `places` decimals, or nothing when that is not exact. */
export function exactDecimal(value: number, factor: number, places = DISPLAY_PLACES): string | undefined {
  if (!Number.isSafeInteger(value) || !Number.isSafeInteger(factor) || factor <= 0) return undefined;
  const divisor = BigInt(factor);
  const negative = value < 0;
  const magnitude = BigInt(Math.abs(value));
  let remainder = magnitude % divisor;
  let digits = "";
  while (remainder !== 0n && digits.length < places) {
    remainder *= 10n;
    digits += (remainder / divisor).toString();
    remainder %= divisor;
  }
  if (remainder !== 0n) return undefined;
  return `${negative ? "-" : ""}${magnitude / divisor}${digits ? `.${digits}` : ""}`;
}

/** The largest unit that writes the value exactly and at least as one whole unit. */
export function bestUnit(value: number, units: readonly LimitUnit[]): LimitUnit {
  for (let at = units.length - 1; at > 0; at -= 1) {
    const unit = units[at]!;
    if (Math.abs(value) >= unit.factor && exactDecimal(value, unit.factor) !== undefined) return unit;
  }
  return units[0]!;
}

const groupDigits = (text: string) => text.replace(/^(-?)(\d+)/, (_, sign: string, whole: string) =>
  sign + whole.replace(/\B(?=(\d{3})+(?!\d))/g, ","));

/** A limit as read: grouped digits in its best exact unit, `64 KiB`, `15 s`, `4,000`. */
export function formatLimit(value: number, kind: LimitEntry["unit"]): string {
  const unit = bestUnit(value, unitsFor(kind));
  const text = exactDecimal(value, unit.factor) ?? String(value);
  return `${groupDigits(text)}${unit.name ? ` ${unit.name}` : ""}`;
}

export type ParsedLimit =
  | { readonly ok: true; readonly value: number }
  | { readonly ok: false; readonly message: string };

const DECIMAL = /^(-?)(\d+)(?:\.(\d+))?$/;
/** Room for every safe integer plus an exact 30-place fraction; anything longer is refused unread. */
const MAX_INPUT_LENGTH = 64;
const refuse = (message: string): ParsedLimit => ({ ok: false, message });

/**
 * A typed value in base units, exactly, or why not.
 *
 * Decimal text is read as a rational number, multiplied by the unit and kept only if it lands on a
 * whole, safe base unit inside the entry's bounds: `1.5` MiB is 1,572,864 bytes, `1.3` bytes is
 * refused. Nothing is rounded or clamped, so what is saved is what was typed.
 */
export function parseLimit(text: string, unit: LimitUnit,
  entry: Pick<LimitEntry, "min" | "max" | "unit">): ParsedLimit {
  const trimmed = text.trim();
  if (trimmed === "") return refuse("Enter a value.");
  if (trimmed.length > MAX_INPUT_LENGTH) return refuse("Value is too long to store exactly.");
  const match = DECIMAL.exec(trimmed);
  if (!match) return refuse("Use digits with an optional decimal point.");
  const [, sign = "", whole = "", fraction = ""] = match;
  const scale = 10n ** BigInt(fraction.length);
  const scaled = BigInt(whole + fraction) * BigInt(unit.factor);
  if (scaled % scale !== 0n) return refuse(`Must be ${BASE_NAMES[entry.unit] ?? BASE_NAMES.count}.`);
  const magnitude = scaled / scale;
  if (magnitude > BigInt(Number.MAX_SAFE_INTEGER)) return refuse("Too large to store exactly.");
  const value = sign && magnitude !== 0n ? -Number(magnitude) : Number(magnitude);
  if (value < entry.min) return refuse(`Must be at least ${formatLimit(entry.min, entry.unit)}.`);
  if (value > entry.max) return refuse(`Must be at most ${formatLimit(entry.max, entry.unit)}.`);
  return { ok: true, value };
}

/** A row's unsaved text, its unit, and the saved value it was started from. */
interface Draft {
  readonly text: string;
  readonly unit: string;
  readonly baseline: number;
}

/** Why a unit change was refused, shown while the row still reads `text` in `unit`. */
interface UnitNote {
  readonly text: string;
  readonly unit: string;
  readonly message: string;
}

interface Row {
  readonly entry: LimitEntry;
  readonly units: readonly LimitUnit[];
  readonly unit: LimitUnit;
  readonly text: string;
  readonly draft?: Draft;
  readonly parsed?: ParsedLimit;
  /** The draft differs from the saved value, or cannot be read as one. */
  readonly dirty: boolean;
  readonly invalid: boolean;
  /** The saved value changed after this draft was started. */
  readonly moved: boolean;
  /** What the row would save: the draft's value when it has a valid one, the saved value otherwise. */
  readonly effective?: number;
  readonly pending: boolean;
}

function rowOf(entry: LimitEntry, draft: Draft | undefined): Row {
  const units = unitsFor(entry.unit);
  const pending = entry.active !== entry.saved;
  if (!entry.editable || draft === undefined) {
    const unit = bestUnit(entry.saved, units);
    return { entry, units, unit, text: exactDecimal(entry.saved, unit.factor) ?? String(entry.saved),
      dirty: false, invalid: false, moved: false, effective: entry.saved, pending };
  }
  const unit = units.find((it) => it.name === draft.unit) ?? units[0]!;
  const parsed = parseLimit(draft.text, unit, entry);
  return {
    entry, units, unit, text: draft.text, draft, parsed,
    dirty: !parsed.ok || parsed.value !== entry.saved,
    invalid: !parsed.ok,
    moved: draft.baseline !== entry.saved,
    ...(parsed.ok ? { effective: parsed.value } : {}),
    pending,
  };
}

const failureText = (error: unknown) =>
  error instanceof Error && error.message ? error.message : typeof error === "string" && error ? error : "The budgets could not be saved.";

const plural = (count: number, one: string, many = `${one}s`) => `${count} ${count === 1 ? one : many}`;

const without = <T,>(map: ReadonlyMap<string, T>, key: string): ReadonlyMap<string, T> => {
  if (!map.has(key)) return map;
  const next = new Map(map);
  next.delete(key);
  return next;
};

/** The unified limits tab: search, a category filter, and one row per limit. */
export function LimitsPanel({ entries, loading = false, problem, busy = false, onSave, onReload }: LimitsPanelProps) {
  const id = useId();
  const [drafts, setDrafts] = useState<ReadonlyMap<string, Draft>>(() => new Map());
  const [unitNotes, setUnitNotes] = useState<ReadonlyMap<string, UnitNote>>(() => new Map());
  const [query, setQuery] = useState("");
  const [group, setGroup] = useState(ALL_GROUPS);
  const [changedOnly, setChangedOnly] = useState(false);
  const [saving, setSaving] = useState(false);
  const [outcome, setOutcome] = useState<{ readonly ok: boolean; readonly text: string }>();
  const alive = useRef(true);
  // A second click lands before `saving` renders; this refuses it without waiting for state.
  const inFlight = useRef(false);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);

  const rows = useMemo(() => entries.map((entry) => rowOf(entry, drafts.get(entry.id))), [entries, drafts]);
  const groups = useMemo(() => [...new Set(entries.map((entry) => entry.group))], [entries]);
  const chosenGroup = groups.includes(group) ? group : ALL_GROUPS;
  const needle = query.trim().toLowerCase();
  const visible = rows.filter(({ entry, dirty, pending }) =>
    (chosenGroup === ALL_GROUPS || entry.group === chosenGroup)
    && (!changedOnly || dirty || pending || entry.saved !== entry.default)
    && (needle === "" || [entry.label, entry.description, entry.id].some((it) => it.toLowerCase().includes(needle))));

  const dirtyRows = rows.filter((row) => row.entry.editable && row.dirty);
  const invalidCount = dirtyRows.filter((row) => row.invalid).length;
  const restartCount = dirtyRows.filter((row) => row.entry.restart).length;
  const waiting = rows.filter((row) => row.pending).length;
  const locked = loading || busy || saving;
  const resettable = visible.filter((row) => row.entry.editable && row.effective !== row.entry.default);

  const edit = (entry: LimitEntry, text: string, unit: string) => {
    setOutcome(undefined);
    setUnitNotes((previous) => without(previous, entry.id));
    setDrafts((previous) => new Map(previous).set(entry.id,
      { text, unit, baseline: previous.get(entry.id)?.baseline ?? entry.saved }));
  };
  const revert = (entryId: string) => {
    setOutcome(undefined);
    setUnitNotes((previous) => without(previous, entryId));
    setDrafts((previous) => without(previous, entryId));
  };
  // A unit change rewrites the same base value in the new unit, with as many decimals as that
  // takes. It never rounds, and it never reinterprets the typed number: when the value is invalid
  // or has no finite decimal in the new unit, the text and unit stay as they are and the row says why.
  const changeUnit = (row: Row, unitName: string) => {
    if (locked) return;
    const next = row.units.find((it) => it.name === unitName);
    if (next === undefined || next === row.unit) return;
    const value = row.draft === undefined ? row.entry.saved : row.parsed?.ok ? row.parsed.value : undefined;
    const text = value === undefined ? undefined : exactDecimal(value, next.factor, EXACT_PLACES);
    if (text !== undefined) { edit(row.entry, text, next.name); return; }
    setOutcome(undefined);
    setUnitNotes((previous) => new Map(previous).set(row.entry.id, {
      text: row.text, unit: row.unit.name,
      message: value === undefined ? UNIT_NEEDS_VALUE : unitHasNoExact(row.unit, next),
    }));
  };
  const resetToDefault = (row: Row) => {
    if (row.entry.default === row.entry.saved) { revert(row.entry.id); return; }
    const unit = bestUnit(row.entry.default, row.units);
    edit(row.entry, exactDecimal(row.entry.default, unit.factor) ?? String(row.entry.default), unit.name);
  };
  const reload = () => {
    if (locked) return;
    setDrafts(new Map());
    setUnitNotes(new Map());
    setOutcome(undefined);
    onReload();
  };
  const save = async () => {
    if (inFlight.current || locked || invalidCount > 0 || dirtyRows.length === 0) return;
    const submitted = new Map<string, Draft>();
    const values: Record<string, number> = {};
    for (const row of dirtyRows) {
      if (row.draft === undefined || row.effective === undefined) continue;
      submitted.set(row.entry.id, row.draft);
      values[row.entry.id] = row.effective;
    }
    const restartSaved = dirtyRows.some((row) => row.entry.restart);
    inFlight.current = true;
    setSaving(true);
    setOutcome(undefined);
    try {
      await onSave(values);
      if (!alive.current) return;
      // Only the drafts that went out unchanged are done; the parent's next props say what is saved.
      setDrafts((previous) => {
        const next = new Map(previous);
        for (const [entryId, draft] of submitted) if (next.get(entryId) === draft) next.delete(entryId);
        return next;
      });
      setOutcome({ ok: true, text: `Saved ${plural(submitted.size, "budget")}.${restartSaved ? ` ${NEXT_LAUNCH}` : ""}` });
    } catch (error) {
      // Every draft stays, including the ones that went out, so the person can retry or revert.
      if (alive.current) setOutcome({ ok: false, text: failureText(error) });
    } finally {
      inFlight.current = false;
      if (alive.current) setSaving(false);
    }
  };

  const shown = groups.filter((name) => visible.some((row) => row.entry.group === name));
  return (
    <div className="limits-panel" aria-busy={locked}>
      <p className="limits-message limits-quiet limits-scope">{SCOPE_NOTICE}</p>
      <div className="limits-toolbar" role="search">
        <input type="search" className="limits-search" aria-label="Search budgets" placeholder="Search budgets"
          value={query} onChange={(event) => setQuery(event.target.value)} spellCheck={false}
          onKeyDown={(event) => {
            // Escape empties the search first; an empty search lets it close the screen.
            // An input method's Escape cancels its composition and leaves the query alone.
            if (event.key !== "Escape" || query === "" || composing(event)) return;
            event.preventDefault();
            event.stopPropagation();
            setQuery("");
          }} />
        <select className="limits-select" aria-label="Category" value={chosenGroup}
          onChange={(event) => setGroup(event.target.value)}>
          <option value={ALL_GROUPS}>All categories</option>
          {groups.map((name) => <option key={name} value={name}>{name}</option>)}
        </select>
        <label className="limits-check">
          <input type="checkbox" checked={changedOnly} onChange={(event) => setChangedOnly(event.target.checked)} />
          Changed only
        </label>
        <span className="limits-count" aria-live="polite">{`${visible.length} of ${plural(entries.length, "budget")}`}</span>
      </div>

      {problem && <p className="limits-message limits-bad" role="alert">{problem}</p>}
      {waiting > 0 && (
        <p className="limits-message limits-restart" role="status">
          {`${plural(waiting, "saved budget")} ${waiting === 1 ? "differs" : "differ"} from what is running. `
            + `Restart WesDesk to apply ${waiting === 1 ? "it" : "them"}.`}
        </p>
      )}
      {loading && <p className="limits-message limits-quiet" role="status">Reading budgets…</p>}
      {!loading && entries.length > 0 && visible.length === 0 && (
        <p className="limits-message limits-quiet">No budgets match.</p>
      )}

      {shown.map((name) => (
        <section className="limits-group" key={name} aria-label={name}>
          <h3 className="limits-group-name">{name}</h3>
          {visible.filter((row) => row.entry.group === name).map((row) => {
            const note = unitNotes.get(row.entry.id);
            return (
              <LimitRow key={row.entry.id} row={row} rowId={`${id}-${entries.indexOf(row.entry)}`} locked={locked}
                unitNote={note && note.text === row.text && note.unit === row.unit.name ? note.message : undefined}
                onEdit={edit} onUnit={changeUnit} onRevert={revert} onReset={resetToDefault} />
            );
          })}
        </section>
      ))}

      <div className="limits-footer">
        <p className="limits-summary" aria-live="polite">
          {dirtyRows.length === 0
            ? "No unsaved changes."
            : `${plural(dirtyRows.length, "unsaved change")} · ${dirtyRows.length - restartCount} immediate · ${restartCount} at next launch`
              + (invalidCount > 0 ? ` · fix ${invalidCount} invalid to save` : "")}
        </p>
        <div className="limits-actions">
          <button type="button" className="screen-chip cell-action" disabled={locked || resettable.length === 0}
            onClick={() => { if (!locked) resettable.forEach(resetToDefault); }}
            title="Draft the default for every editable budget shown">Reset shown to defaults</button>
          <button type="button" className="screen-chip cell-action" disabled={locked} onClick={reload}>
            {dirtyRows.length > 0 ? "Reload and discard edits" : "Reload"}
          </button>
          {/* While saving, Save stays focusable and only reports itself unavailable: disabling the
              button just pressed would drop focus to the page and leave Escape nothing to reach. */}
          <button type="button" className={`screen-chip ${dirtyRows.length > 0 && invalidCount === 0 ? "chip-chosen" : "cell-action"}`}
            disabled={!saving && (locked || dirtyRows.length === 0 || invalidCount > 0)} aria-disabled={saving || undefined}
            onClick={() => { void save(); }}>
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
        {outcome && (
          <p className={`limits-message ${outcome.ok ? "limits-ok" : "limits-bad"}`} role={outcome.ok ? "status" : "alert"}>
            {outcome.text}
          </p>
        )}
      </div>
    </div>
  );
}

interface LimitRowProps {
  readonly row: Row;
  readonly rowId: string;
  /** Reading, busy or saving: the row can be read and focused but not changed. */
  readonly locked: boolean;
  /** Why the last unit change was refused, while it still applies. */
  readonly unitNote?: string;
  readonly onEdit: (entry: LimitEntry, text: string, unit: string) => void;
  readonly onUnit: (row: Row, unit: string) => void;
  readonly onRevert: (entryId: string) => void;
  readonly onReset: (row: Row) => void;
}

function LimitRow({ row, rowId, locked, unitNote, onEdit, onUnit, onRevert, onReset }: LimitRowProps) {
  const { entry, units, unit, text, parsed, dirty, invalid, moved, effective, pending } = row;
  const labelId = `${rowId}-label`;
  const messageId = `${rowId}-message`;
  const rangeId = `${rowId}-range`;
  // Escape takes back this row's edit and stops there; with nothing to take back it closes as usual.
  // While locked it still stops there, so a stray Escape cannot close the screen over a pending edit.
  const onKey = (event: KeyboardEvent<HTMLElement>) => {
    if (composing(event) || event.key !== "Escape" || row.draft === undefined) return;
    event.preventDefault();
    event.stopPropagation();
    if (!locked) onRevert(entry.id);
  };
  // What is stored, in the base unit, whenever the draft is written in a larger one.
  const preview = parsed?.ok && unit.factor !== 1 ? `= ${groupDigits(String(parsed.value))} ${units[0]!.name}` : undefined;
  const classes = ["limits-row", dirty ? "limits-row-dirty" : "", invalid ? "limits-row-invalid" : ""].filter(Boolean).join(" ");

  return (
    <div className={classes} role="group" aria-labelledby={labelId}>
      <div className="limits-head">
        <div className="limits-title">
          <span className="limits-label" id={labelId}>{entry.label}</span>
          {!entry.editable
            ? <span className="limits-badge limits-badge-fixed">Fixed</span>
            : entry.restart
              ? <span className="limits-badge limits-badge-restart" title="Takes effect the next time WesDesk starts">Next launch</span>
              : <span className="limits-badge limits-badge-now" title="Takes effect when saved">Immediate</span>}
        </div>
        <p className="limits-description">{entry.description}</p>
        {!entry.editable && <p className="limits-reason">{entry.reason || FALLBACK_REASON}</p>}
        <details className="limits-advanced">
          <summary>Details</summary>
          <dl className="limits-facts">
            <dt>Key</dt><dd>{entry.id}</dd>
            {entry.source && <><dt>Source</dt><dd>{entry.source}</dd></>}
          </dl>
        </details>
      </div>

      <dl className="limits-values">
        <dt>{pending ? "Running" : "Current"}</dt>
        <dd>{formatLimit(entry.active, entry.unit)}</dd>
        {pending && <><dt>Next start</dt><dd className="limits-next">{formatLimit(entry.saved, entry.unit)}</dd></>}
        {entry.editable && <><dt>Default</dt><dd>{formatLimit(entry.default, entry.unit)}</dd></>}
        {entry.editable && <><dt>Range</dt><dd id={rangeId}>{`${formatLimit(entry.min, entry.unit)} – ${formatLimit(entry.max, entry.unit)}`}</dd></>}
      </dl>

      {entry.editable && (
        <div className="limits-editor">
          <div className="limits-field-row">
            <input type="text" inputMode="decimal" className="limits-input" value={text} readOnly={locked}
              aria-labelledby={labelId} aria-invalid={invalid}
              aria-describedby={`${invalid || moved || preview || unitNote ? `${messageId} ` : ""}${rangeId}`}
              spellCheck={false} autoComplete="off"
              onChange={(event) => { if (!locked) onEdit(entry, event.target.value, unit.name); }} onKeyDown={onKey} />
            {units.length > 1 && (
              <select className="limits-select limits-unit" aria-label={`${entry.label} unit`} value={unit.name}
                disabled={locked} onChange={(event) => onUnit(row, event.target.value)} onKeyDown={onKey}>
                {units.map((it) => <option key={it.name} value={it.name}>{it.name}</option>)}
              </select>
            )}
          </div>
          <div className="limits-row-actions">
            <button type="button" className="screen-chip cell-action" disabled={locked || row.draft === undefined}
              onClick={() => onRevert(entry.id)} title="Back to the saved value (Escape)">Revert</button>
            <button type="button" className="screen-chip cell-action" disabled={locked || effective === entry.default}
              onClick={() => onReset(row)}>Reset to default</button>
          </div>
          <div id={messageId}>
            {parsed && !parsed.ok && <p className="limits-field-message limits-bad">{parsed.message}</p>}
            {unitNote && <p className="limits-field-message limits-warn" role="status">{unitNote}</p>}
            {preview && <p className="limits-field-message limits-quiet">{preview}</p>}
            {moved && (
              <p className="limits-field-message limits-warn">
                {`Saved value changed to ${formatLimit(entry.saved, entry.unit)} while you were editing; your edit is kept.`}
              </p>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
