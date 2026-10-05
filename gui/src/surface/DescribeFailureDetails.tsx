/**
 * The saved report behind a failed `:describe`: read privately, shown twice.
 *
 * The engine's error carries only an opaque report reference (`DSC_REPORT`). The report itself
 * — validation reason, findings with their kind, operation and source lines, the omitted count —
 * is read through the trusted API-library endpoint, once it is wanted: when the cell's fold opens,
 * or as soon as the error peek does. One reader (`useDescribeFailureReport`) and one presentation
 * (`DescribeFailureContent`) serve both, so neither can drift from the other. Source text is
 * rendered as text, never as markup, and reading never reruns extraction.
 */
import { useCallback, useEffect, useState } from "react";
import type { ErrorRecord } from "../protocol";
import { libraryRequest } from "../api-library";

export interface DescribeFailureIssue {
  kind: string; operation: string; message: string; lines: { start: number; end: number }[];
}
export interface DescribeFailureReport {
  id: string; location: string; discoveredLocation: string; sourceDigest: string;
  report: { version: 1; message: string; omitted: number; issues: DescribeFailureIssue[] };
}
export function describeReportId(error?: ErrorRecord): string | undefined {
  const id = error?.issues.find(issue => issue.code === "DSC_REPORT" && issue.path === "/describeReport")?.message;
  return id && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(id) ? id : undefined;
}

const UNAVAILABLE = "Failure details unavailable.";
const CAVEAT = "OpenAPI parsing and contract validation findings. No spec was saved.";

/** One read of one report: what came back, or why it did not, and a way to ask again. */
export interface DescribeFailureRead {
  readonly report?: DescribeFailureReport;
  readonly problem?: string;
  readonly reading: boolean;
  readonly retry: () => void;
}

interface ReadState { id?: string; report?: DescribeFailureReport; problem?: string; attempt: number }

/**
 * Reads the report `id` names once `wanted`, and keeps it. An answer is kept only under the id
 * it was asked for: a changed id starts over and the earlier request is aborted, so a stale
 * report cannot outlive a retry. `retry` forgets a failed read and asks again.
 */
export function useDescribeFailureReport(id: string | undefined, wanted: boolean): DescribeFailureRead {
  const [state, setState] = useState<ReadState>({ attempt: 0 });
  const current = state.id === id ? state : undefined;
  const settled = current?.report !== undefined || current?.problem !== undefined;
  const retry = useCallback(() => setState(previous => ({ attempt: previous.attempt + 1 })), []);
  useEffect(() => {
    if (!wanted || !id || settled) return;
    const controller = new AbortController();
    void libraryRequest<DescribeFailureReport>({ action: "describeFailure", id }, controller.signal)
      .then(report => { if (!controller.signal.aborted) setState(previous => ({ attempt: previous.attempt, id, report })); })
      .catch(error => {
        if (controller.signal.aborted) return;
        const problem = error instanceof Error && error.message ? error.message : UNAVAILABLE;
        setState(previous => ({ attempt: previous.attempt, id, problem }));
      });
    return () => controller.abort();
  }, [id, wanted, settled, state.attempt]);
  return {
    ...(current?.report ? { report: current.report } : {}),
    ...(current?.problem ? { problem: current.problem } : {}),
    reading: wanted && id !== undefined && !settled,
    retry,
  };
}

/** `83–85`, or `137` when a range is one line: the same fact, said once. */
export function describeLineRanges(lines: readonly { start: number; end: number }[]): string {
  return lines.map(line => (line.start === line.end ? `${line.start}` : `${line.start}–${line.end}`)).join(", ");
}

/** The report as text, for the clipboard: every fact the screen shows, in the same order. */
export function describeFailureText(details: DescribeFailureReport): string {
  const { report } = details;
  const head = [
    report.message,
    `source: ${details.location}`,
    ...(details.discoveredLocation && details.discoveredLocation !== details.location ? [`discovered: ${details.discoveredLocation}`] : []),
    `digest: ${details.sourceDigest}`,
    `report: ${details.id}`,
    CAVEAT,
  ];
  const issues = report.issues.map(issue => {
    const label = [issue.kind, ...(issue.operation ? [issue.operation] : []), ...(issue.lines.length > 0 ? [`lines ${describeLineRanges(issue.lines)}`] : [])];
    return `${label.join(" · ")}\n  ${issue.message.split("\n").join("\n  ")}`;
  });
  const tail = report.omitted > 0 ? [`${report.omitted} additional diagnostics omitted by the report budget.`] : [];
  return [head.join("\n"), ...issues, ...tail].join("\n\n");
}

const dot = <span className="mono-faint"> · </span>;

/**
 * The report, laid out: the reason first, the source as one compact line, then one
 * finding per row — its kind, operation and lines on a head line, its prose wrapping under it —
 * and the omitted count last. `cell` keeps the rows under a scroll ceiling and points at the
 * peek; `peek` gives them the room and the provenance the window has.
 */
export function DescribeFailureContent({ details, mode }: { details: DescribeFailureReport; mode: "cell" | "peek" }) {
  const { report } = details;
  const discovered = details.discoveredLocation && details.discoveredLocation !== details.location ? details.discoveredLocation : undefined;
  return <div className={`describe-failure describe-failure-${mode}`}>
    <p className="mono-bad describe-failure-prose">{report.message}</p>
    <p className="describe-failure-meta">
      <span className="mono-dim">{details.location}</span>
    </p>
    {mode === "peek" && <p className="describe-failure-meta">
      {discovered && <><span className="mono-faint">discovered </span><span className="mono-dim">{discovered}</span>{dot}</>}
      <span className="mono-faint">digest </span><span className="mono-faint">{details.sourceDigest}</span>{dot}
      <span className="mono-faint">report </span><span className="mono-faint">{details.id}</span>
    </p>}
    <p className="mono-faint describe-failure-prose">{CAVEAT}</p>
    {report.issues.length > 0 && <ol className="describe-failure-issues" aria-label="Findings" tabIndex={mode === "cell" ? 0 : undefined}>
      {report.issues.map((issue, index) => <li key={index} className="describe-failure-issue">
        <p className="describe-failure-issue-head">
          <span className="mono-warn">{issue.kind}</span>
          {issue.operation && <>{dot}<span className="mono-provider">{issue.operation}</span></>}
          {issue.lines.length > 0 && <>{dot}<span className="mono-faint">{`lines ${describeLineRanges(issue.lines)}`}</span></>}
        </p>
        <p className="mono-ink describe-failure-prose">{issue.message}</p>
      </li>)}
    </ol>}
    {report.omitted > 0 && <p className="mono-warn describe-failure-prose">{`${report.omitted} additional diagnostics omitted by the report budget.`}</p>}
    {mode === "cell" && <p className="mono-dim describe-failure-prose">⌘click the verdict to read the whole report in its own window, with copy.</p>}
  </div>;
}

/** What a read looks like at each moment: still reading, unreadable (with retry), or the report. */
export function DescribeFailureReading({ read, mode }: { read: DescribeFailureRead; mode: "cell" | "peek" }) {
  if (read.problem) {
    return <div className="describe-failure-problem" role="alert">
      <p className="mono-bad describe-failure-prose">{read.problem}</p>
      <button type="button" className="cell-action" onClick={read.retry}>retry reading details</button>
    </div>;
  }
  if (read.report) return <DescribeFailureContent details={read.report} mode={mode} />;
  return <p className="mono-faint" role="status">{read.reading ? "Reading saved failure details…" : "Open to read saved diagnostics."}</p>;
}

/** The cell's fold: closed until asked, so a scrollback of failures costs no reads. */
export function DescribeFailureDetails({ id }: { id: string }) {
  const [open, setOpen] = useState(false);
  const read = useDescribeFailureReport(id, open);
  return <details className="result-inspection describe-failure-fold" onToggle={event => setOpen(event.currentTarget.open)}>
    <summary>Failure details</summary>
    <DescribeFailureReading read={read} mode="cell" />
  </details>;
}
