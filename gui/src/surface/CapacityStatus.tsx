import type { ExecutionCapacity } from "../protocol";
import "./capacity-status.css";

/** Occupied backend leases across all workspaces; never inferred from visible cells. */
export function CapacityStatus({ capacity }: { readonly capacity?: ExecutionCapacity }) {
  if (!capacity) return null;
  return <details className="capacity-status">
    <summary aria-label={`Execution capacity: operations ${capacity.operations.used}/${capacity.operations.limit}, streams ${capacity.streams.used}/${capacity.streams.limit}`}>
      <span className={capacity.operations.used === capacity.operations.limit ? "mono-warn" : "mono-dim"}>operations {capacity.operations.used}/{capacity.operations.limit}</span>
      <span className="mono-faint"> · </span>
      <span className={capacity.streams.used === capacity.streams.limit ? "mono-warn" : "mono-dim"}>streams {capacity.streams.used}/{capacity.streams.limit}</span>
    </summary>
    <div className="capacity-explanation">
      <strong>Across all workspaces</strong>
      <p>Operations hold a slot while a call or calculation executes. Opening a stream also takes an operation slot, released once the stream opens. Interactive conversations keep their operation slot until they finish.</p>
      <p>Streams reserve a separate slot before opening and keep it until cleanup finishes, including while closing. Stream events can trigger calculations that use operation slots again.</p>
      <p>When operations are full, work waits. When streams are full, a new stream is refused before entering the provider. Existing streams continue.</p>
      <p>Startup limits: <code>--concurrency {capacity.operations.limit}</code> · <code>--max-streams {capacity.streams.limit}</code></p>
    </div>
  </details>;
}
