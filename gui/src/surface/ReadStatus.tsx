import { MonoLine } from "./MonoLine";
import { WITHDRAWN_TITLE } from "./render/dataset-source";

/**
 * A stored result being read, or why it could not be. An ordinary failure offers to read again; a
 * withdrawal does not, since nothing about reading again could make the result readable.
 */
export function ReadStatus({ problem, withdrawn = false, onRetry }: { problem?: string; withdrawn?: boolean; onRetry?: () => void }) {
  return <div className="result-read-status" role="status">
    <MonoLine segments={[{ text: withdrawn ? WITHDRAWN_TITLE : problem ? `could not read result · ${problem}` : "reading result…", role: problem || withdrawn ? "mono-warn" : "mono-faint" }]} />
    {problem && !withdrawn && onRetry && <button type="button" className="cell-action" onClick={onRetry}>retry reading result</button>}
  </div>;
}
