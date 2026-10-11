import { describeSize, type StoredValue } from "../protocol";
import { isExactNumber, stringifyExactJson } from "../exact-json";
import type { Engine } from "../engine";
import type { WorkspaceNode } from "../workspace";
import { useState } from "react";
import { useResults } from "./results";
import { resultAccess } from "./result-access";
import { ReadStatus } from "./ReadStatus";
import { budget } from "../limits/policy";

export const GRAPH_PREVIEW_BYTES = 256 * 1024;
const ITEMS = 6;
const CHARACTERS = 4096;

/** A display excerpt, never a StoredValue or an input offered to a calculation or View. */
export function previewText(value: StoredValue): { text: string; note?: string } {
  const data = value.data;
  const short = (item: unknown) => {
    const text = typeof item === "string" ? item : stringifyExactJson(item) ?? "null";
    return text.length > 512 ? `${text.slice(0, 511)}…` : text;
  };
  let text: string;
  let note: string | undefined;
  if (typeof data === "string") {
    const lines = data.split("\n", ITEMS + 1);
    text = lines.slice(0, ITEMS).join("\n");
    if (lines.length > ITEMS || text.length > CHARACTERS) note = "Text excerpt · open result for more";
  } else if (Array.isArray(data)) {
    text = data.slice(0, ITEMS).map((row, i) => `${i + 1}  ${short(row)}`).join("\n") || "[]";
    note = `Showing ${Math.min(data.length, ITEMS)} of ${data.length} items`;
  } else if (data && typeof data === "object" && !isExactNumber(data)) {
    const entries = Object.entries(data);
    text = entries.slice(0, ITEMS).map(([name, item]) => `${name}: ${short(item)}`).join("\n") || "{}";
    note = `Showing ${Math.min(entries.length, ITEMS)} of ${entries.length} fields`;
  } else text = short(data);
  if (text.length > CHARACTERS) { text = `${text.slice(0, CHARACTERS - 1)}…`; note = "Value excerpt · open result for more"; }
  return { text, note };
}

export function GraphPreview({ engine, generation, node }: { engine: Engine; generation?: string; node: WorkspaceNode }) {
  const [requested, setRequested] = useState(false);
  const access = resultAccess(node);
  const permitted = access.current && !access.live && !node.accessWithdrawn && node.publication?.state !== "unavailable";
  const maximum = Math.min(GRAPH_PREVIEW_BYTES, budget("codec.bytes"));
  const large = node.bytes !== undefined && node.bytes > maximum;
  const wanted = permitted && !large && (node.bytes !== undefined || requested) && node.handle ? [node.handle] : [];
  const { held, reads, retry } = useResults(engine, generation, wanted, [], maximum);
  const value = node.handle && wanted.length ? held.get(node.handle) : undefined;
  const read = node.handle ? reads.get(node.handle) : undefined;
  let message: string | undefined;
  if (node.accessWithdrawn) message = "Result · Access withdrawn";
  else if (node.doubt) message = "Outcome uncertain · inspect the result before any new operation";
  else if (access.live) message = "Live result · open result to follow";
  else if (!permitted || !node.handle) message = `No readable value · ${node.state}`;
  else if (large) message = `${describeSize(node.bytes!)} · too large for this preview · open result to inspect`;
  const excerpt = value ? previewText(value) : undefined;
  return <section className="graph-preview" aria-label="Value preview">
    <span className="screen-label">Value preview</span>
    {access.partial && <p className="mono-warn">Incomplete · partial result</p>}
    {node.state === "stale" && <p className="mono-warn">Stale · last result</p>}
    {(node.state === "running" || node.state === "pending") && value && <p className="mono-warn">Updating · previous result</p>}
    {node.private && <p className="mono-faint">Private result</p>}
    {message ? <p className="mono-faint" role="status">{message}</p> : excerpt ? <>
      <pre className="graph-preview-value">{excerpt.text}</pre>
      {excerpt.note && <p className="mono-faint graph-preview-note">{excerpt.note}</p>}
    </> : wanted.length ? <ReadStatus {...read} onRetry={node.handle ? () => retry(node.handle!) : undefined} />
      : <button type="button" className="cell-action" onClick={() => setRequested(true)}>read bounded preview</button>}
  </section>;
}
