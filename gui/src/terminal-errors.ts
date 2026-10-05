/** Authority has ended; retrying the same terminal ID cannot recover it. */
export class TerminalUnavailable extends Error {
  constructor() {
    super("Terminal session is no longer available. Start a new terminal to continue.");
    this.name = "TerminalUnavailable";
  }
}

/** Classified at the request boundary, without retaining command/input payloads. */
export class TerminalRequestError extends Error {
  constructor(readonly code: string, readonly source: string, readonly operation: string, message: string, readonly detail?: string) {
    super(message); this.name = "TerminalRequestError";
  }
}
