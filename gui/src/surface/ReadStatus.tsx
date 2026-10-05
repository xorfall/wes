import { MonoLine } from "./MonoLine";

export function ReadStatus({ problem, onRetry }: { problem?: string; onRetry?: () => void }) {
  return <div className="result-read-status" role="status">
    <MonoLine segments={[{ text: problem ? `could not read result · ${problem}` : "reading result…", role: problem ? "mono-warn" : "mono-faint" }]} />
    {problem && onRetry && <button type="button" className="cell-action" onClick={onRetry}>retry reading result</button>}
  </div>;
}
