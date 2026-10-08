/**
 * What a finite record analysis reports while it runs, and what its stopped value is.
 *
 * One reading for the cell, `/open` and the inspector. Always three rows, so arriving counts, a
 * phase change or a withheld counter never move what is below them. Every figure is the engine's:
 * committed and read positions are said separately in the engine's unit; work is said against the
 * allowance (input-earned, plus any explicitly authorized continuation credit) and the fixed limit;
 * held and output figures are conservative logical charges,
 * never memory use or stored bytes. Nothing here is drawn from a guess, and no phase implies success.
 *
 * A recording uses the same three rows for its writer's latest acknowledged status: committed and
 * accepted sequences, pending (or unknown) and rejected events with how it ended, and its charges
 * against the captured limits. It has no finite extent, held memory or earned allowance.
 */
import type { RecordCounters, RecordingCounters, RecordPhase } from "../protocol";
import { TERMINATION_TEXT } from "./recording-terms";
import { currentProgress, type WorkspaceNode } from "../workspace";
import type { MonoRole, Segment } from "./MonoLine";

/** The fixed height of the progress block, in rows. */
export const PROGRESS_ROWS = 3;

/** What the display-only value of a terminal node is. Only a stopped stream is a stream. */
export function evidenceLabel(node: WorkspaceNode | undefined): string | undefined {
  if (node?.evidence?.kind === "stopped_stream") return "stream stopped · last value";
  if (node?.evidence?.kind === "incomplete") return "incomplete · committed partial result, not a completed run";
  return undefined;
}

/** `1234567` → `1,234,567`, exactly, at any size. */
export function grouped(digits: string): string {
  return digits.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

/** The engine state each terminal phase belongs to: complete is only ever a ready node's. */
const TERMINAL: Partial<Readonly<Record<RecordPhase, string>>> = { complete: "ready", stopped: "failed", cancelled: "cancelled" };
const SETTLED = ["ready", "failed", "cancelled", "skipped", "stale"];
const PHASE_ROLE: Record<RecordPhase, MonoRole> = {
  reading: "mono-meta", processing: "mono-meta", finishing: "mono-meta", committing: "mono-meta",
  complete: "mono-ok", stopped: "mono-warn", cancelled: "mono-warn",
};
const SEP: Segment = { text: " · ", role: "mono-faint" };
const value = (text: string): Segment => ({ text, role: "mono-ink" });
const label = (text: string): Segment => ({ text, role: "mono-dim" });

/**
 * Whether the engine says this node's current run — a recording or a followed scan — is still open.
 * Only its own lifetime signal for this exact run counts: never progress, a receipt, a type or the
 * command. Recording controls are separate authority and do not decide this.
 */
export function lifetimeActive(node: WorkspaceNode | undefined): boolean {
  const open = node?.openLifetime;
  return !!node && !!open && node.run !== undefined && open.run === node.run && !node.accessWithdrawn
    && (node.state === "ready" || node.state === "running");
}

/**
 * What a ready node with an open lifetime is doing, named by its engine progress kind: native scan
 * progress analyzes, writer progress records. Without a report for this run it is only "active".
 */
export function openLifetimeWord(node: WorkspaceNode): "analyzing" | "recording" | "active" {
  const kind = currentProgress(node)?.kind;
  return kind === "records" ? "analyzing" : kind === "recording" ? "recording" : "active";
}

/** The three rows for the node's current run, or nothing when the engine has reported none for it. */
export function progressRows(node: WorkspaceNode | undefined): Segment[][] | undefined {
  const progress = currentProgress(node);
  if (!node || !progress) return undefined;
  // Reports are lossy and the engine's state wins. A working phase on a settled node is old; a
  // terminal phase is current only on the node state it belongs to — `complete` never speaks for a
  // running, stale, failed or cancelled node, and `stopped` never for a ready one.
  const owner = TERMINAL[progress.phase];
  // A recording or followed scan is ready once its first prefix is usable while its run goes on. Only
  // the engine's lifetime signal for this exact run says it is still open: then a working phase on a
  // ready node is current and a terminal report is not yet the run's end. Without that signal
  // (finite work, copies, restored work) the ordinary rules apply unchanged.
  const open = lifetimeActive(node);
  const old = owner === undefined
    ? SETTLED.includes(node.state) && !(open && node.state === "ready")
    : node.state !== owner || open;
  // A recording names its writer state, which is more exact than the shared phase it maps to.
  const said = progress.kind === "recording" && progress.recording ? progress.recording.state : progress.phase;
  const phase: Segment = old
    ? { text: `last reported ${said}`, role: "mono-dim" }
    : { text: said, role: PHASE_ROLE[progress.phase] };
  if (progress.kind === "recording") {
    if (!progress.recording) {
      return [
        [phase, SEP, { text: "writer status withheld · the source is not public", role: "mono-warn" }],
        [label("accepted "), value("—")],
        [label("charge "), value("—")],
      ];
    }
    return recordingRows(phase, progress.recording);
  }
  const counters = progress.counters;
  if (!counters) {
    return [
      [phase, SEP, { text: "counters withheld · the input is not public", role: "mono-warn" }],
      [label("records "), value("—")],
      [label("charge "), value("—")],
    ];
  }
  return [positionRow(phase, counters), recordsRow(counters), chargeRow(counters)];
}

function positionRow(phase: Segment, c: RecordCounters): Segment[] {
  const pending = BigInt(c.readPosition) > BigInt(c.committedPosition);
  return [
    phase, SEP,
    label("committed through "), value(grouped(c.committedPosition)), label(` of ${grouped(c.extent)} ${c.unit}`), SEP,
    label("read through "), value(grouped(c.readPosition)),
    ...(pending ? [label(" · read, not committed")] : []),
  ];
}

function recordsRow(c: RecordCounters): Segment[] {
  return [
    value(grouped(c.inputRecords)), label(" records in"), SEP, value(grouped(c.outputRecords)), label(" outputs"), SEP,
    // The allowance grows with committed input and, after an explicit continuation, may include
    // authorized credit this progress does not separate; so it is not called earned. The cap is fixed.
    label("work "), value(grouped(c.work)), label(" used of "), value(grouped(c.workAllowance)), label(" allowed"),
    SEP, label("cap "), value(grouped(c.workLimit)),
  ];
}

function chargeRow(c: RecordCounters): Segment[] {
  return [
    label("logical charge · held "), value(grouped(c.heldCharge)), label(" · high-water "), value(grouped(c.highWaterCharge)),
    label(" of cap "), value(grouped(c.heldLimit)), SEP,
    label("output "), value(grouped(c.outputCharge)), label(" of cap "), value(grouped(c.outputLimit)),
  ];
}

/**
 * A recording's three rows: what is committed and accepted; what is pending (or unknown) and
 * rejected, with how the writer ended; and its conservative charges against the captured limits.
 */
function recordingRows(phase: Segment, r: RecordingCounters): Segment[][] {
  const before = (BigInt(r.first) - 1n).toString();
  const committed: Segment[] = r.committedThrough === before
    ? [label("nothing committed yet"), label(` · from sequence ${grouped(r.first)}`)]
    : [label("committed "), value(`${grouped(r.first)}–${grouped(r.committedThrough)}`)];
  const end: Segment = r.termination === null
    ? label("no end reported")
    : { text: TERMINATION_TEXT[r.termination].text, role: TERMINATION_TEXT[r.termination].warn ? "mono-warn" : "mono-dim" };
  return [
    [phase, SEP, ...committed, SEP, label("accepted through "), value(grouped(r.acceptedThrough))],
    [r.pending === null ? { text: "pending unknown", role: "mono-warn" } : { text: `${grouped(r.pending)} pending`, role: r.pending === "0" ? "mono-dim" : "mono-ink" },
      SEP, { text: `${grouped(r.rejected)} rejected`, role: r.rejected === "0" ? "mono-dim" : "mono-warn" }, SEP, end],
    [label("logical charge · value "), value(grouped(r.chargedBytes)), label(" of cap "), value(grouped(r.bytesLimit)), SEP,
      label("work "), value(grouped(r.chargedWork)), label(" of cap "), value(grouped(r.workLimit))],
  ];
}

/** What the block says to assistive technology, whole, since narrow rows clip. */
export const PROGRESS_NOTE = "Engine-reported progress of this run. Charges are conservative logical charges, not memory use or stored bytes.";
/** The same for a recording: the writer's latest lossy status, not a stored receipt. */
export const RECORDING_NOTE = "Latest status acknowledged by this recording's writer, reported at most ten times a second. It is not a stored receipt, the source's state or a live queue count. Charges are conservative logical charges against the captured limits, not memory use or stored bytes.";
/** Which kind of run the progress block describes, for its label and note. */
export const progressKind = (node: WorkspaceNode | undefined) => currentProgress(node)?.kind;
