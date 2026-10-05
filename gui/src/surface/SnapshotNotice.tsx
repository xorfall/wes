import type { StoredValue } from "../protocol";

/** Metadata belongs to the displayed value, even when a replacement is being computed. */
export function SnapshotNotice({ value, onRefresh, refreshing }: { readonly value: StoredValue; readonly onRefresh?: () => void; readonly refreshing?: boolean }) {
  if (value.provenance["snapshot.kind"] !== "names") return null;
  const captured = value.provenance["snapshot.capturedAt"];
  if (!captured || !Number.isFinite(Date.parse(captured))) return null;
  return <div className="snapshot-notice mono-dim">
    <span aria-description="States and types describe the capture instant. This result does not update as other work finishes.">Snapshot · <time dateTime={captured}>{captured}</time></span>
    {onRefresh && <button type="button" className="cell-action" disabled={refreshing} onClick={onRefresh} aria-description="Run this cell again to capture a new snapshot">Refresh</button>}
    <span>Pending/running results may not have a determined type yet.</span>
  </div>;
}
