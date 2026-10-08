import { resultArrangement } from "../cells";
import { resultAccess } from "./result-access";
import { SnapshotNotice } from "./SnapshotNotice";
import { LiveView } from "./LiveView";
import { issueText } from "../failure-text";
/**
 * Cell output: one block per node, wherever the cell is drawn.
 *
 * Run state precedes value shape, in this order and for each node on its
 * own: a conversation that is asking, a failure, a skipped stage, a cancellation, work with no
 * result yet, a result still being read (or whose read failed), a recipe nothing has run — and only
 * then the value itself, handed to the presentation layer. Expanding never runs a recipe, makes a
 * provider call or restarts a stopped stream; a running node the GUI cannot read is not drawn as if
 * it could.
 */
import type { ReactNode } from "react";
import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";
import type { Workspace, WorkspaceNode } from "../workspace";
import { formatExecutionDuration } from "../presentation/format";
import { matchesHttp } from "../views/http-response";
import { summarize } from "../presentation/summary";
import { presentationType } from "../presentation/prepare";
import { PREVIEW_LINES } from "../presentation/types";
import type { CellBlock } from "./Cell";
import type { Failure } from "./session-model";
import { readInteractive } from "./forms/Interactive";
import { InteractiveResponse } from "./InteractiveResponse";
import { conversationKey, createInteractiveState, type InteractiveStates } from "./interactive-state";
import { MonoLine, type Segment } from "./MonoLine";
import { ReadStatus } from "./ReadStatus";
import { NoticeBlock } from "./render/NoticeBlock";
import { ValueBlock } from "./render/ValueBlock";
import { isViewInstance } from "../value-views/InstanceView";
import type { ResultRead, ResultObservation } from "./results";
import { ObservationStatus } from "./ObservationStatus";
import type { NodeView, SessionCell } from "./session-model";
import { DescribeFailureDetails, describeReportId } from "./DescribeFailureDetails";

export interface OutputInput {
  readonly onRefresh?: () => void;
  readonly interactiveStates?: InteractiveStates;
  readonly cell: SessionCell;
  readonly workspace: Workspace;
  readonly held: ReadonlyMap<string, StoredValue>;
  readonly reads: ReadonlyMap<string, ResultRead>;
  readonly observations?: ReadonlyMap<string, ResultObservation>;
  readonly retryRead: (handle: string) => void;
  readonly engine: Engine;
  readonly generation: string | undefined;
}

function line(segments: readonly Segment[]): ReactNode {
  return <MonoLine segments={segments} className="value-line" />;
}

/** The block of a node that has no value to present: what it is doing instead. */
function stateBlock(node: WorkspaceNode, input: OutputInput): ReactNode | undefined {
  if (node.interactive && node.run && node.conversationActive && node.state === "running") {
    const key = conversationKey(node.id, node.run);
    let state = input.interactiveStates?.get(key);
    if (!state && input.interactiveStates) {
      state = createInteractiveState();
      input.interactiveStates.set(key, state);
    }
    const question = (node.wrote ?? "").split("\n").filter((it) => it.trim() !== "").at(-1) ?? "";
    return <InteractiveResponse key={`${input.generation}:${node.id}:${node.run}`} state={state} engine={input.engine} node={node.id} run={node.run}
      model={readInteractive({ type: { kind: "unknown" }, data: undefined, asking: question || "process input", wrote: node.wrote })} />;
  }
  if(node.doubt)return line([{text:"outcome unknown · no current value",role:"mono-warn"}]);
  if (node.state === "skipped") return line([{ text: "skipped · an earlier stage failed", role: "mono-faint" }]);
  if (node.state === "cancelled" && !node.stopped) return line([{ text: `cancelled · ${node.cancellation?.reason ?? "stopped before a result"}`, role: "mono-warn" }]);
  const observation = input.observations?.get(node.id);
  if (observation && observation.state !== "current") return undefined;
  if (node.state === "running") return line([{ text: "● running", role: "mono-meta" }, { text: " · the GUI cannot read a running result yet; it appears when the command finishes or stops", role: "mono-faint" }]);
  if (node.state === "pending") return line([{ text: node.waiting?.length ? node.waiting.map((wait) => wait.message).join(" · ") : "waiting for its inputs", role: "mono-faint" }]);
  if (node.handle && !input.held.has(node.handle)) {
    const handle = node.handle;
    return <ReadStatus problem={input.reads.get(handle)?.problem} onRetry={() => input.retryRead(handle)} />;
  }
  const value = node.handle ? input.held.get(node.handle) : undefined;
  if (value?.type?.kind === "iter") {
    return line([{ text: "◇ recipe", role: "mono-meta" }, { text: " · nothing run yet — collect it explicitly to run it", role: "mono-warn" }]);
  }
  if (!value) return node.state === "stale" ? line([{ text: "stale · the value was not re-run", role: "mono-warn" }]) : line([{ text: "no result", role: "mono-faint" }]);
  return undefined;
}

/** Data facts are independent of the type's structured disclosure. */
function headerOf(value: StoredValue | undefined): Segment[] {
  const shape = value?.type;
  const http = shape ? matchesHttp(shape, value?.data) : false;
  const facts = summarize(shape, value?.data);
  const record = shape?.kind === "record" && !shape.name && typeof value?.data === "object" && value.data !== null && !Array.isArray(value.data);
  const shown = record && facts.length === 0 ? [{ text: `${Object.keys(value.data as object).length} fields`, tone: "dim" as const }] : facts;
  return shown.flatMap((run, at) => [
    ...(at ? [{ text: " · ", role: "mono-faint" as const }] : []),
    { text: Array.isArray(value?.data) ? `${run.text} ${shape?.kind === "list" && shape.element.kind === "record" ? "rows" : "items"}` : http ? `HTTP ${run.text}` : run.text, role: run.tone === "ok" ? "mono-ok" as const : run.tone === "warn" ? "mono-warn" as const : "mono-dim" as const },
  ]);
}

/**
 * What a failure's block says. A failure is one record — code, message, span — and every part of
 * it is said once: when the verdict speaks for the node (a lone node), it lifts the headline
 * (`headlineOf`) and the block draws only the rest; among several nodes the block owns the whole
 * record. No rows left means no block.
 */
export function failureRows(failure: Failure, lifted: boolean): Segment[][] {
  const rows: Segment[][] = [];
  if (!lifted) rows.push([...(failure.code ? [{ text: failure.code, role: "mono-bad" as const }, { text: " · ", role: "mono-faint" as const }] : []), { text: failure.message, role: "mono-bad" }]);
  else if (failure.code) rows.push([{ text: failure.message, role: "mono-bad" }]);
  if (failure.span) {
    for (const text of failure.span.split("\n")) rows.push([{ text, role: "mono-faint" }]);
  }
  for (const issue of failure.issues ?? []) rows.push([{ text: issueText(issue), role: "mono-bad" }]);
  return rows;
}

/**
 * The blocks of one cell. A single node is always open; of several, the failed ones, the named ones
 * and the last are open. Every result has its own six-line preview and arrangement.
 */
export function cellBlocks(input: OutputInput): CellBlock[] {
  const { cell, workspace } = input;
  const nodes = cell.nodes.map((view) => ({ view, node: workspace.nodes.find((it) => it.id === view.id) }))
    .filter((it): it is { view: NodeView; node: WorkspaceNode } => it.node !== undefined);
  // Cardinality is the actual nodes: a notice beside one node does not make it a stage among several.
  const several = nodes.length > 1;
  // The verdict speaks for a lone node, except when it describes a newer attempt that was not run.
  const lifted = nodes.length === 1 && !cell.previousAttemptResults;
  const opens = (node: WorkspaceNode, at: number) => !several || node.state === "failed" || node.name !== undefined && node.name !== "" || at === nodes.length - 1;
  const mode = cell.view === "expanded" ? "expanded" : "preview";
  const blocks: CellBlock[] = [];
  if (cell.previousAttemptResults) blocks.push({ key: "previous-attempt", zone: "run", open: true,
    content: <p className="mono-dim">Previous results · the latest attempt was not run.</p> });
  if (hasNotice(cell)) {
    const lines = cell.documentNotice ? cell.documentNotice.split("\n") : (cell.notices ?? []).flatMap((notice) => notice.message.split("\n"));
    const severity = cell.notices?.some((notice) => notice.severity === "warning") ? "warning" : "info";
    blocks.push({ key: "notices", zone: "run", open: true, content: <NoticeBlock lines={lines} severity={severity} mode={mode} /> });
  }
  nodes.forEach(({ view, node }, at) => {
    const arrangement = resultArrangement(cell, node.id);
    const mode = arrangement.view === "expanded" ? "expanded" : "preview";
    const failure = node.state === "failed" ? failureRows(view.failure ?? { message: node.failure ?? "" }, lifted) : undefined;
    const report = describeReportId(node.failureRecord);
    const observation = input.observations?.get(node.id);
    const value = observation?.value ?? (node.handle ? input.held.get(node.handle) : undefined);
    const displayHandle = observation?.handle ?? node.handle;
    const viewId=value&&isViewInstance(value)&&typeof value.data==="object"&&value.data!==null&&"id" in value.data?String(value.data.id):undefined;
    const instanceDisplay=viewId?{label:`$${workspace.nodes.find(candidate=>candidate.id===viewId)?.name||viewId}`,
      referenceOnly:/^\s*:view\s+(connect|disconnect|bind)\b/u.test(node.command)}:undefined;
    const status = !failure && value && displayHandle
      ? <ObservationStatus observation={observation} inline={!cell.streamOutput} onRetry={node.handle ? () => input.retryRead(node.handle!) : undefined} /> : undefined;
    const live=Boolean(input.generation && resultAccess(node).live);
    // Among several stages each failure names its own stage, so failures are never read together.
    const failureContent = failure ? <div className="value-error">{several && <MonoLine segments={[{ text: "✗ ", role: "mono-bad" }, { text: view.label, role: "mono-ref" }]} className="value-line value-error-stage" />}{failure.map((row, at) => <MonoLine key={at} segments={row} className="value-line" />)}{report && <DescribeFailureDetails key={report} id={report} />}</div> : undefined;
    const content = (live && input.generation
        ? <LiveView mode={mode} collapsed={arrangement.view==="collapsed"} key={`${input.generation}:${node.id}:${node.command}:${JSON.stringify(node.environment)}`} engine={input.engine} generation={input.generation} node={node} workspace={workspace.identity?.name} sourceLabel={source => { const named = workspace.nodes.find(candidate => candidate.id === source)?.name; return named ? `$${named}` : source; }} />
        : undefined)
      ?? stateBlock(node, input) ?? (value && displayHandle
      ? <div className={`result-observation${cell.streamOutput ? "" : " result-observation-inline"}`}>{cell.streamOutput && status}<SnapshotNotice value={value} onRefresh={input.onRefresh} refreshing={node.state === "pending" || node.state === "running"} /><ValueBlock engine={input.engine} value={value} cacheKey={`${input.generation ?? ""}:${displayHandle}`} bindingKey={`${input.generation ?? ""}:${node.id}`} mode={mode} collapsed={arrangement.view === "collapsed"}
          instanceDisplay={instanceDisplay} facts={node.stopped ? STOPPED : WHOLE} lines={PREVIEW_LINES} /></div>
      : line([{ text: "no result", role: "mono-faint" }]));
    if(failure && (failure.length || report))blocks.push({key:`${node.id}:failure`,zone:"run",open:true,content:failureContent});
    // A lone failed job has no result: its record is in the run zone and no empty result card follows.
    if (failure && !live && lifted) return;
    blocks.push({ key: node.id, stream: live && input.generation ? {engine:input.engine,generation:input.generation,node} : undefined, identity: view, row: view.row, header: headerOf(value), type: value ? presentationType(value) : undefined, meta: value?.meta, typeLabel: view.type,
      // A lone node's duration is the run summary's; only stages carry their own.
      duration: several && view.durationMs !== undefined ? formatExecutionDuration(view.durationMs) : undefined, view: arrangement.view, height: arrangement.rows, hasValue: live || resultAccess(node).current && node.state !== "failed" && node.state !== "skipped" && !(node.state === "cancelled" && !node.stopped) && Boolean(value || node.streamOutput), status: <>{node.private && <span className="mono-warn">Private · memory only</span>}{cell.streamOutput ? undefined : status}</>, open: opens(node, at), content: failure && !live ? <p className="mono-faint">no value · the run failed</p> : content });
  });
  return blocks;
}

const STOPPED = { stopped: true, whole: true } as const;
const WHOLE = { whole: true } as const;

function hasNotice(cell: SessionCell): boolean {
  return Boolean(cell.documentNotice) || (cell.notices?.length ?? 0) > 0;
}
