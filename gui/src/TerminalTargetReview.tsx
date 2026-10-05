import type { TerminalTarget } from "./terminal-target";
import "./terminal-review.css";
export type TargetSummary = { kind: string; destination: string; shell: string; cwd: string | null; variables: string[]; transport: Record<string, string | null> };
export type TerminalReview = {
  target: TerminalTarget; previousRevision: string; previousAvailable: boolean;
  before: TargetSummary | null; after: TargetSummary; providers: string[];
  changes: null | { added: string[]; removed: string[]; updated: string[]; configuration: string[];
    credentialReferences: string[]; targetChanged: boolean; variables: string[] | null };
};
const list = (names: string[]) => names.length ? names.join(", ") : "None";
function Destination({ value }: { value: TargetSummary | null }) {
  return value ? <dl>
    <dt>Transport</dt><dd>{value.kind}</dd><dt>Destination</dt><dd>{value.destination}</dd>
    <dt>Shell</dt><dd>{value.shell}</dd><dt>Directory</dt><dd>{value.cwd ?? "Default"}</dd>
    {Object.entries(value.transport).map(([key, value]) => <div key={key}><dt>{key}</dt><dd>{value ?? "Default"}</dd></div>)}
    <dt>Variable names</dt><dd>{list(value.variables)}</dd>
  </dl> : <p>Previous target unavailable.</p>;
}
/** Review is local to this terminal pane; it never grants environment execution permission. */
export function TerminalTargetReview({ review, busy, onConfirm, onCancel }: {
  review: TerminalReview; busy: boolean; onConfirm: () => void; onCancel: () => void;
}) {
  const changes = review.changes;
  return <section className="terminal-target-review" aria-label="Review environment changes">
    <h3>Environment changed · {review.target.environment}</h3>
    <p>No terminal has started. Review the changes before using target <strong>{review.target.target}</strong>.</p>
    {!review.previousAvailable && <p>The previous revision is no longer retained. Changes cannot be compared; the current destination and providers are shown below.</p>}
    <div className="terminal-review-comparison">
      <section><h4>Previously selected</h4><code>{review.previousRevision}</code><Destination value={review.before} /></section>
      <section><h4>Current</h4><code>{review.target.revision}</code><Destination value={review.after} /></section>
    </div>
    {changes && <dl>
      <dt>Providers added</dt><dd>{list(changes.added)}</dd>
      <dt>Providers removed</dt><dd>{list(changes.removed)}</dd>
      <dt>Providers updated</dt><dd>{list(changes.updated)}</dd>
      <dt>Configuration keys changed</dt><dd>{list(changes.configuration)}</dd>
      <dt>Credential references changed</dt><dd>{list(changes.credentialReferences)}</dd>
      <dt>Target changed</dt><dd>{changes.targetChanged ? "Yes" : "No"}</dd>
      <dt>Variables changed</dt><dd>{changes.variables ? list(changes.variables) : "Previous target unavailable"}</dd>
    </dl>}
    <p>Current providers: {list(review.providers)}</p>
    <p className="hint">Secret and variable values are hidden. This approval applies only to the current revision above.</p>
    <div className="terminal-review-actions"><button disabled={busy} onClick={onConfirm}>Start with current settings</button><button disabled={busy} onClick={onCancel}>Cancel</button></div>
  </section>;
}
