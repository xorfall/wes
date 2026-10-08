import type { WorkspaceNode } from "../workspace";
import { lineText, MonoLine } from "./MonoLine";
import { PROGRESS_NOTE, progressKind, progressRows, RECORDING_NOTE } from "./record-progress";
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
  const recording = progressKind(node) === "recording";
  const note = recording ? RECORDING_NOTE : PROGRESS_NOTE;
  return (
    <div className={`record-progress${full ? " record-progress-full" : ""}`} role="group" aria-label={recording ? "recording progress" : "analysis progress"}
      aria-description={full ? note : `${note} Whole rows${recording ? "" : " and the analysis receipt"} are under details.`}>
      {rows.map((row, at) => <MonoLine key={at} segments={row} className="record-progress-row" description={full ? undefined : lineText(row)} />)}
    </div>
  );
}
