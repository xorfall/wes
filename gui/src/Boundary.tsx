import { clientDiagnostic } from "./local-telemetry";
import { Component, type ErrorInfo, type ReactNode } from "react";

/**
 * Catches what would otherwise be a blank page.
 *
 * React unmounts the whole tree when a render throws, so without this the symptom of any mistake is an
 * empty window — which tells whoever is looking at it nothing at all, and tells whoever has to fix it
 * less. Showing the message and stack makes render failures diagnosable.
 *
 * It shows the message and where it came from, because that is what someone would have to go and find.
 */
interface Props {
  readonly children: ReactNode;
}

interface State {
  readonly failure?: Error;
  readonly where?: string;
}

export class Boundary extends Component<Props, State> {
  override state: State = {};

  static getDerivedStateFromError(failure: Error): State {
    return { failure };
  }

  override componentDidCatch(failure: Error, info: ErrorInfo): void {
    clientDiagnostic("render");
    this.setState({ failure, where: info.componentStack ?? undefined });
  }

  override render(): ReactNode {
    const { failure, where } = this.state;
    if (failure === undefined) {
      return this.props.children;
    }
    return (
      <div className="crash">
        <h1>wes stopped drawing</h1>
        <p className="crash-message">{failure.message}</p>
        {failure.stack !== undefined && <pre className="crash-detail">{failure.stack}</pre>}
        {where !== undefined && <pre className="crash-detail">{where}</pre>}
        <div className="crash-actions">
          <button type="button" onClick={() => this.setState({})}>
            Try again
          </button>
          <button
            type="button"
            onClick={() => {
              window.localStorage.removeItem("wes.settings");
              window.location.reload();
            }}
          >
            Forget settings and reload
          </button>
        </div>
        <p className="hint">
          The engine is untouched — it is in its own process, and this is only the client.
        </p>
      </div>
    );
  }
}
