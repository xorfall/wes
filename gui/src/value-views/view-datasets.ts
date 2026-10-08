/**
 * The host side of a View's Dataset page reads.
 *
 * A sandboxed View asks, through its own message port, for a page of a Dataset field of its input:
 * a JSON Pointer inside that input and a position. The host never takes an address, identifier or
 * session from the View. It resolves the pointer against the declared input contract and the input
 * it actually drew, and reads through the one source it already holds for that drawing:
 *
 * - a View member of a drawn frame reads `/view-datasets/{root}/{instance}/{member}`, bound to the
 *   frame's root, the member and its render and input revisions;
 * - a value renderer over a stored result reads `/datasets/{handle}` at its path in that result.
 *
 * Anything else has no source, and the View is told `unavailable`. Replies are checked against the
 * reference the input names, rows are decoded against the declared element contract, at most two
 * reads are in flight per drawing, and a new input, a closed frame, a withdrawal or a session change
 * settles every read in flight; a late reply is never delivered.
 */
import { createContext } from "react";
import type { Engine } from "../engine";
import { DatasetReadError, type DatasetPosition, type DatasetRead } from "../dataset-read";
import { stringifyExactJson } from "../exact-json";
import type { DatasetReference } from "../presentation/dataset";
import type { DatasetSource } from "../surface/render/dataset-source";
import { decodeContract, type ViewDefinition } from "./definition";
import type { ViewFrame } from "./instances";

/** Exactly what the host drew for one View member: the frame it came from and its revisions. */
export interface ViewDatasetBinding {
  readonly engine: Engine;
  readonly generation: string;
  /** The frame root's node and instance identity, as the frame was read. */
  readonly root: string;
  readonly rootInstance: string;
  /** The drawn member and the revisions it was drawn at. */
  readonly member: string;
  readonly revision: string;
  readonly inputRevision: string;
  /** Input fields bound from other results; the server reads only the member's own input. */
  readonly linkedInputs: readonly string[];
}

/** The binding of the member drawn at `view/{id}` in the frame currently presented, if any. */
export type ViewDatasetBindings = (path: string) => ViewDatasetBinding | undefined;
export const ViewDatasetContext = createContext<ViewDatasetBindings | undefined>(undefined);

/** Bindings for every member of `frame`, read from that frame only. */
export function frameBindings(frame: ViewFrame, rootInstance: string, engine: Engine, generation: string): ViewDatasetBindings {
  const members = new Map(frame.instances.map(entry => [`view/${entry.id}`, entry]));
  return path => {
    const entry = members.get(path);
    if (!entry || !entry.input) return undefined;
    return { engine, generation, root: frame.root, rootInstance, member: entry.id, revision: entry.revision, inputRevision: entry.inputRevision, linkedInputs: entry.linkedInputs };
  };
}

export type DatasetErrorCode = "unavailable" | "busy" | "changed" | "withdrawn" | "limit" | "invalid" | "failed";

/** Where a drawing's reads go: one of the two real sources, or none. */
export type DatasetRoute =
  | { readonly kind: "frame"; readonly binding: ViewDatasetBinding }
  | { readonly kind: "stored"; readonly source: DatasetSource; readonly path: string }
  | { readonly kind: "none" };

const MAX_SELECT = 2048;
const DECIMAL = /^(?:0|[1-9][0-9]{0,19})$/;
const CURSOR = /^[A-Za-z0-9_-]{1,4096}$/;
const INDEX = /^(?:0|[1-9][0-9]*)$/;
export const DATASET_READS_PER_DRAWING = 2;
const DEFAULT_LIMIT = 50;

/** A request as the View sent it, or undefined when it is not exactly one. */
export function viewDatasetRequest(message: Record<string, unknown>):
  { request: number; operation: "page" | "inspect"; select: string; position?: DatasetPosition; limit?: number } | undefined {
  const allowed = new Set(["kind", "request", "operation", "select", "position", "limit"]);
  if (Object.keys(message).some(key => !allowed.has(key))) return undefined;
  const { request, operation, select, position, limit } = message;
  if (typeof request !== "number" || !Number.isSafeInteger(request) || request < 1) return undefined;
  if (operation !== "page" && operation !== "inspect") return undefined;
  if (typeof select !== "string" || select.length > MAX_SELECT || (select !== "" && !select.startsWith("/"))) return undefined;
  if (operation === "inspect") return position === undefined && limit === undefined ? { request, operation, select } : undefined;
  let checked: DatasetPosition | undefined;
  if (position !== undefined) {
    if (typeof position !== "object" || position === null || Array.isArray(position) || Object.keys(position).length !== 1) return undefined;
    const raw = position as Record<string, unknown>;
    if (typeof raw.from === "string" && DECIMAL.test(raw.from) && BigInt(raw.from) <= 18446744073709551615n) checked = { from: raw.from };
    else if (typeof raw.cursor === "string" && CURSOR.test(raw.cursor)) checked = { cursor: raw.cursor };
    else return undefined;
  }
  if (limit !== undefined && (typeof limit !== "number" || !Number.isInteger(limit) || limit < 1 || limit > 100)) return undefined;
  return { request, operation, select, position: checked ?? { from: "0" }, limit: (limit as number | undefined) ?? DEFAULT_LIMIT };
}

/**
 * The declared row contract and the snapshot reference at `select` in the drawn input, or
 * undefined when the pointer does not reach a declared Dataset field. Only record fields and list
 * indices are stepped through, as the server does; an Option on the way is refused.
 */
export function datasetTarget(definition: ViewDefinition, input: unknown, select: string, linked: readonly string[] = []):
  { readonly element: string; readonly reference: DatasetReference } | undefined {
  if (select !== "" && !select.startsWith("/")) return undefined;
  const segments = select === "" ? [] : select.slice(1).split("/").map(part => part.replace(/~1/g, "/").replace(/~0/g, "~"));
  // A linked field is bound from another result; this member's input does not hold it.
  if (segments[0] !== undefined && linked.includes(segments[0])) return undefined;
  let name = definition.input, data = input;
  for (const segment of segments) {
    const schema = definition.contracts[name];
    if (!schema || data === null || typeof data !== "object") return undefined;
    if (schema.kind === "record") {
      const field = schema.fields?.[segment];
      if (!field || !Object.hasOwn(data, segment)) return undefined;
      name = field.type; data = (data as Record<string, unknown>)[segment];
    } else if (schema.kind === "list") {
      if (!INDEX.test(segment) || !Array.isArray(data) || Number(segment) >= data.length) return undefined;
      name = schema.element!; data = data[Number(segment)];
    } else return undefined;
  }
  const schema = definition.contracts[name];
  if (schema?.kind !== "dataset" || !schema.element || data === null || typeof data !== "object") return undefined;
  const reference = (data as { kind?: unknown; reference?: DatasetReference }).reference;
  return (data as { kind?: unknown }).kind === "dataset" && reference ? { element: schema.element, reference } : undefined;
}

function code(error: unknown): DatasetErrorCode {
  if (!(error instanceof DatasetReadError)) return "failed";
  switch (error.kind) {
    case "busy": return "busy";
    case "withdrawn": return "withdrawn";
    case "session": return "changed";
    case "limit": return "limit";
    case "invalid": return "invalid";
    case "missing": return "unavailable";
    case "failed": return "failed";
    // A View reads only its frozen input; a host continuity refusal is an ordinary bounded,
    // non-retryable read failure there, never a workspace change.
    case "continuity": return "failed";
  }
}

/** The page as the View receives it: rows decoded against the declared element contract. */
function shaped(definition: ViewDefinition, element: string, read: DatasetRead): unknown {
  // Recording coverage is the snapshot's own read-only receipt; its run and epoch identities stay with the host.
  const recording = read.recording && { first: read.recording.first, committedThrough: read.recording.committedThrough, acceptedThrough: read.recording.acceptedThrough,
    pending: read.recording.pending, rejected: read.recording.rejected, termination: read.recording.termination };
  const snapshot = { generation: read.reference.generation, records: read.reference.records, lifecycle: read.lifecycle, ...(recording ? { recording } : {}) };
  if (!read.page) return { ...snapshot, protected: read.protected, segmentBytes: read.segmentBytes };
  const page = read.page;
  return { ...snapshot, first: page.first, next: page.next, extentExhausted: page.extentExhausted, cursor: page.cursor,
    rows: page.rows.map(row => ({ ordinal: row.ordinal, sourceStart: row.sourceStart, sourceEnd: row.sourceEnd,
      value: decodeContract(definition, element, row.value.data, true, row.value.type) })) };
}

/**
 * One drawing's reads. `reset` is called whenever the drawn input, its binding or the session
 * changes and when the drawing closes: every read in flight is aborted and told `changed`, and its
 * result, should it still arrive, is dropped.
 */
export class DatasetBridge {
  private epoch = 0;
  private readonly inFlight = new Map<number, AbortController>();
  constructor(private readonly reply: (text: string) => void, private readonly maxBytes: number) {}

  /** The epoch the drawing is at; it is delivered with each render so the View can tell. */
  current(): number { return this.epoch; }

  reset(): void {
    this.epoch++;
    for (const [request, controller] of this.inFlight) { controller.abort(); this.send({ kind: "dataset-reply", request, ok: false, error: "changed" }); }
    this.inFlight.clear();
  }

  close(): void {
    this.epoch++;
    for (const controller of this.inFlight.values()) controller.abort();
    this.inFlight.clear();
  }

  handle(message: Record<string, unknown>, definition: ViewDefinition, input: unknown, route: DatasetRoute): void {
    const request = viewDatasetRequest(message);
    const fail = (error: DatasetErrorCode, id = message.request) => this.send({ kind: "dataset-reply", request: id, ok: false, error });
    if (!request) return fail("invalid");
    if (route.kind === "none") return fail("unavailable");
    if (this.inFlight.size >= DATASET_READS_PER_DRAWING || this.inFlight.has(request.request)) return fail("busy");
    const target = datasetTarget(definition, input, request.select, route.kind === "frame" ? route.binding.linkedInputs : []);
    if (!target) return fail("invalid");
    const controller = new AbortController(), epoch = this.epoch;
    this.inFlight.set(request.request, controller);
    const position = request.operation === "page" ? request.position : undefined;
    const limit = request.limit ?? DEFAULT_LIMIT;
    const reading = route.kind === "frame"
      ? route.binding.engine.readViewDataset(route.binding, target.reference, request.select, position, limit, controller.signal)
      : route.source.engine.readDataset(route.source.handle, route.source.generation, target.reference, `${route.path}${request.select}`, position, limit, controller.signal);
    void reading.then(read => {
      if (!this.owns(request.request, controller, epoch)) return;
      this.inFlight.delete(request.request);
      let result: unknown;
      try { result = shaped(definition, target.element, read); } catch { return fail("invalid", request.request); }
      this.send({ kind: "dataset-reply", request: request.request, ok: true, result }, request.request);
    }, error => {
      if (!this.owns(request.request, controller, epoch)) return;
      this.inFlight.delete(request.request);
      fail(code(error), request.request);
    });
  }

  private owns(request: number, controller: AbortController, epoch: number): boolean {
    return epoch === this.epoch && !controller.signal.aborted && this.inFlight.get(request) === controller;
  }

  private send(message: Record<string, unknown>, request?: number): void {
    let text = stringifyExactJson(message);
    if (new TextEncoder().encode(text).length > this.maxBytes) text = stringifyExactJson({ kind: "dataset-reply", request: request ?? message.request, ok: false, error: "limit" });
    try { this.reply(text); } catch { /* A closed port receives nothing. */ }
  }
}
