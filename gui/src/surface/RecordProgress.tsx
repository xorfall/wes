import type { WorkspaceNode } from "../workspace";
import { lineText, MonoLine } from "./MonoLine";
import { PROGRESS_NOTE, progressRows } from "./record-progress";
import "./record-progress.css";

/**
 * The run's own three status rows.
 *
 * Compact (cell, inspector header area, /open result tabs): fixed height, each row clipped with an
 * ellipsis and described whole. `full` is the details path: the same rows, wrapped, nothing cut.
 */
export function RecordProgress({ node, full = false }: { readonly node: WorkspaceNode | undefined; readonly full?: boolean }) {
  const rows = progressRows(node);
  if (!rows) return null;
  return (
    <div className={`record-progress${full ? " record-progress-full" : ""}`} role="group" aria-label="analysis progress"
      aria-description={full ? PROGRESS_NOTE : `${PROGRESS_NOTE} Whole rows and the analysis receipt are under details.`}>
      {rows.map((row, at) => <MonoLine key={at} segments={row} className="record-progress-row" description={full ? undefined : lineText(row)} />)}
    </div>
  );
}
