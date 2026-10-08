/**
 * The engine's read-only continuation review, laid out beside the generic value in `/open` details
 * and the Inspector alike, with the only two things a person may prepare from it.
 *
 * - **review changed bounds…** writes a new `:scan continuation` of the original analysis with the
 *   edited totals into the prompt. It is another read-only review, never an application.
 * - **continue with these bounds…** writes the guarded `:scan continue` for exactly the basis and the
 *   six totals this review shows; the basis binds them all. Editing any total withdraws it until a new
 *   review arrives.
 *
 * Neither runs anything: the person submits the command, and the engine checks the latest checkpoint,
 * active ceilings, ownership and source again. Availability is the engine's `canContinue` and its
 * closed reason alone; the counters are shown, never used to decide. The original analysis is named
 * by the node id the engine put in the review, never by parsing command text, and is never refreshed.
 *
 * Reading order puts the decision first — verdicts, stop and committed position, then the six totals
 * and the two actions — and keeps identities, issuance history, accounting and the frozen
 * configuration in a subordinate disclosure after them. Nothing is hidden: it is only ordered.
 */
import { useContext, useState } from "react";
import type { WorkspaceNode } from "../workspace";
import { ComposeContext, freshName, REFERABLE_NODE } from "./dataset-management";
import { lineText, MonoLine, type MonoRole, type Segment } from "./MonoLine";
import { grouped } from "./record-progress";
import { dimensionWords } from "./scan-receipt";
import {
  continuationCommand, continueCommand, literalTotal, requestedTotals, TOTALS,
  type BoundStatus, type ContinuationPreview, type ContinuationReason, type TotalName, type Totals,
} from "./scan-continuation";
import "./record-progress.css";
import "./dataset-management.css";

/** The engine's closed refusal reasons, said in words. None recommends raising a global limit. */
export const REASON_WORDS: Readonly<Record<ContinuationReason, string>> = {
  not_latest: "a newer checkpoint of this analysis exists · review its latest attempt",
  active_writer: "the analysis is still writing",
  finished: "finished · nothing to continue",
  deterministic_stop: "stopped by its own code or a per-record limit · raising totals cannot change that",
  cumulative_stop: "stopped at a cumulative total · the same totals would stop there again",
  incomplete_source: "its recorded source is incomplete · raising totals cannot change that",
  lowered: "a requested total is below the current one · totals can only stay or rise",
  above_ceiling: "a total is above the active ceiling",
  frozen_above_ceiling: "its frozen configuration is above the active ceiling",
  unchanged: "no total is raised",
  ineffective_raise: "the raise does not include the limit that stopped it",
  no_headroom: "no headroom remains under these totals",
};
const STATUS_WORDS: Readonly<Record<BoundStatus, string>> = {
  above_ceiling: "above active ceiling", lowered: "lowered", unchanged: "unchanged", raised: "raised",
};
const STATUS_ROLE: Readonly<Record<BoundStatus, MonoRole>> = {
  above_ceiling: "mono-warn", lowered: "mono-warn", unchanged: "mono-dim", raised: "mono-ok",
};
const TOTAL_WORDS: Readonly<Record<TotalName, string>> = {
  work: "work", input: "input charge", records: "input records", output: "output charge", outputs: "output records", duration: "duration ms",
};
const STOP_WORDS: Readonly<Record<string, string>> = {
  cancelled: "cancelled", deterministic: "its own code or a per-record limit", incomplete_source: "incomplete source",
};
export const stopWords = (stop: string) => STOP_WORDS[stop] ?? dimensionWords(stop);

export const REVIEW_CHANGED = "review changed bounds…";
export const CONTINUE = "continue with these bounds…";
const EDITED = "bounds edited · review them first; this review covers only the totals it showed";
const PREPARED = "a new attempt with exactly these totals; prior debits stay charged and the engine checks everything again. Nothing runs until you submit it";
const REVIEW_NOTE = "a new read-only review; nothing is reserved or run";
const UNHOSTED = "continuation commands are prepared in the session";
const UNNAMED = "the original analysis has no node id a command can name";

const SEP: Segment = { text: " · ", role: "mono-faint" };
const dim = (text: string): Segment => ({ text, role: "mono-dim" });
const ink = (text: string, role: MonoRole = "mono-ink"): Segment => ({ text, role });
const n = (digits: string) => ink(grouped(digits));
const yes = (value: boolean) => ink(value ? "yes" : "no");

export function continuationHeadline(preview: ContinuationPreview): Segment[] {
  return [dim("continuation review"), SEP, preview.canContinue
    ? ink("these bounds can continue", "mono-ok")
    : ink(`continue refused · ${REASON_WORDS[preview.continueReason!]}`, "mono-warn")];
}

/** The decision first: the engine's two verdicts, where it stopped and what it has committed. */
export function continuationRows(preview: ContinuationPreview): Segment[][] {
  return [
    [dim("continue with the requested bounds "), preview.canContinue ? ink("permitted", "mono-ok") : ink(`refused · ${REASON_WORDS[preview.continueReason!]}`, "mono-warn")],
    [dim("ordinary resume under the latest granted bounds "), preview.canResume ? ink("permitted", "mono-ok") : ink(`refused · ${REASON_WORDS[preview.resumeReason!]}`, "mono-warn")],
    [dim("stopped by "), preview.stop === undefined ? ink("none") : ink(stopWords(preview.stop), "mono-warn"), SEP,
      dim("committed position "), n(preview.position), SEP, n(preview.outputCount), dim(" outputs")],
  ];
}

/**
 * Everything else the engine reported, for the subordinate disclosure after the decision: checkpoint
 * state, exact accounting, the historical issuance ceilings, identities and the frozen configuration.
 */
export function continuationDetailRows(preview: ContinuationPreview): Segment[][] {
  const f = preview.frozen;
  return [
    [dim("latest checkpoint "), yes(preview.latest), SEP, dim("active writer "), yes(preview.activeWriter), SEP, dim("lifecycle "), ink(preview.lifecycle)],
    [dim("work measured "), n(preview.measuredWork), SEP, dim("charged "), n(preview.chargedWork), SEP, dim("unconfirmed reservation "), n(preview.outstandingWork)],
    [dim("work charged after interruption "), n(preview.chargedAfterInterruption), dim(" · the whole unconfirmed reservation, once")],
    [dim("duration charged "), n(preview.durationChargedMs), dim(" ms"), SEP, dim("unconfirmed reservation "), n(preview.durationOutstandingMs), dim(" ms"), SEP,
      dim("after interruption "), n(preview.durationAfterInterruptionMs), dim(" ms")],
    [dim("work allowance now "), n(preview.allowanceBefore), SEP, dim("after continuing "), n(preview.allowanceAfter), SEP,
      dim("explicitly authorized so far "), n(preview.authorizedWork), SEP, dim("this grant "), n(preview.newWorkGrant)],
    // History: the ceilings in force when the current totals were issued, not the active ones above.
    [dim("issuance ceilings when the current totals were issued · not the active ceiling")],
    ...preview.bounds.map(bound => [ink(TOTAL_WORDS[bound.key], "mono-param"), SEP, dim("issuance ceiling "), n(bound.issuanceCeiling)]),
    [dim("analysis "), ink(preview.analysis, "mono-ref"), SEP, dim("run "), ink(preview.run, "mono-dim"), SEP, dim("checkpoint attempt "), ink(preview.attempt, "mono-dim")],
    [dim("bounds receipt "), ink(preview.budgetDigest, "mono-dim"), SEP, dim("issued by attempt "), ink(preview.budgetIssuedAttempt, "mono-dim")],
    [dim("frozen · held memory "), n(f.memory), SEP, dim("per-record work "), n(f.recordWork), SEP, dim("scratch "), n(f.scratch), SEP,
      dim("outputs per record "), n(f.recordOutputs), SEP, dim("startup work "), n(f.startup), SEP, dim("work per input unit "), n(f.rate)],
    [dim("frozen · transition "), ink(f.stepRevision, "mono-dim"), SEP, dim("finish "), f.finishRevision === undefined ? ink("none") : ink(f.finishRevision, "mono-dim"), SEP,
      dim("profile "), ink(f.profileRevision, "mono-dim"), SEP, dim("source "), ink(f.sourceDigest, "mono-dim")],
  ];
}

/** One total as decided: the current total, the request, the active ceiling and the engine's verdict. */
export function boundRow(bound: ContinuationPreview["bounds"][number]): Segment[] {
  return [ink(TOTAL_WORDS[bound.key], "mono-param"), SEP, dim("current "), n(bound.current), SEP, dim("requested "), n(bound.requested), SEP,
    dim("active ceiling "), n(bound.activeCeiling), SEP, ink(STATUS_WORDS[bound.status], STATUS_ROLE[bound.status])];
}

export const DETAILS = "accounting, identities and frozen settings";
const NOTES: readonly string[] = [
  "Read-only review: nothing was reserved, reconciled or run, and no producer is contacted.",
  "Duration is a cooperative limit, not preemption; no external effect is replayed or promised exactly once.",
  "This value grants nothing and a copy of it is no authority; Continue is checked again against the latest checkpoint.",
];

export function ScanContinuationDetails({ preview, open = false, node }: { readonly preview: ContinuationPreview; readonly open?: boolean; readonly node?: WorkspaceNode }) {
  const headline = continuationHeadline(preview);
  return (
    <details className="scan-receipt scan-continuation" open={open}>
      <summary aria-label={lineText(headline)}><MonoLine segments={headline} className="scan-receipt-headline" /></summary>
      {continuationRows(preview).map((row, at) => <MonoLine key={at} segments={row} className="scan-receipt-row" />)}
      {/* A new review is a new value: edits never survive into it. */}
      <ContinuationBounds key={`${preview.basis}:${TOTALS.map(key => requestedTotals(preview)[key]).join(",")}`} preview={preview} {...(node ? { node } : {})} />
      <details className="scan-continuation-more">
        <summary><MonoLine segments={[dim(DETAILS)]} className="scan-receipt-row" /></summary>
        {continuationDetailRows(preview).map((row, at) => <MonoLine key={at} segments={row} className="scan-receipt-row" />)}
        {NOTES.map(note => <MonoLine key={note} segments={[{ text: note, role: "mono-faint" }]} className="scan-receipt-row" />)}
      </details>
    </details>
  );
}

function ContinuationBounds({ preview, node }: { readonly preview: ContinuationPreview; readonly node?: WorkspaceNode }) {
  const composer = useContext(ComposeContext);
  const reviewed = requestedTotals(preview);
  const [draft, setDraft] = useState<Totals>(reviewed);
  const edited = TOTALS.some(key => draft[key] !== reviewed[key]);
  const invalid = TOTALS.filter(key => !literalTotal(draft[key]));
  const nameable = REFERABLE_NODE.test(preview.node);
  // The hosting review is current only once its own node has settled ready; anything else is older.
  const current = node === undefined || node.state === "ready";

  const reviewName = composer ? freshName("bounds", composer.taken) : undefined;
  const reviewCommand = reviewName ? continuationCommand(preview.node, draft, reviewName) : undefined;
  const reviewWhy = !nameable ? UNNAMED
    // Only the literal's spelling is checked here; the new review reports whether the engine admits it.
    : invalid.length ? `${invalid.map(key => TOTAL_WORDS[key]).join(", ")}: a positive whole number is required`
    : !edited ? "edit a total to review changed bounds"
    : !composer ? UNHOSTED
    : !reviewCommand ? "no unused result name is available" : undefined;

  const continueName = composer ? freshName("continued", composer.taken) : undefined;
  const command = continueName ? continueCommand(preview.node, preview.basis, reviewed, continueName) : undefined;
  const continueWhy = edited ? EDITED
    : !preview.canContinue ? REASON_WORDS[preview.continueReason!]
    : !current ? "this review is not current · review again"
    : !nameable ? UNNAMED
    : TOTALS.some(key => !literalTotal(reviewed[key])) ? "a reviewed total cannot be written as a command literal"
    : !composer ? UNHOSTED
    : !command ? "no unused result name is available" : undefined;

  return <div className="scan-continuation-bounds" role="group" aria-label="Continuation bounds"
    onKeyDown={event => {
      // Escape first discards local edits, before it may leave the enclosing surface.
      if (event.key === "Escape" && edited && !event.defaultPrevented) { event.preventDefault(); event.stopPropagation(); setDraft(reviewed); }
    }}>
    {preview.bounds.map(bound => {
      const changed = draft[bound.key] !== bound.requested;
      return <div key={bound.key} className="scan-continuation-bound">
        <MonoLine segments={changed ? [...boundRow(bound).slice(0, -1), ink("edited · not reviewed", "mono-meta")] : boundRow(bound)} className="scan-receipt-row" />
        <label className="scan-continuation-edit">
          <span className="mono-dim">requested {TOTAL_WORDS[bound.key]}</span>
          <input className="scan-continuation-input" type="text" inputMode="numeric" spellCheck={false} autoComplete="off"
            aria-label={`requested ${TOTAL_WORDS[bound.key]}`} aria-invalid={!literalTotal(draft[bound.key])}
            value={draft[bound.key]} onChange={event => { const next = event.currentTarget.value.trim(); setDraft(was => ({ ...was, [bound.key]: next })); }} />
        </label>
      </div>;
    })}
    <div className="management-actions" role="group" aria-label="Analysis continuation">
      <button type="button" className="cell-action" disabled={reviewWhy !== undefined}
        onClick={() => { if (!reviewWhy && composer && reviewCommand) composer.compose(reviewCommand); }}>{REVIEW_CHANGED}</button>
      <MonoLine segments={[{ text: reviewWhy ?? REVIEW_NOTE, role: "mono-faint" }]} className="value-line" />
    </div>
    <div className="management-actions" role="group" aria-label="Continue analysis">
      <button type="button" className="cell-action" disabled={continueWhy !== undefined}
        onClick={() => { if (!continueWhy && composer && command) composer.compose(command); }}>{CONTINUE}</button>
      <MonoLine segments={[{ text: continueWhy ?? PREPARED, role: continueWhy && !edited && !preview.canContinue ? "mono-warn" : "mono-faint" }]} className="value-line" />
    </div>
  </div>;
}
