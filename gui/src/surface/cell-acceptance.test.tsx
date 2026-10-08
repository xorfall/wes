import { httpValue } from "../testing/http-response";
import { locatedError, locationSummary } from "./testing/located-error";
import { describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { readFileSync } from "node:fs";
import type { Engine } from "../engine";
import type { StoredValue, TypeShape } from "../protocol";
import { emptyWorkspace, type Workspace, type WorkspaceNode } from "../workspace";
import type { Cell as ClientCell } from "../cells";
import { cellBlocks } from "./cell-output";
import { InteractiveResponse } from "./InteractiveResponse";
import { DescribeFailureDetails } from "./DescribeFailureDetails";
import { MonoLine, lineText } from "./MonoLine";
import { ReadStatus } from "./ReadStatus";
import { durationsOf, failureOf, joined, readSession, type SessionCell } from "./session-model";
import { ValueBlock } from "./render/ValueBlock";
import { Cell, runAvailability } from "./Cell";

/*
 * Cell acceptance over the session model and the block builder. Synthetic workspaces only:
 * invented nodes, commands and values, never a user's.
 */

const INT: TypeShape = { kind: "primitive", name: "INT" };
const node = (over: Partial<WorkspaceNode> & { id: string }): WorkspaceNode => ({
  command: `:calc { return 1; }`, dependsOn: [], state: "ready", provenance: {}, cautions: [], kept: true, ...over,
});
const client = (text: string, nodes: string[], over: Partial<ClientCell> = {}): ClientCell => ({
  id: "c1", text, lastRun: "c1", state: "answered", nodes, diagnostics: [], pinned: false, view: "preview", ...over,
});
const log = (node: string, run: string, state: string, at: string) =>
  ({ event: "log" as const, durable: true, persistenceProblem: "", record: { id: `${node}-${state}-${at}`, node, run, at, state: state as never, error: [] as const } });

function session(workspace: Workspace, cell: ClientCell, held = new Map<string, StoredValue>()): SessionCell {
  return readSession({ workspace, cells: [cell], held, context: { workspace: "synthetic", connection: "connected" } }, new Date("2026-09-26T10:00:00Z")).cells[0]!;
}

const engine = { fetch: vi.fn(), cancel: vi.fn(), answer: vi.fn(), eof: vi.fn(), rerun: vi.fn() } as unknown as Engine;

it("projects per-result arrangements without pooling sibling preview budgets or executing values", () => {
  const ids = ["a", "b", "c", "d"];
  const workspace = { ...emptyWorkspace, nodes: ids.map(id => node({ id, handle: id, type: "Int" })) };
  const held = new Map(ids.map(id => [id, { type: INT, data: 1, provenance: {} }]));
  const model = session(workspace, client("synthetic", ids, { results: { a: { view: "expanded", rows: 20 }, b: { view: "collapsed" } } }), held);
  const { blocks, tree } = blocksOf(model, workspace, held);
  expect(blocks.map(block => block.view)).toEqual(["expanded", "collapsed", "preview", "preview"]);
  const values = tree.root.findAllByType(ValueBlock);
  expect(values.map(value => [value.props.mode, value.props.collapsed, value.props.lines])).toEqual([["expanded", false, 6], ["preview", true, 6], ["preview", false, 6], ["preview", false, 6]]);
  expect(blocks[0]!.height).toBe(20);expect(engine.rerun).not.toHaveBeenCalled();act(() => tree.unmount());
});

it("offers saved describe details in the actual failed cell", () => {
  const id="12345678-1234-1234-1234-123456789abc";
  const workspace: Workspace = {...emptyWorkspace,nodes:[node({id:"id1",command:":describe file:synthetic.json provider:demo",state:"failed",failure:"Describe rejected",failureRecord:{id:"e",code:"DSC002",message:"Describe rejected",causeId:"",issues:[{path:"/describeReport",code:"DSC_REPORT",message:id}]}})]};
  const {tree}=blocksOf(session(workspace,client(":describe file:synthetic.json provider:demo",["id1"])),workspace);
  expect(tree.root.findByType(DescribeFailureDetails).props.id).toBe(id);
  act(()=>tree.unmount());
});

function blocksOf(cell: SessionCell, workspace: Workspace, held = new Map<string, StoredValue>(), reads = new Map()) {
  const blocks = cellBlocks({ cell, workspace, held, reads, retryRead: () => {}, engine, generation: "g1" });
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<>{blocks.map((block) => <div key={block.key}>{block.content}</div>)}</>); });
  const text = tree.root.findAllByType(MonoLine).map((line) => lineText(line.props.segments));
  return { blocks, tree, text };
}

it("uses the same sub-millisecond bound in cell and node run summaries", () => {
  const at="2026-09-26T09:00:00.000Z";
  const workspace: Workspace={...emptyWorkspace,nodes:[node({id:"id1",run:"r1",type:"Int",handle:"h1"}),node({id:"id2",run:"r2",type:"Int",handle:"h2"})],history:[log("id1","r1","RUNNING",at),log("id1","r1","READY",at),log("id2","r2","RUNNING",at),log("id2","r2","READY",at)]};
  const value: StoredValue={type:INT,data:1,provenance:{}};
  const held=new Map([["h1",value],["h2",value]]);
  const cell=session(workspace,client(":calc { return 1; }\n:calc { return 1; }",["id1","id2"]),held);
  expect(lineText(joined(cell.verdict))).toContain("<1 ms");
  const {blocks,tree}=blocksOf(cell,workspace,held);
  expect(blocks.filter(block=>block.duration === "<1 ms")).toHaveLength(2);
  act(()=>tree.unmount());
});

describe("a four-stage pipeline", () => {
  const source = 'http request url:"http://127.0.0.1:9/missing"\n| :calc { return input.status; } > status\n:calc { return div(1,0); }\n| :calc { return input+1; }';
  const workspace: Workspace = {
    ...emptyWorkspace,
    nodes: [
      node({ id: "id1", run: "r1", command: 'http request url:"http://127.0.0.1:9/missing"', type: "HttpResponse" }),
      node({ id: "id2", run: "r2", name: "status", command: "| :calc { return input.status; } > status", type: "Int" }),
      node({ id: "id3", run: "r3", command: ":calc { return div(1,0); }", state: "failed", failure: "CAL005: div divisor must not be zero", failureRecord: locatedError }),
      node({ id: "id4", command: "| :calc { return input+1; }", state: "skipped" }),
    ],
    history: [log("id1", "r1", "RUNNING", "2026-09-26T09:00:00.000Z"), log("id1", "r1", "READY", "2026-09-26T09:00:00.100Z"),
      log("id2", "r2", "RUNNING", "2026-09-26T09:00:00.100Z"), log("id2", "r2", "READY", "2026-09-26T09:00:00.101Z"),
      log("id3", "r3", "RUNNING", "2026-09-26T09:00:00.001Z"), log("id3", "r3", "FAILED", "2026-09-26T09:00:00.002Z")],
  };
  const cell = session(workspace, client(source, ["id1", "id2", "id3", "id4"]));

  it("should_DrawAGlyphPerStageAndCountThemInTheVerdict_When_StatesDiffer", () => {
    // Assert
    expect(cell.rows.map((row) => row.nodes.map((it) => `${it.glyph} ${it.label}`).join(" "))).toEqual(["ready id1", "ready $status", "failed id3", "skipped id4"]);
    expect(lineText(joined(cell.verdict))).toBe("failed · 4 results · 2 ok · 1 failed · 1 skipped · 101 ms · kept");
    expect(cell.state).toBe("failed");
  });

  it("should_ShowTheFailedStagesCodeMessageAndSpan_When_ItsBlockIsDrawn", () => {
    // Act
    const { text } = blocksOf(cell, workspace);
    // Assert
    expect(text).toContain("CAL005 · div divisor must not be zero");
    expect(text.join("\n")).toContain(locationSummary);
    expect(text).toContain("skipped · an earlier stage failed");
  });

  it("should_TakeDurationsFromLogRecordsAndLeaveOutWhatIsMissing_When_TheyArePartial", () => {
    // Act
    const spans = durationsOf(workspace);
    // Assert
    expect(spans.get("id1")).toEqual({ start: Date.parse("2026-09-26T09:00:00.000Z"), end: Date.parse("2026-09-26T09:00:00.100Z") });
    expect(spans.has("id4")).toBe(false);
    expect(cell.nodes.find((it) => it.id === "id3")?.durationMs).toBe(1);
    // id4 never ran, so the cell time is the span of the three that did.
    const whole = session({ ...workspace, nodes: workspace.nodes.map((it) => (it.id === "id3" ? { ...it, state: "ready" as const, failure: undefined } : it)) },
      client(source, ["id1", "id2", "id3"]));
    expect(lineText(joined(whole.verdict))).toContain("101 ms");
    const missing = session({ ...workspace, history: workspace.history.slice(0, 2) }, client(source, ["id1", "id2"]));
    expect(lineText(joined(missing.verdict))).not.toMatch(/\d ms/);
  });

  it("should_ReadCodeMessageAndSpan_When_AFailureReasonIsParsed", () => {
    expect(failureOf("CAL005: div divisor must not be zero", locatedError)).toEqual({ code: "CAL005", message: "div divisor must not be zero", span: locationSummary });
    expect(failureOf("not found")).toEqual({ message: "not found" });
  });
});

describe("run state precedes value shape", () => {
  it("keeps a long known type in its disclosure across stream computations", () => {
    const type="{ container: Text, timestamp: Instant, cpu: Option<Float>, memory: Option<Float>, sequence: Int }";
    let tree!:ReactTestRenderer;
    for(const state of ["ready", "running", "ready"] as const){
      const workspace={...emptyWorkspace,nodes:[node({id:"s",name:"stats",state,type,streamOutput:true})]};
      const model=session(workspace,client(":calc pure { return $feed; } > stats",["s"]));
      expect(lineText(joined(model.verdict.filter(field=>field.zone!=="data")))).not.toContain(type);
      const blocks=cellBlocks({cell:model,workspace,held:new Map(),reads:new Map(),retryRead:()=>{},engine,generation:undefined});
      const props={theme:"keys" as const,state:model.state,label:model.id,rows:model.rows,verdict:model.verdict,blocks};
      act(()=>{if(tree)tree.update(<Cell {...props}/>);else tree=create(<Cell {...props}/>);});
      const run=tree.root.findByProps({"aria-label":"Result status and type"});
      expect(run.findAllByType(MonoLine).map(line=>lineText(line.props.segments)).join("")).not.toContain(type);
      const trigger=tree.root.findByProps({"aria-label":"Type of $stats"});
      expect(trigger.props["aria-haspopup"]).toBe("dialog");
      expect(trigger.children.join("").length).toBeLessThanOrEqual(40);
    }
    act(()=>tree.unmount());
    const waiting={...emptyWorkspace,nodes:[node({id:"s",state:"pending",type,waiting:[{source:"feed",port:"data" as const,state:"pending" as const,run:null,message:"waiting for $feed"}]})]};
    const model=session(waiting,client("synthetic",["s"]));
    expect(lineText(joined(model.verdict.filter(field=>field.zone!=="data")))).toContain("waiting for $feed");
    expect(model.nodes[0]?.type).toBe(type);
  });

  it("assigns execution slots without guessing a missing duration or retention", () => {
    const model=session({...emptyWorkspace,nodes:[node({id:"s",type:"Int"})]},client("synthetic",["s"]));
    expect(model.verdict.filter(field=>field.slot).map(field=>[field.slot,lineText(field.segments)]))
      .toEqual([["state","ok"],["retention","kept"]]);
    const unknown=session({...emptyWorkspace,nodes:[]},client("synthetic",[],{state:"unanswered"}));
    expect(unknown.verdict.filter(field=>field.slot).map(field=>[field.slot,lineText(field.segments)]))
      .toEqual([["state","outcome unknown"]]);
  });

  it("should_DrawStateOnlyAndNoRows_When_TheNodeIsRunning", () => {
    const workspace = { ...emptyWorkspace, nodes: [node({ id: "s", name: "stats", state: "running", startedAt: "2026-09-26T09:59:00Z" })] };
    const { text, tree } = blocksOf(session(workspace, client("docker stats container:$c > stats", ["s"], { state: "running" })), workspace);
    expect(text.join("")).toContain("● running");
    expect(tree.root.findAll((it) => it.props.className === "value-table")).toHaveLength(0);
    expect(engine.fetch).not.toHaveBeenCalled();
  });

  it("should_SayWhatItWaitsFor_When_TheEngineNamesTheWaitingInput", () => {
    const waiting = [{ source: "id9", port: "data" as const, state: "pending" as const, run: null, message: "waiting for $orders" }];
    const workspace = { ...emptyWorkspace, nodes: [node({ id: "w", state: "pending", waiting })] };
    const cell = session(workspace, client("synthetic | :calc { return input; }", ["w"]));
    const { text } = blocksOf(cell, workspace);
    expect(text).toContain("waiting for $orders");
    expect(text.join("\n")).not.toContain("id9");
    const unnamed = { ...emptyWorkspace, nodes: [node({ id: "w", state: "pending" })] };
    expect(blocksOf(session(unnamed, client("synthetic", ["w"])), unnamed).text).toContain("waiting for its inputs");
  });

  it("should_SayReadingOrOfferARetry_When_TheResultIsNotReadYet", () => {
    const workspace = { ...emptyWorkspace, nodes: [node({ id: "r", name: "receipt", handle: "h1" })] };
    const cell = session(workspace, client(":calc 1 > receipt", ["r"]));
    expect(blocksOf(cell, workspace).tree.root.findByType(ReadStatus).props.problem).toBeUndefined();
    const failed = blocksOf(cell, workspace, new Map(), new Map([["h1", { problem: "read timed out" }]]));
    expect(failed.text.join("")).toContain("could not read result · read timed out");
    expect(cell.state).toBe("default");
  });

  it("should_KeepTheConversation_When_AProcessIsAsking", () => {
    const workspace = { ...emptyWorkspace, nodes: [node({ id: "p", name: "probe", state: "running", interactive: true, conversationActive: true, run: "run1", wrote: "Password:" })] };
    const { tree } = blocksOf(session(workspace, client('sh run cmd:"su qa" > probe', ["p"], { state: "running" })), workspace);
    expect(tree.root.findAllByType(InteractiveResponse)).toHaveLength(1);
    expect(engine.answer).not.toHaveBeenCalled();
  });

  it("should_NeverEvaluateARecipe_When_ItsBlockIsDrawnOrExpanded", () => {
    const recipe: StoredValue = { type: { kind: "iter", element: INT }, data: { mode: "recipe" }, provenance: {} };
    const workspace = { ...emptyWorkspace, nodes: [node({ id: "i", name: "calls", handle: "h" })] };
    const cell = session(workspace, client(":calc { return range(3); } > calls", ["i"]), new Map([["h", recipe]]));
    for (const view of ["preview", "expanded"] as const) {
      const { text } = blocksOf({ ...cell, view }, workspace, new Map([["h", recipe]]));
      expect(text.join("")).toContain("nothing run yet");
    }
    expect(cell.rows[0]!.nodes[0]!.glyph).toBe("recipe");
    expect(engine.fetch).not.toHaveBeenCalled();
    expect(engine.rerun).not.toHaveBeenCalled();
  });

  it("should_ShowTheLastWindowAndSayStopped_When_AStreamWasStopped", () => {
    const sample: TypeShape = { kind: "record", name: "Sample", fields: [{ name: "sequence", type: INT }] };
    const value: StoredValue = { type: { kind: "list", element: sample }, data: Array.from({ length: 213 }, (_, at) => ({ sequence: at + 1 })), provenance: {} };
    const workspace = { ...emptyWorkspace, nodes: [node({ id: "s", name: "stats", state: "cancelled", evidence: { kind: "stopped_stream", source: "s", run: "r" }, handle: "h", type: "List<Sample>" })] };
    const held = new Map([["h", value]]);
    const cell = session(workspace, client("docker stats container:$c > stats", ["s"]), held);
    expect(lineText(joined(cell.verdict))).toBe("stopped · stream stopped · last value · 213 · kept");
    const { tree } = blocksOf(cell, workspace, held);
    const lastRow = tree.root.findAllByProps({ role: "row" }).filter((it) => it.type === "tr").at(-1)!;
    expect(lastRow.findAllByProps({ role: "cell" }).filter((it) => it.type === "td").map((it) => it.children.filter(child=>typeof child==="string").join(""))).toEqual(["213"]);
    expect(cell.rows[0]!.nodes[0]!.glyph).toBe("stopped");
    expect(runAvailability(cell.state, cell.verdict, cell.rows.flatMap(row => row.nodes)).repeatVerb).toBe("restart…");
  });
});

/*
 * Finite record analysis. Node and counter inputs follow the declared Rust contract
 * (driver/progress.rs, web/projection.rs evidence frames); the stored value is a minimal structural
 * stand-in for a ScanResult, not a captured engine receipt.
 */
describe("finite record analysis", () => {
  const counters = { committedPosition: "10", readPosition: "40", extent: "100", unit: "bytes" as const, inputRecords: "3", outputRecords: "2",
    work: "900", workAllowance: "1000", workLimit: "5000", heldCharge: "64", highWaterCharge: "96", heldLimit: "4096", outputCharge: "32", outputLimit: "1024" };
  const progress = (phase: "processing" | "stopped" | "complete", withheld = false) =>
    ({ run: "r1", value: { kind: "records" as const, phase, counters: withheld ? null : counters } });
  const partial: StoredValue = { type: { kind: "record", name: "ScanResult", fields: [{ name: "outputs", type: { kind: "list", element: INT } }] }, data: { outputs: [1, 2] }, provenance: {} };
  const error = { id: "e", code: "CAL006", message: "scan work limit reached (5000)", causeId: "", issues: [] };
  const incomplete = node({ id: "a", name: "normalized", command: ":scan source:$raw > normalized", run: "r1", state: "failed", kept: false,
    failure: error.message, failureRecord: error, evidence: { kind: "incomplete", source: "a", run: "r1" }, handle: "h", type: "ScanResult", progress: progress("stopped") });

  it("should_KeepTheCellFailedAndShowThePartialResultBesideIt_When_AnAnalysisStopped", () => {
    const workspace = { ...emptyWorkspace, nodes: [incomplete] };
    const held = new Map([["h", partial]]);
    const cell = session(workspace, client(":scan source:$raw > normalized", ["a"]), held);
    const verdict = lineText(joined(cell.verdict));
    expect(cell.state).toBe("failed");
    expect(verdict).toMatch(/^failed · /);
    expect(verdict).toContain("partial result · incomplete");
    // No success word of its own; the required "incomplete" is not a standalone "complete".
    expect(verdict).not.toMatch(/\bok\b|stream stopped|\bcompleted?\b/);
    expect(cell.rows[0]!.nodes[0]!.glyph).toBe("failed");
    expect(runAvailability(cell.state, cell.verdict, cell.rows.flatMap(row => row.nodes)).repeatVerb).not.toBe("restart…");
    const { blocks, tree, text } = blocksOf(cell, workspace, held);
    expect(blocks.map(block => [block.key, block.zone])).toEqual([["a:progress", "run"], ["a:failure", "run"], ["a", undefined]]);
    expect(blocks.at(-1)!.hasValue).toBe(true);
    const said = text.join("\n");
    expect(said).toContain("incomplete · committed partial result, not a completed run");
    expect(said).toContain("stopped · committed through 10 of 100 bytes · read through 40 · read, not committed");
    expect(said).toContain("3 records in · 2 outputs · work 900 used of 1,000 earned · cap 5,000");
    expect(said).toContain("logical charge · held 64 · high-water 96 of cap 4,096 · output 32 of cap 1,024");
    expect(said).not.toMatch(/no value · the run failed|stream stopped|restart|resume|dataset|KiB|MiB|RSS/i);
    expect(tree.root.findAllByType(ValueBlock)).toHaveLength(1);
    act(() => tree.unmount());
  });

  it("should_DrawTheSameThreeRows_When_CountersAreWithheldForANonPublicRun", () => {
    const running = node({ ...incomplete, state: "running", evidence: undefined, failure: undefined, failureRecord: undefined, handle: undefined, private: true, progress: progress("processing", true) });
    const workspace = { ...emptyWorkspace, nodes: [running] };
    const cell = session(workspace, client(":scan source:$raw > normalized", ["a"]));
    const { blocks, tree } = blocksOf(cell, workspace);
    const rows = tree.root.findAllByProps({ className: "mono-line record-progress-row" });
    expect(rows).toHaveLength(3);
    const said = tree.root.findAllByType(MonoLine).map(line => lineText(line.props.segments)).join("\n");
    expect(said).toContain("processing · counters withheld · the input is not public");
    expect(said).not.toMatch(/\d{2,}/);
    expect(blocks.some(block => block.key === "a:progress")).toBe(true);
    // Running work is cancellable through the cell's existing cancel action; nothing else is offered.
    expect(runAvailability(cell.state, cell.verdict, cell.rows.flatMap(row => row.nodes))).toMatchObject({ running: true, working: true });
    act(() => tree.unmount());
  });

  it("should_CallALaggingReportOld_When_TheEnginesStateIsAlreadyTerminal", () => {
    for (const [state, phase] of [["cancelled", "processing"], ["failed", "complete"]] as const) {
      const workspace = { ...emptyWorkspace, nodes: [node({ ...incomplete, state, evidence: undefined, handle: undefined, progress: progress(phase) })] };
      const { text, tree } = blocksOf(session(workspace, client(":scan source:$raw > normalized", ["a"])), workspace);
      expect(text.join("\n")).toContain(`last reported ${phase} · committed through 10 of 100 bytes`);
      act(() => tree.unmount());
    }
  });

  it("should_IgnoreProgressOfAnotherRun_When_TheNodeHasMovedOn", () => {
    const workspace = { ...emptyWorkspace, nodes: [node({ ...incomplete, run: "r2", state: "running", evidence: undefined, handle: undefined })] };
    const { blocks, tree } = blocksOf(session(workspace, client(":scan source:$raw > normalized", ["a"])), workspace);
    expect(blocks.some(block => block.key === "a:progress")).toBe(false);
    act(() => tree.unmount());
  });
});

describe("process output", () => {
  it("should_LiftExitOneIntoTheVerdictAndKeepTheCellDefault_When_TheProcessFailed", () => {
    const BYTES: TypeShape = { kind: "primitive", name: "BYTES" };
    const value: StoredValue = {
      type: { kind: "record", name: "ProcessOutput", fields: [{ name: "exitCode", type: INT }, { name: "stdout", type: BYTES }, { name: "stderr", type: BYTES }] },
      data: { exitCode: 1, stdout: "", stderr: btoa("cat: /nowhere: No such file\n") }, provenance: {},
    };
    const workspace = { ...emptyWorkspace, nodes: [node({ id: "p", name: "missing", handle: "h", type: "ProcessOutput" })] };
    const held = new Map([["h", value]]);
    const cell = session(workspace, client('cat run args:"/nowhere" > missing', ["p"]), held);
    const exit = cell.verdict.flatMap((it) => it.segments).find((it) => it.text === "exit 1");
    expect(exit?.role).toBe("mono-warn");
    expect(lineText(joined(cell.verdict))).toBe("ok · ProcessOutput · exit 1 · kept");
    expect(cell.state).toBe("default");
    const { tree } = blocksOf(cell, workspace, held);
    expect(tree.root.findByProps({className:"process-facts"}).findAllByType("span").map(span=>span.children.join("")).join("")).toContain("stdout empty");
    expect(JSON.stringify(tree.toJSON())).toContain("cat: /nowhere: No such file");
  });
});

describe("the spacing rule", () => {
  it("should_OwnOneGutterPerLevel_When_TheStylesheetsAreRead", () => {
    const cell = readFileSync(new URL("./cell.css", import.meta.url), "utf8");
    const session = readFileSync(new URL("./session.css", import.meta.url), "utf8");
    const split = readFileSync(new URL("./split.css", import.meta.url), "utf8");
    expect(cell).toMatch(/\.cell-band \{[^}]*grid-template-columns:minmax\(0,1fr\);/);
    expect(cell).toMatch(/\.cell-actions>[\s\S]*border-left:1px solid var\(--rule\)/);
    expect(cell).toMatch(/\.cell-action \{[^}]*padding:1px 4px/);
    expect(session).toMatch(/\.session-in-pane \{[^}]*--page-gutter: 0px;/);
    expect(split).toMatch(/\.split-pane \{[^}]*padding: var\(--space-sm\) 0;/);
  });
});

describe("rejected attempts keep previous results distinct", () => {
  const reason = "Stream pipelines require an explicit source restart";
  it("does not claim four successful nodes ran in a rejected repeat", () => {
    const workspace = { ...emptyWorkspace, attemptFailures: { retry: reason },
      nodes: [1, 2, 3, 4].map(id => node({ id: `id${id}` })),
      history: [log("id1", "old", "RUNNING", "2026-09-26T09:00:00.000Z"),
        log("id1", "old", "READY", "2026-09-26T09:00:00.060Z")] };
    const cell = session(workspace, client("synthetic stream", ["id1", "id2", "id3", "id4"], { lastRun: "retry" }));
    expect(lineText(joined(cell.verdict))).toBe(`not run · ${reason} · previous results shown`);
    expect(cell.nodes).toHaveLength(4);
    const { tree } = blocksOf(cell, workspace);
    expect(JSON.stringify(tree.toJSON())).toContain("Previous results · the latest attempt was not run.");
    act(() => tree.unmount());
  });
  it("does not invent previous results for a refused first submission", () => {
    const cell = session({ ...emptyWorkspace, attemptFailures: { c1: reason } }, client("synthetic stream", []));
    expect(lineText(joined(cell.verdict))).toBe(`not run · ${reason}`);
    expect(cell.previousAttemptResults).toBe(false);
  });
  it("keeps accepted partial execution with error diagnostics an execution outcome", () => {
    const cell = session({ ...emptyWorkspace, nodes: [node({ id: "id1" })] }, client("synthetic partial", ["id1"], {
      diagnostics: [{ code: "SYNTHETIC", severity: "error", message: "Another statement was invalid", start: 0, end: 0, hints: [] }] }));
    expect(lineText(joined(cell.verdict))).not.toContain("not run");
    expect(cell.previousAttemptResults).toBe(false);
  });
});

it("counts successful sibling results once while retaining a mixed-state breakdown",()=>{
  const ids=["a","b","c"];
  const workspace={...emptyWorkspace,nodes:ids.map(id=>node({id}))};
  expect(lineText(joined(session(workspace,client("synthetic",ids)).verdict))).toBe("ok · 3 results · kept");
  const mixed={...workspace,nodes:workspace.nodes.map(n=>n.id==="b"?{...n,state:"failed" as const}:n)};
  expect(lineText(joined(session(mixed,client("synthetic",ids)).verdict))).toContain("3 results · 2 ok · 1 failed");
});

it("keeps an anonymous parsed HTTP response status ahead of a generic field count",()=>{
  const fixture=httpValue();
  if(fixture.type.kind!=="record")throw new Error("synthetic HTTP record required");
  const value:StoredValue={...fixture,type:{...fixture.type,name:"",fields:fixture.type.fields.map(f=>f.name==="body"?{name:"body",type:{kind:"record",name:"",fields:[]}}:f)},data:{...(fixture.data as object),status:503,body:{message:"synthetic unavailable"}}};
  const workspace={...emptyWorkspace,nodes:[node({id:"http",handle:"h",type:"synthetic anonymous HTTP"})]};
  const held=new Map([["h",value]]);
  const {blocks,tree}=blocksOf(session(workspace,client("synthetic",["http"]),held),workspace,held);
  expect(lineText(blocks[0]!.header??[])).toBe("HTTP 503");
  act(()=>tree.unmount());
});
