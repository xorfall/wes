import type { ResultObservation } from "./results";

/**
 * Observation status of a displayed value; changes never replace the value it describes.
 *
 * Live streams reserve an overlay line so samples never resize the stream. Finite results use the
 * `inline` variant: a flow row of its own above the value, present only while there is something
 * to say, so a current preview carries no blank status height.
 */
export function ObservationStatus({ observation, onRetry, inline = false }: { observation?: ResultObservation; onRetry?: () => void; inline?: boolean }) {
  const pending = observation !== undefined && observation.state !== "current";
  const label = observation?.state === "stale" ? "Stale · previous result" : "Updating · previous result";
  return <span className={`result-observation-status${inline ? " result-observation-status-inline" : ""}`} role="status" aria-description={observation?.problem ?? observation?.staleReason}
    aria-hidden={!pending} data-state={pending ? observation.problem ? "problem" : observation.state : "current"}>
    <span className="result-observation-label">{pending ? observation.problem ? `Previous result · ${observation.problem}` : label : ""}</span>
    {pending && observation.problem && onRetry && <button type="button" className="cell-action" onClick={onRetry}>retry reading result</button>}
  </span>;
}
