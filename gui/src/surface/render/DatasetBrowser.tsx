/**
 * A Dataset's records, one bounded page at a time, beneath its descriptor.
 *
 * Each page is one read of the same committed snapshot the descriptor names; a reply for any other
 * snapshot is refused, so pages are never mixed. Rows keep their ordinal and source span beside
 * the record, drawn with the ordinary table formatting of the element type. The row area has a
 * fixed height per tier and owns its scroller, so arriving pages, read notices and a focused
 * record never move the controls. Reading is the only thing here: nothing records, refreshes or
 * changes the dataset. An explicit Follow may show newer committed records of the same recording
 * epoch or analysis attempt; they are labelled apart from the result's saved snapshot, which is
 * what Keep, Pin and the management commands refer to. A newer prefix shown this way can only be
 * captured as a new result by an ordinary `:dataset snapshot` command the person submits.
 *
 * Withdrawal or refused access clears the page and marks the stored result withdrawn, which makes
 * the enclosing block drop the descriptor, count and type details as well.
 */
import { useContext, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type KeyboardEvent, type ReactNode } from "react";
import {
  DATASET_PAGE_MIN, DatasetReadError, lastOrdinal, previousStart, recordsAfter, sameReference, withinSnapshot,
  type DatasetLifecycle, type DatasetPage, type DatasetPosition, type DatasetRead, type DatasetRecording, type RecordingTermination,
} from "../../dataset-read";
import { groupedDigits } from "../../presentation/format";
import { prepareSync } from "../../presentation/prepare";
import { present } from "../../presentation/present";
import { DATASET_REFERENCE_FIELDS, type DatasetAnchor, type DatasetReference } from "../../presentation/dataset";
import type { Mode, Run } from "../../presentation/types";
import type { StoredValue } from "../../protocol";
import { ELEMENT, metaWithin, sharedMeta, type ValueMeta } from "../../value-meta";
import { lineText, MonoLine, type Segment } from "../MonoLine";
import { DataView } from "./DataView";
import { typedJsonSummary } from "./JsonTree";
import { segments } from "./Presentation";
import { DatasetHostContext, datasetWithdrawals, type DatasetSource } from "./dataset-source";
import { registryStore } from "../../presentation/registry-store";
import { ComposeContext, freshName, inspectCommand, planDeleteCommand, retentionCommand, selectionExpression, snapshotCommand } from "../dataset-management";
import "../dataset-management.css";
import { TERMINATION_TEXT } from "../recording-terms";
import "./dataset.css";

/** Rows one read asks for, and rows the fixed row area shows before it scrolls, per tier. */
export const DATASET_PAGE_ROWS: Readonly<Record<Mode, number>> = { preview: 10, expanded: 50, window: 100 };
export const DATASET_VISIBLE_ROWS: Readonly<Record<Mode, number>> = { preview: 5, expanded: 12, window: 20 };
/** Widest a record column is drawn in a page row. */
const PAGE_COLUMNS = 160;
const DASH = "–";

type ReadState =
  | { readonly kind: "reading"; readonly retained?: DatasetRead }
  | { readonly kind: "shown"; readonly read: DatasetRead }
  | { readonly kind: "failed"; readonly error: DatasetReadError; readonly retained?: DatasetRead };

/**
 * The descriptor and, beneath it, the page reader. Without a readable source the descriptor is
 * shown alone with the reason no records are read.
 */
export function DatasetRegion({ anchor, children }: { readonly anchor: DatasetAnchor; readonly children: ReactNode }) {
  const host = useContext(DatasetHostContext);
  const source = host?.source;
  // Anything drawn without a host has no stored result either: it says so instead of staying silent.
  const reason = host?.collapsed ? undefined
    : !source ? "records unavailable · they are read from a stored result; this view holds none"
    : anchor.select === undefined ? "records behind an optional value are not read here; select the Dataset itself"
    : undefined;
  return <div className="dataset-region">
    {children}
    {reason && <MonoLine segments={[{ text: reason, role: "mono-faint" }]} className="value-line" />}
    {host && !host.collapsed && source && <DatasetManagement anchor={anchor} {...(host.name ? { name: host.name } : {})} />}
    {host && !host.collapsed && source && anchor.select !== undefined
      && <DatasetBrowser key={`${source.generation}\u0000${source.handle}\u0000${anchor.select}\u0000${referenceKey(anchor)}`}
        anchor={anchor} select={anchor.select} source={source} mode={host.mode} {...(host.name ? { name: host.name } : {})} />}
  </div>;
}

/**
 * What can be done with this Dataset beyond reading it, said beneath its descriptor. Keep and Pin
 * of the holding result retain exactly the snapshot named here — this generation and its committed
 * records — never records committed later. Inspect, plan deletion and the retention preview are
 * written into the prompt as the ordinary commands; nothing runs until the person submits them. The
 * preview is guarded by this saved snapshot's digest, never by a newer prefix a reader shows: that
 * prefix is captured as its own result first.
 */
function DatasetManagement({ anchor, name }: { readonly anchor: DatasetAnchor; readonly name?: string }) {
  const composer = useContext(ComposeContext);
  const records = anchor.reference.records;
  const last = lastOrdinal(records);
  const expression = anchor.select === undefined ? undefined : selectionExpression(name, anchor.select);
  const plan = composer ? freshName("deletion", composer.taken) : undefined;
  const retention = composer && expression ? freshName("retention", composer.taken) : undefined;
  const preview = expression && retention ? retentionCommand(expression, anchor.reference.manifestDigest, retention) : undefined;
  // A field of a larger result: its preview leaves out the result's other fields.
  const within = anchor.select !== undefined && anchor.select !== "";
  return <>
    <MonoLine segments={[{ text: "Keep or Pin retains exactly ", role: "mono-faint" },
      { text: last === undefined ? "this empty snapshot" : `records 0${DASH}${groupedDigits(last)} of generation ${groupedDigits(anchor.reference.generation)}`, role: "mono-dim" },
      { text: "; records committed later are not included", role: "mono-faint" }]} className="value-line dataset-prefix" />
    <div className="management-actions" role="group" aria-label="Dataset management">
      {composer && expression ? <>
        <button type="button" className="cell-action" onClick={() => composer.compose(inspectCommand(expression))}>inspect…</button>
        <button type="button" className="cell-action" disabled={!plan} onClick={() => plan && composer.compose(planDeleteCommand(expression, plan))}>plan deletion…</button>
        <button type="button" className="cell-action" disabled={!preview} onClick={() => preview && composer.compose(preview)}>preview retention…</button>
        <MonoLine segments={[{ text: `prepares the command in the prompt; nothing runs until you submit it${within ? " · retention covers this snapshot, not the result's other fields" : ""}`, role: "mono-faint" }]} className="value-line" />
      </> : <MonoLine segments={[{ text: !composer ? "management commands are prepared in the session" : !name ? "management commands refer to results by name; this result has none"
        : "management commands cannot name this Dataset's path", role: "mono-faint" }]} className="value-line" />}
    </div>
  </>;
}

/**
 * What a continuity refusal means here. It promises no later access to any snapshot; no other
 * attempt or source is opened on its account.
 */
export const CONTINUITY_TEXT = "this dataset no longer has the same committed identity or analysis attempt · open a result explicitly to read another snapshot";

/** At most one head inspection per second while following. */
export const FOLLOW_INTERVAL_MS = 1000;

/**
 * Why following stopped on its own, said in the follow line until the person follows again: the
 * head ended (the final page is shown) or a read failed in a way another poll would not fix.
 */
type FollowEnd = { readonly kind: "ended"; readonly read: DatasetRead } | { readonly kind: "stopped"; readonly reason: string };

/**
 * One Dataset's records. It starts Reading the stored result's own snapshot and reads nothing
 * else on its own: no head is inspected when it mounts, is restored or is expanded again.
 *
 * Follow is an explicit reader choice. It inspects the current committed head of the same EventLog
 * epoch or analysis attempt — at most once a second, one request at a time, only while this
 * reader is mounted and following — and shows that head's last page. Any reading gesture (scroll,
 * previous/next/go, editing the ordinal, opening a row, `reading`) returns to Reading at exactly
 * the head and rows shown; new records never move them. A head that ended shows its final page and
 * returns to Reading; nothing reconnects, and nothing records, refreshes or runs.
 *
 * When the last successful read shows a newer generation than the saved snapshot, `capture shown
 * prefix…` returns to Reading and writes the snapshot command for exactly that read into the prompt.
 */
export function DatasetBrowser({ anchor, select, source, mode, name }: {
  readonly anchor: DatasetAnchor; readonly select: string; readonly source: DatasetSource; readonly mode: Mode; readonly name?: string;
}) {
  const composer = useContext(ComposeContext);
  // Presentation rebuilds the anchor on every layout pass; reads follow only the snapshot it names.
  const identity = referenceKey(anchor);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const reference = useMemo(() => anchor.reference, [identity]);
  /** The committed snapshot the rows are read from: the result's own, or a newer head Follow drew. */
  const [extent, setExtent] = useState<DatasetReference>(reference);
  const extentRef = useRef(reference);
  const records = extent.records;
  const [limit, setLimit] = useState(DATASET_PAGE_ROWS[mode]);
  const [position, setPosition] = useState<DatasetPosition>({ from: "0" });
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<ReadState>({ kind: "reading" });
  const [focused, setFocused] = useState<string>();
  const [target, setTarget] = useState("");
  const [targetProblem, setTargetProblem] = useState<string>();
  const [following, setFollowing] = useState(false);
  const [followEnd, setFollowEnd] = useState<FollowEnd>();
  const latest = useRef(0);
  /** The page Follow last drew, so returning to Reading keeps it instead of reading it again. */
  const drawn = useRef<string>();
  const attemptRef = useRef(attempt);
  attemptRef.current = attempt;

  const fail = (error: unknown): DatasetReadError => error instanceof DatasetReadError ? error
    : new DatasetReadError("failed", 0, "DATASET_READ_FAILED", error instanceof Error ? error.message : String(error), false);
  const withdraw = (failure: DatasetReadError) => {
    setState({ kind: "failed", error: failure });
    setFocused(undefined);
    setFollowing(false);
    datasetWithdrawals.withdraw(source);
  };

  useEffect(() => { setLimit(DATASET_PAGE_ROWS[mode]); }, [mode]);
  // Reading: one page of the shown snapshot whenever the reader moves. Never while following.
  useEffect(() => {
    if (following) return;
    const key = pageKey(extent, position, limit, attempt);
    if (key === drawn.current) return;
    drawn.current = undefined;
    const request = ++latest.current;
    const controller = new AbortController();
    setState(was => ({ kind: "reading", ...(retainedOf(was) ? { retained: retainedOf(was)! } : {}) }));
    const reply = sameReference(extent, reference)
      ? source.engine.readDataset(source.handle, source.generation, reference, select, position, limit, controller.signal)
      : source.engine.readDatasetExtent(source.handle, source.generation, reference, extent, select, position, limit, controller.signal);
    reply.then(read => {
      if (request !== latest.current || controller.signal.aborted) return;
      drawn.current = key;
      setState({ kind: "shown", read });
      setFocused(was => was !== undefined && read.page?.rows.some(row => row.ordinal === was) ? was : undefined);
    }, (error: unknown) => {
      if (request !== latest.current || controller.signal.aborted) return;
      const failure = fail(error);
      // A reply from an older session is dropped; the block remounts for the new one.
      if (failure.kind === "session") return;
      if (failure.kind === "withdrawn") { withdraw(failure); return; }
      // Busy and failed reads may keep the last page, labelled; a missing result keeps nothing.
      setState(was => ({ kind: "failed", error: failure, ...(failure.kind !== "missing" && retainedOf(was) ? { retained: retainedOf(was)! } : {}) }));
    });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [source, select, reference, snapshotKey(extent), position, limit, attempt, following]);

  // Following: one coalesced cycle at a time — inspect the head, then read its last page — and
  // nothing else in flight. Stopping, unmounting or a new session aborts it; nothing late lands.
  useEffect(() => {
    if (!following) return;
    const request = ++latest.current;
    const controller = new AbortController();
    const signal = controller.signal;
    const live = () => request === latest.current && !signal.aborted && !datasetWithdrawals.has(source);
    void (async () => {
      // The tail is read when following starts, after a failed tail read, when the head grew, and
      // once more at its end.
      let tailDue = true;
      while (live()) {
        try {
          const head = await source.engine.readDatasetHead(source.handle, source.generation, reference, extentRef.current, select, signal);
          if (!live()) return;
          // An append may commit between the head's inspection and its read, so the reply can name a
          // prefix: later generations exist, so it grew, it did not end.
          const ended = head.lifecycle !== "open" && head.lifecycle !== "prefix";
          if (tailDue || ended || !sameReference(head.reference, extentRef.current)) {
            tailDue = true;
            const from = tailStart(head.reference.records, limit);
            const page = await source.engine.readDatasetExtent(source.handle, source.generation, reference, head.reference, select, { from }, limit, signal);
            if (!live()) return;
            extentRef.current = head.reference;
            drawn.current = pageKey(head.reference, { from }, limit, attemptRef.current);
            setExtent(head.reference);
            setPosition({ from });
            setState({ kind: "shown", read: page });
            setFocused(undefined);
            tailDue = false;
          }
          if (ended) { setFollowEnd({ kind: "ended", read: head }); setFollowing(false); return; }
        } catch (error) {
          if (!live()) return;
          const failure = fail(error);
          if (failure.kind === "session") return;
          if (failure.kind === "withdrawn") { withdraw(failure); return; }
          // The last page stays, labelled. Only a busy reader that may be asked again keeps Follow.
          setState(was => ({ kind: "failed", error: failure, ...(failure.kind !== "missing" && retainedOf(was) ? { retained: retainedOf(was)! } : {}) }));
          if (!(failure.kind === "busy" && failure.retryable)) {
            // A changed identity or attempt is never followed into; only the page already shown is kept.
            setFollowEnd({ kind: "stopped", reason: failure.kind === "continuity" ? CONTINUITY_TEXT : failure.message });
            setFollowing(false);
            return;
          }
        }
        if (!(await pause(FOLLOW_INTERVAL_MS, signal))) return;
      }
    })();
    return () => controller.abort();
  }, [following, source, select, reference, limit]);

  const shown = state.kind === "shown" ? state.read : state.retained;
  const page = shown?.page;
  const stale = state.kind !== "shown" && shown !== undefined;
  const last = lastOrdinal(records);
  const visible = DATASET_VISIBLE_ROWS[mode];
  const capture = captureOf(reference, extent, state, composer && selectionExpression(name, select), composer && freshName("shownPrefix", composer.taken));

  /** Every reading gesture leaves Follow where it is: the shown head and rows stay as they are. */
  const read = () => { setFollowing(false); };
  const follow = () => { setFollowEnd(undefined); setTargetProblem(undefined); setFollowing(true); };
  const go = (next: DatasetPosition) => { read(); setPosition(next); setTargetProblem(undefined); };
  const submitTarget = () => {
    const text = target.trim();
    if (!withinSnapshot(text, records)) {
      setTargetProblem(last === undefined ? "no records in this snapshot" : `enter an ordinal from 0 to ${groupedDigits(last)}`);
      return;
    }
    go({ from: text });
  };

  return <div className="dataset-browser" data-mode={mode} data-following={following || undefined}>
    <div className="dataset-toolbar" role="toolbar" aria-label="Dataset pages">
      <button type="button" className="cell-action" disabled={!page || page.first === "0" || state.kind === "reading"}
        onClick={() => page && go({ from: previousStart(page.first, limit) })}>‹ previous</button>
      <button type="button" className="cell-action" disabled={!page || page.cursor === null || state.kind === "reading"}
        onClick={() => page?.cursor && go({ cursor: page.cursor })}>next ›</button>
      <form className="dataset-goto" onSubmit={event => { event.preventDefault(); submitTarget(); }}>
        <label><span className="mono-faint">from</span>
          <input className="dataset-goto-input" inputMode="numeric" spellCheck={false} aria-label="First ordinal to read" aria-invalid={targetProblem !== undefined}
            value={target} placeholder="0" onChange={event => { read(); setTarget(event.target.value); setTargetProblem(undefined); }} />
        </label>
        <button type="submit" className="cell-action" disabled={last === undefined}>go</button>
      </form>
      {/* One fixed toggle: `follow` while Reading, `reading` while following. */}
      {following
        ? <button type="button" className="cell-action" aria-pressed={true} onClick={read}>reading</button>
        : <button type="button" className="cell-action" aria-pressed={false} onClick={follow}>follow</button>}
    </div>
    {/* Always one line, so a mode change never moves the rows; a withdrawal leaves it empty. */}
    <MonoLine segments={state.kind === "failed" && state.error.kind === "withdrawn" ? [] : followLine(reference, extent, following, followEnd)} className="value-line dataset-follow" />
    <MonoLine segments={rangeLine(extent, shown, page)} className="value-line dataset-range" />
    <MonoLine segments={statusLine(state, stale, targetProblem)} className="value-line dataset-status" />
    {/* Only an explicit press reads again, and only after busy, limit or read failures; withdrawn and missing stay as they are.
        The same line offers a capture only when no read failed this way, so the two never compete for it. */}
    <div className="dataset-recovery">{state.kind === "failed" && (state.error.kind === "busy" || state.error.kind === "limit" || state.error.kind === "failed") ? (state.error.kind === "limit" && limit > DATASET_PAGE_MIN
        ? <button type="button" className="cell-action" onClick={() => setLimit(was => Math.max(DATASET_PAGE_MIN, Math.floor(was / 2)))}>read {Math.max(DATASET_PAGE_MIN, Math.floor(limit / 2))} rows per page</button>
        : <button type="button" className="cell-action" onClick={() => setAttempt(n => n + 1)}>read again</button>)
      : capture && composer && <>
        {/* Reading first, so no newer extent is drawn between this press and the prepared command. */}
        <button type="button" className="cell-action" onClick={() => { read(); composer.compose(capture.command); }}>capture shown prefix…</button>
        <MonoLine segments={capture.note} description={lineText(capture.note)} className="value-line" />
      </>}</div>
    <Rows anchor={anchor} page={state.kind === "failed" && state.error.kind === "withdrawn" ? undefined : page} stale={stale}
      visible={visible} focused={focused} onFocus={ordinal => { read(); setFocused(ordinal); }} records={records}
      atEnd={following} onGesture={read} />
    {/* Below the rows, so coverage arriving with a read never moves the controls or the grid. */}
    {shown?.recording && !(state.kind === "failed" && state.error.kind === "withdrawn") && <section className="dataset-recording" aria-label="Recording coverage of this generation">
      {recordingLines(shown.recording).map((line, at) => <MonoLine key={at} segments={line} className="value-line" />)}
    </section>}
    {focused !== undefined && page && <Focused page={page} ordinal={focused} onClose={() => setFocused(undefined)} />}
  </div>;
}

const termination = (end: RecordingTermination): Segment =>
  ({ text: TERMINATION_TEXT[end].text, role: TERMINATION_TEXT[end].warn ? "mono-warn" : "mono-faint" });

/**
 * The recording's own account of this generation, in two fixed lines: the committed sequence
 * interval, then what was accepted beyond it, what was rejected, and how this generation ended.
 * It describes the snapshot only — never the writer's current state — so an unrecorded end reads
 * as such, never as an active recording, and accepted events are never called saved.
 */
export function recordingLines(recording: DatasetRecording): Segment[][] {
  const before = (BigInt(recording.first) - 1n).toString();
  const committed: Segment[] = recording.committedThrough === before
    ? [{ text: "recording · no sequences committed in this generation", role: "mono-dim" }, { text: ` · from sequence ${groupedDigits(recording.first)}`, role: "mono-faint" }]
    : [{ text: `recording · sequences ${groupedDigits(recording.first)}${DASH}${groupedDigits(recording.committedThrough)} committed in this generation`, role: "mono-dim" }];
  const end: Segment = recording.termination === null ? { text: "no end recorded in this generation", role: "mono-faint" } : termination(recording.termination);
  const pending: Segment = recording.pending === null
    ? { text: "accepted but uncommitted: unknown", role: "mono-warn" }
    : { text: `${groupedDigits(recording.pending)} accepted but not committed`, role: recording.pending === "0" ? "mono-faint" : "mono-warn" };
  return [committed, [
    { text: `accepted through ${groupedDigits(recording.acceptedThrough)}`, role: "mono-faint" }, { text: " · ", role: "mono-faint" }, pending,
    { text: ` · ${groupedDigits(recording.rejected)} rejected`, role: recording.rejected === "0" ? "mono-faint" : "mono-warn" },
    { text: " · ", role: "mono-faint" }, end,
  ]];
}

function referenceKey(anchor: DatasetAnchor): string {
  return snapshotKey(anchor.reference);
}

function snapshotKey(reference: DatasetReference): string {
  return DATASET_REFERENCE_FIELDS.map(name => reference[name]).join("\u0000");
}

/** One page read: which snapshot, where, how many rows, and which explicit read-again. */
function pageKey(extent: DatasetReference, position: DatasetPosition, limit: number, attempt: number): string {
  return [snapshotKey(extent), "cursor" in position ? `c${position.cursor}` : `f${position.from}`, limit, attempt].join("\u0001");
}

/** Where the last page of a snapshot of `records` starts, exactly. */
export function tailStart(records: string, limit: number): string {
  const start = BigInt(records) - BigInt(limit);
  return (start < 0n ? 0n : start).toString();
}

/** Waits `ms`, or resolves false at once when `signal` aborts. */
function pause(ms: number, signal: AbortSignal): Promise<boolean> {
  return new Promise(resolve => {
    if (signal.aborted) { resolve(false); return; }
    const timer = setTimeout(() => { signal.removeEventListener("abort", stop); resolve(true); }, ms);
    const stop = () => { clearTimeout(timer); resolve(false); };
    signal.addEventListener("abort", stop, { once: true });
  });
}

/**
 * Whether this reader follows or reads, and which snapshot it shows. A newer committed head is
 * labelled as such beside the result's own saved snapshot, which is what Keep and Pin retain.
 */
function followLine(original: DatasetReference, shown: DatasetReference, following: boolean, end: FollowEnd | undefined): Segment[] {
  const mode: Segment = following
    ? { text: "following · newest committed records, checked at most once a second", role: "mono-meta" }
    : end?.kind === "ended" ? { text: `reading · final page · ${endText(end.read)}`, role: "mono-dim" }
    : end?.kind === "stopped" ? { text: `reading · following stopped · ${end.reason}`, role: "mono-warn" }
    : { text: "reading", role: "mono-dim" };
  if (sameReference(original, shown)) return [mode, { text: " · the result's saved snapshot", role: "mono-faint" }];
  return [mode, { text: ` · showing newer committed records: generation ${groupedDigits(shown.generation)}, ${groupedDigits(shown.records)} records`, role: "mono-warn" },
    { text: ` · saved input is generation ${groupedDigits(original.generation)}, ${groupedDigits(original.records)} records`, role: "mono-faint" }];
}

/** How a followed head ended, in the head's own terms. */
function endText(read: DatasetRead): string {
  const end = read.recording?.termination;
  return end ? `${read.lifecycle} · ${TERMINATION_TEXT[end].text}` : read.lifecycle;
}

function retainedOf(state: ReadState): DatasetRead | undefined {
  return state.kind === "shown" ? state.read : state.retained;
}

/** What `capture shown prefix…` prepares, and the note beside it. */
interface PrefixCapture { readonly command: string; readonly note: Segment[] }

/**
 * The capture of the prefix the reader shows, when there is exactly one to name: the last successful
 * read is of the drawn extent, that extent is a later generation than the saved snapshot (with more
 * records or the same, such as a sealing commit), and the command can name the Dataset and a fresh
 * result. A page read in flight for the same extent keeps it; a page retained after a continuity stop
 * may still be named, for the engine to check. Any other failure, withdrawal or a missing result
 * offers none.
 */
function captureOf(original: DatasetReference, extent: DatasetReference, state: ReadState, expression: string | undefined, result: string | undefined): PrefixCapture | undefined {
  const read = state.kind === "shown" ? state.read
    : state.kind === "reading" || state.error.kind === "continuity" ? state.retained : undefined;
  if (!read || !expression || !result || !sameReference(read.reference, extent)) return undefined;
  if (BigInt(extent.generation) <= BigInt(original.generation)) return undefined;
  const command = snapshotCommand(expression, original.manifestDigest, extent.generation, extent.manifestDigest, result);
  if (!command) return undefined;
  const last = lastOrdinal(extent.records);
  const range = last === undefined ? `the empty generation ${groupedDigits(extent.generation)}` : `records 0${DASH}${groupedDigits(last)} of generation ${groupedDigits(extent.generation)}`;
  // Segment data is all the read reports: never a Keep, shared or exclusive storage cost.
  return { command, note: [{ text: range, role: "mono-dim" },
    { text: ` · segment data ${groupedDigits(read.segmentBytes)} bytes`, role: "mono-faint" },
    { text: " · captures it as a new result; Keep is a separate action · nothing runs until you submit it", role: "mono-faint" }] };
}

const LATER_GENERATIONS = "later generations committed";

/** A lifecycle as the range line says it. An earlier prefix is committed history, not a stopped writer. */
function lifecycleSegment(lifecycle: DatasetLifecycle): Segment {
  if (lifecycle === "prefix") return { text: ` · prefix · ${LATER_GENERATIONS}`, role: "mono-faint" };
  return { text: ` · ${lifecycle}`, role: lifecycle === "sealed" || lifecycle === "open" ? "mono-faint" : "mono-warn" };
}

/** `records 0–9 of 1 234 · 1 224 after · sealed · generation 3`: the shown range in its snapshot. */
function rangeLine(reference: DatasetReference, shown: DatasetRead | undefined, page: DatasetPage | undefined): Segment[] {
  const of = `of ${groupedDigits(reference.records)}`;
  const parts: Segment[] = [];
  if (reference.records === "0") parts.push({ text: "no records in this snapshot", role: "mono-dim" });
  else if (page && page.rows.length > 0) {
    const before = page.first, after = recordsAfter(page.next, reference.records);
    parts.push({ text: `records ${groupedDigits(page.first)}${DASH}${groupedDigits(page.rows.at(-1)!.ordinal)} ${of}`, role: "mono-dim" });
    if (before !== "0") parts.push({ text: ` · ${groupedDigits(before)} before`, role: "mono-faint" });
    if (after !== "0") parts.push({ text: ` · ${groupedDigits(after)} after`, role: "mono-faint" });
  } else parts.push({ text: `records ${of}`, role: "mono-dim" });
  parts.push({ text: ` · generation ${groupedDigits(reference.generation)}`, role: "mono-faint" });
  if (shown) parts.push(lifecycleSegment(shown.lifecycle));
  if (shown?.protected) parts.push({ text: " · kept", role: "mono-faint" });
  return parts;
}

/** The read's state in one fixed line: reading, the end of the snapshot, or why a read failed. */
function statusLine(state: ReadState, stale: boolean, targetProblem: string | undefined): Segment[] {
  if (targetProblem) return [{ text: targetProblem, role: "mono-warn" }];
  if (state.kind === "reading") return [{ text: stale ? "reading · previous page shown" : "reading…", role: "mono-dim" }];
  if (state.kind === "failed") {
    const error = state.error;
    const text = error.kind === "withdrawn" ? "Access withdrawn"
      : error.kind === "busy" ? `readers busy${stale ? " · previous page shown" : ""} · ${error.message}`
      : error.kind === "missing" ? "stored result unavailable"
      : error.kind === "continuity" ? `${CONTINUITY_TEXT}${stale ? " · previous committed page retained" : ""}`
      : error.kind === "limit" ? `page exceeds the read limit · ${error.message}`
      : `${error.code} · ${error.message}${stale ? " · previous page shown" : ""}`;
    return [{ text, role: error.kind === "busy" || error.kind === "continuity" ? "mono-warn" : "mono-bad" }];
  }
  const page = state.read.page;
  if (!page) return [];
  if (!page.extentExhausted) return [{ text: page.limitedBy ? `page limited by ${page.limitedBy}` : "", role: "mono-faint" }];
  // The end of what is committed is not the end of the producer: an open dataset can still grow.
  const lifecycle = state.read.lifecycle;
  return [{ text: lifecycle === "sealed" ? "end of dataset"
    : `end of committed snapshot · ${lifecycle === "open" ? "dataset still open" : lifecycle === "prefix" ? LATER_GENERATIONS : lifecycle}`, role: "mono-faint" }];
}

interface RowCells { readonly header: readonly string[]; readonly widths: readonly number[]; readonly cells: readonly (readonly (readonly Run[])[])[] }

/**
 * Page rows as table cells, with the formatting a List of the element type gets; a page the table
 * cannot draw falls back to one summary per record. Rows are never merged, reordered or invented.
 *
 * Declared tones come from each row's own captured metadata, moved under the List's element so
 * the table resolves them exactly as it would for that record. Rows that share one metadata are
 * drawn together; when they differ, each row is drawn under its own and only its cells are used.
 */
function useCells(anchor: DatasetAnchor, page: DatasetPage | undefined): RowCells | undefined {
  const registry = useSyncExternalStore(registryStore.subscribe, registryStore.get, registryStore.get);
  return useMemo(() => {
    if (!page || page.rows.length === 0) return undefined;
    const element = anchor.type.element;
    const values = page.rows.map(row => row.value);
    const table = (rows: readonly StoredValue[], meta: ValueMeta | undefined) => present({
      prepared: prepareSync({ type: { kind: "list", element }, data: rows.map(row => row.data), ...(meta ? { meta: metaWithin(meta, ELEMENT) } : {}) }), registry,
      context: { mode: "window", columns: PAGE_COLUMNS, lines: rows.length + 4, rows: rows.length, density: "normal", locale: "en-GB", timeZone: "UTC" },
    }).root;
    const shared = sharedMeta(values);
    const list = table(values, shared);
    if (list.kind !== "table" || list.rows.length !== page.rows.length) {
      return { header: ["value"], widths: [PAGE_COLUMNS], cells: page.rows.map(row => [[{ text: typedJsonSummary(row.value.data, row.value.type), tone: "literal" as const }]]) };
    }
    const names = list.columns.map(column => column.name).join("\u0000");
    const cells = shared || values.every(value => !value.meta) ? list.rows : values.map((value, at) => {
      if (!value.meta) return list.rows[at]!;
      // A row's own drawing never has wider columns than the page's, so its runs fit the page grid.
      const own = table([value], value.meta);
      return own.kind === "table" && own.rows.length === 1 && own.columns.map(column => column.name).join("\u0000") === names ? own.rows[0]! : list.rows[at]!;
    });
    return { header: list.columns.map(column => column.label), widths: list.columns.map(column => column.width), cells };
  }, [anchor.type.element, page, registry]);
}

/** Keys that scroll the row area: a reader pressing them is reading, not following. */
const SCROLL_KEYS = new Set(["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "]);

function Rows({ anchor, page, stale, visible, focused, onFocus, records, atEnd, onGesture }: {
  readonly anchor: DatasetAnchor; readonly page: DatasetPage | undefined; readonly stale: boolean; readonly visible: number;
  readonly focused: string | undefined; readonly onFocus: (ordinal: string | undefined) => void; readonly records: string;
  /** Following: keep the newest shown row in view as pages arrive. */
  readonly atEnd: boolean;
  /** A reader's own scroll gesture. Scrolls the view itself causes are never one. */
  readonly onGesture: () => void;
}) {
  const cells = useCells(anchor, page);
  const area = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const element = area.current;
    if (atEnd && element) element.scrollTop = element.scrollHeight;
  }, [atEnd, page]);
  // The ordinal column fits the largest ordinal of the snapshot, so paging never changes its width.
  const ordinalWidth = groupedDigits(lastOrdinal(records) ?? "0").length;
  const rows = page?.rows ?? [];
  const spanWidth = Math.max(6, ...rows.map(row => spanText(row.sourceStart, row.sourceEnd).length));
  const move = (event: KeyboardEvent<HTMLDivElement>, at: number) => {
    const next = event.key === "ArrowDown" ? at + 1 : event.key === "ArrowUp" ? at - 1 : undefined;
    if (event.key === "Escape" && focused !== undefined) { event.stopPropagation(); onFocus(undefined); return; }
    if (next === undefined || next < 0 || next >= rows.length) return;
    event.preventDefault();
    const target = event.currentTarget.parentElement?.children[next + 1] as HTMLElement | undefined;
    target?.focus();
  };
  return <div ref={area} className="dataset-rows" role="grid" aria-label="Dataset records" aria-busy={stale} style={{ ["--dataset-rows" as string]: visible }} data-stale={stale || undefined}
    onWheel={onGesture} onTouchStart={onGesture} onPointerDown={event => { if (event.target === event.currentTarget) onGesture(); }}
    onKeyDown={event => { if (SCROLL_KEYS.has(event.key)) onGesture(); }}>
    <div className="dataset-row dataset-row-header" role="row">
      <span role="columnheader" className="mono-faint dataset-ordinal" style={{ width: `${ordinalWidth}ch` }}>#</span>
      <span role="columnheader" className="mono-faint dataset-span" style={{ width: `${spanWidth}ch` }}>source</span>
      {cells?.header.map((name, at) => <span key={at} role="columnheader" className="mono-param dataset-cell" style={{ width: `${cells.widths[at]}ch` }}>{name}</span>)}
    </div>
    {rows.map((row, at) => <div key={row.ordinal} role="row" tabIndex={0} className="dataset-row" aria-selected={focused === row.ordinal}
      onClick={() => onFocus(focused === row.ordinal ? undefined : row.ordinal)}
      onKeyDown={event => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); onFocus(focused === row.ordinal ? undefined : row.ordinal); } else move(event, at); }}>
      <span role="rowheader" className="mono-ref dataset-ordinal" style={{ width: `${ordinalWidth}ch` }}>{groupedDigits(row.ordinal)}</span>
      <span role="gridcell" className="mono-faint dataset-span" style={{ width: `${spanWidth}ch` }}>{spanText(row.sourceStart, row.sourceEnd)}</span>
      {cells?.cells[at]?.map((runs, column) => <span key={column} role="gridcell" className="dataset-cell" style={{ width: `${cells.widths[column]}ch` }}>
        <MonoLine segments={segments(runs)} className="value-line" /></span>)}
    </div>)}
  </div>;
}

/** A record's source span, `start–end`, exact. */
function spanText(start: string, end: string): string {
  return `${groupedDigits(start)}${DASH}${groupedDigits(end)}`;
}

/** The focused record whole, beneath the rows, with its own type and captured metadata. */
function Focused({ page, ordinal, onClose }: { readonly page: DatasetPage; readonly ordinal: string; readonly onClose: () => void }) {
  const row = page.rows.find(it => it.ordinal === ordinal);
  if (!row) return null;
  return <section className="dataset-focused" aria-label={`Record ${ordinal}`}>
    <div className="dataset-focused-head">
      <MonoLine segments={[{ text: `record ${groupedDigits(row.ordinal)}`, role: "mono-ref" }, { text: ` · source ${spanText(row.sourceStart, row.sourceEnd)}`, role: "mono-faint" }]} className="value-line" />
      <button type="button" className="cell-action" onClick={onClose}>close</button>
    </div>
    <div className="dataset-focused-body"><DataView type={row.value.type} data={row.value.data} {...(row.value.meta ? { meta: row.value.meta } : {})} lines={60} /></div>
  </section>;
}
