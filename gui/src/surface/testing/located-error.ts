import type { ErrorRecord, SourceLocation } from "../../protocol";
export const locations: readonly SourceLocation[] = [
  { source: "fixture.wes", start: 60, end: 68, line: 3, column: 10, endLine: 3, endColumn: 18 },
  { source: "fixture.wes", start: 52, end: 70, line: 2, column: 4, endLine: 3, endColumn: 20 },
];
export const locatedError: ErrorRecord = { id: "synthetic-error", code: "CAL005", message: "div divisor must not be zero", causeId: "", issues: [], locations };
export const locationSummary = "fixture.wes · line 3, column 10\ncalled from fixture.wes · line 2, column 4";
