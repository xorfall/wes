/**
 * What a finite record analysis reports while it runs, and what its stopped value is.
 *
 * One reading for the cell, `/open` and the inspector. Always three rows, so arriving counts, a
 * phase change or a withheld counter never move what is below them. Every figure is the engine's:
 * committed and read positions are said separately in the engine's unit; work is said against the
 * earned allowance and the fixed limit; held and output figures are conservative logical charges,
 * never memory use or stored bytes. Nothing here is drawn from a guess, and no phase implies success.
 */
import type { RecordCounters, RecordPhase } from "../protocol";
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
  reading: "mono-meta", processing: "mono-meta", finishing: "mono-meta",
  complete: "mono-ok", stopped: "mono-warn", cancelled: "mono-warn",
};
const SEP: Segment = { text: " · ", role: "mono-faint" };
const value = (text: string): Segment => ({ text, role: "mono-ink" });
const label = (text: string): Segment => ({ text, role: "mono-dim" });

/** The three rows for the node's current run, or nothing when the engine has reported none for it. */
export function progressRows(node: WorkspaceNode | undefined): Segment[][] | undefined {
  const progress = currentProgress(node);
  if (!node || !progress) return undefined;
  // Reports are lossy and the engine's state wins. A working phase on a settled node is old; a
  // terminal phase is current only on the node state it belongs to — `complete` never speaks for a
  // running, stale, failed or cancelled node, and `stopped` never for a ready one.
  const owner = TERMINAL[progress.phase];
  const old = owner === undefined ? SETTLED.includes(node.state) : node.state !== owner;
  const phase: Segment = old
    ? { text: `last reported ${progress.phase}`, role: "mono-dim" }
    : { text: progress.phase, role: PHASE_ROLE[progress.phase] };
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
    // Earned allowance grows with committed input; the cap is the run's fixed absolute limit.
    label("work "), value(grouped(c.work)), label(" used of "), value(grouped(c.workAllowance)), label(" earned"),
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

/** What the block says to assistive technology, whole, since narrow rows clip. */
export const PROGRESS_NOTE = "Engine-reported progress of this run. Charges are conservative logical charges, not memory use or stored bytes.";
