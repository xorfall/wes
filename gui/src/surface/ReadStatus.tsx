import { MonoLine } from "./MonoLine";
import { WITHDRAWN_TITLE } from "./render/dataset-source";
import { resultFailure } from "../result-failure";

/**
 * A stored result being read, or why it could not be. Known refusals guide the next action;
 * a manual retry reads only this result, and is never offered after withdrawal.
 */
export function ReadStatus({ problem, problemCode, withdrawn = false, onRetry }: { problem?: string; problemCode?: string; withdrawn?: boolean; onRetry?: () => void }) {
  const failure = resultFailure(problemCode);
  return <div className="result-read-status" role="status">
    <MonoLine segments={[{ text: withdrawn ? WITHDRAWN_TITLE : problem ? `${failure?.title ?? "could not read result"} · ${problem}` : "reading result…", role: problem || withdrawn ? "mono-warn" : "mono-faint" }]} />
    {problem && !withdrawn && failure && <MonoLine segments={[{ text: failure.next, role: "mono-faint" }]} />}
    {problem && !withdrawn && failure?.canReadAgain !== false && onRetry && <button type="button" className="cell-action" onClick={onRetry}>retry reading result</button>}
  </div>;
}
