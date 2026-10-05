import { readFileSync } from "node:fs";
import { expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { Engine } from "../engine";
import type { Event } from "../protocol";
import { newCell } from "../cells";
import { apply, emptyWorkspace } from "../workspace";
import { cellBlocks } from "./cell-output";
import { readSession } from "./session-model";
import { peekOf, peekText } from "./screens/Peek";

// The web acceptance test sends this exact long message through a real synthetic provider.
const reason = readFileSync(new URL("../../../crates/app/tests/fixtures/stream-error.txt", import.meta.url), "utf8");
const failed: Event = { event: "failed", node: "broken", reason, error: { id: "stream-failure", code: "SYN001", message: reason, causeId: "", issues: [] } };
const created = (node: string): Event => ({ event: "created", dependencyLifetime: "continuous", node, name: node, command: "synthetic", dependsOn: [], interactive: false });
const frames: Event[] = [created("source"), created("broken"), created("healthy"),
  { event: "node", constructionComplete: false, node: "source", state: "running" }, failed,
  { event: "node", constructionComplete: false, node: "healthy", state: "ready" }];

it("keeps the complete failed branch message through sibling updates, history eviction and reconnect", () => {
  let workspace = frames.reduce(apply, emptyWorkspace);
  for (let n = 0; n < 100; n++) {
    workspace = apply(workspace, { event: "node", constructionComplete: false, node: "healthy", state: n % 2 ? "ready" : "running" });
  }
  workspace = apply(workspace, { event: "log-delta", reset: true, removed: [], entries: [] });
  expect(workspace.history).toEqual([]);
  expect(workspace.nodes.find(node => node.id === "broken")!.failure).toBe(reason);

  for (const observed of [workspace, frames.reduce(apply, emptyWorkspace)]) {
    const held = new Map();
    const cell = readSession({ workspace: observed, held,
      cells: [{ ...newCell("events watch | :fork { ... }"), state: "answered", nodes: ["source", "broken", "healthy"] }],
      context: { workspace: "synthetic", connection: "connected" } }).cells[0]!;
    expect(cell.state).toBe("live"); // A living sibling does not hide the failed branch.
    const blocks = cellBlocks({ cell, workspace: observed, held, reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "synthetic" });
    const block = blocks.find(item => item.key === "broken:failure")!;
    expect(block.open).toBe(true);
    const markup = renderToStaticMarkup(<>{block.content}</>);
    expect(markup).toContain("SYN001");
    for (const detail of reason.trim().split("\n").slice(1)) expect(markup).toContain(detail);
    expect(markup).toContain("Synthetic stream branch failed; the healthy sibling may continue.");
    const material = peekOf(observed.nodes.find(node => node.id === "broken"), undefined);
    expect(peekText("error", undefined, material.source, material.failure)).toBe(reason);
  }
});
