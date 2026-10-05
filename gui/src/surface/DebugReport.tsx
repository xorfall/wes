/**
 * `/debug`'s report, mounted read only inside the cell that asked for it.
 *
 * The text is whatever the engine's storage reply and the client's own persistence came back as —
 * this draws it in the Surface's themed, monospaced voice and nothing more; deciding what belongs
 * in the report is the command's business, not this component's.
 */
import "./surface.css";
import "./session.css";

export interface DebugReportProps {
  readonly report: string;
}

export function DebugReport({ report }: DebugReportProps) {
  return (
    <pre className="debug-report" tabIndex={0} aria-label="Storage debug report">
      {report}
    </pre>
  );
}
