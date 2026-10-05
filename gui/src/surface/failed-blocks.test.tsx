import { locatedError, locationSummary } from "./testing/located-error";
import { expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { Engine } from "../engine";
import { emptyWorkspace, type WorkspaceNode } from "../workspace";
import { newCell } from "../cells";
import { cellBlocks } from "./cell-output";
import { readSession } from "./session-model";

/* Synthetic failures only. */
const failed = (id: string, failure: string): WorkspaceNode => ({ id, command: ":calc {}", dependsOn: [], state: "failed", kept: false, provenance: {}, cautions: [], failure });
const blocksOf = (nodes: WorkspaceNode[]) => {
  const workspace = { ...emptyWorkspace, nodes };
  const held = new Map();
  const cell = readSession({ workspace, held, cells: [{ ...newCell(":calc {}"), state: "answered", nodes: nodes.map((n) => n.id) }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
  return cellBlocks({ cell, workspace, held, reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" });
};

it("should_DrawNoBlock_When_ALoneFailureHasNoCodeAndTheVerdictAlreadySaysIt", () => {
  // Arrange / Act
  const blocks = blocksOf([failed("n1", "Docker is available but has no connection. Connect a daemon first.")]);
  // Assert: no failure rows beyond the verdict and no empty result card
  expect(blocks).toHaveLength(0);
});

const markup = (blocks: ReturnType<typeof blocksOf>) => blocks.map((block) => renderToStaticMarkup(<>{block.content}</>)).join("\n");

it("should_DrawTheMessageAndTheSpan_When_TheVerdictLiftedTheCodeOfALoneFailure", () => {
  // Arrange / Act
  const coded = blocksOf([{ ...failed("n1", "CAL005: div divisor must not be zero"), failureRecord: locatedError }]);
  // Assert: the verdict says `CAL005`; the block says the rest, and the code only once
  expect(coded.filter(block=>block.zone==="run")).toHaveLength(1);
  expect(coded.filter(block=>block.zone!=="run")).toHaveLength(0);
  expect(markup(coded)).toContain("div divisor must not be zero");
  for (const line of locationSummary.split("\n")) expect(markup(coded)).toContain(line);
  expect(markup(coded)).not.toContain("CAL005");
  expect(markup(coded)).not.toContain("no value");
});

it("keeps provider message text literal instead of treating source-like wording as metadata", () => {
  const uncoded = blocksOf([failed("n1", "expected Bool (source bytes 40..70)")]);
  expect(uncoded).toHaveLength(0); // the unchanged message is already in the verdict
});

it("should_KeepEveryFailedBlock_When_TheCellMadeSeveralNodes", () => {
  // Arrange / Act
  const blocks = blocksOf([failed("n1", "first failed"), failed("n2", "second failed")]);
  // Assert: no verdict speaks for one node, so each block owns its whole failure and names its stage
  const run = blocks.filter(block=>block.zone==="run");
  expect(run.map(block=>block.key)).toEqual(["n1:failure","n2:failure"]);
  expect(markup([run[0]!])).toContain("n1");
  expect(markup([run[0]!])).toContain("first failed");
  expect(markup([run[1]!])).toContain("n2");
  expect(markup([run[1]!])).toContain("second failed");
  expect(blocks.filter(block=>block.zone!=="run").every(block=>block.hasValue===false)).toBe(true);
});

it("treats a lone failure beside a notice as one node: lifted headline, no stage duration, no empty card", () => {
  const node = { ...failed("n1", "CAL005: div divisor must not be zero"), failureRecord: locatedError };
  const workspace = { ...emptyWorkspace, nodes: [node] };
  const client = { ...newCell(":calc {}"), state: "answered" as const, nodes: [node.id], diagnostics: [{ code: "SYN010", message: "synthetic notice", severity: "warning" as const, start: 0, end: 0, hints: [] }] };
  const cell = readSession({ workspace, cells: [client], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
  const blocks = cellBlocks({ cell: { ...cell, nodes: cell.nodes.map(view => ({ ...view, durationMs: 94 })) }, workspace, held: new Map(), reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" });
  expect(blocks.map(block => block.key)).toEqual(["notices", "n1:failure"]);
  expect(markup(blocks)).not.toContain("CAL005");
  expect(markup(blocks)).toContain("synthetic notice");
});

it("keeps per-stage durations only when the cell made several nodes", () => {
  const ready = (id: string): WorkspaceNode => ({ id, command: ":calc 1", dependsOn: [], state: "ready", kept: false, provenance: {}, cautions: [] });
  const durationsOf = (nodes: WorkspaceNode[]) => {
    const workspace = { ...emptyWorkspace, nodes };
    const cell = readSession({ workspace, cells: [{ ...newCell(":calc 1"), state: "answered", nodes: nodes.map(n => n.id) }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
    return cellBlocks({ cell: { ...cell, nodes: cell.nodes.map(view => ({ ...view, durationMs: 94 })) }, workspace, held: new Map(), reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" })
      .filter(block => block.zone !== "run").map(block => block.duration);
  };
  expect(durationsOf([ready("a")])).toEqual([undefined]);
  expect(durationsOf([ready("a"), ready("b")]).every(Boolean)).toBe(true);
});

it("keeps the failed stage card of a rejected attempt's previous results", () => {
  const node = failed("n1", "previous failure");
  const workspace = { ...emptyWorkspace, nodes: [node], attemptFailures: { retry: "SYN003: synthetic rejection" } };
  const cell = readSession({ workspace, cells: [{ ...newCell(":calc {}"), lastRun: "retry", state: "answered", nodes: [node.id] }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
  const blocks = cellBlocks({ cell, workspace, held: new Map(), reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" });
  expect(blocks.map(block => block.key)).toEqual(["previous-attempt", "n1:failure", "n1"]);
  expect(markup(blocks)).toContain("previous failure");
});


it("uses the structured code in the verdict when the reason contains only the message", () => {
  const node = { ...failed("n1", locatedError.message), failureRecord: locatedError };
  const workspace = { ...emptyWorkspace, nodes: [node] };
  const cell = readSession({ workspace, cells: [{ ...newCell(":calc {}"), state: "answered", nodes: [node.id] }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
  expect(JSON.stringify(cell.verdict)).toContain("CAL005");
  expect(JSON.stringify(cell.verdict)).not.toContain(locatedError.message);
  expect(markup(blocksOf([node])).split(locatedError.message)).toHaveLength(2);
});


it("labels the current cell without UUIDs and preserves distinct call locations", () => {
  const id = "11111111-2222-4333-8444-555555555555";
  const record = { ...locatedError, locations: locatedError.locations!.map(location => ({ ...location, source: `cell ${id}` })) };
  const node = { ...failed("n1", record.message), failureRecord: record };
  const workspace = { ...emptyWorkspace, nodes: [node] };
  const cell = readSession({ workspace, cells: [{ ...newCell(":calc {}"), id, state: "answered", nodes: [node.id] }], context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
  const blocks = cellBlocks({ cell, workspace, held: new Map(), reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" });
  const html = markup(blocks);
  expect(html).not.toContain(id);
  expect(html).toContain("this cell · line 3, column 10");
  expect(html).toContain("called from line 2, column 4");
  expect(html).not.toContain("⌘click the source to open it");
});
