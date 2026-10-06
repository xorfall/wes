import { expect, it } from "vitest";
import { apply, emptyWorkspace, type Workspace } from "./workspace";
import { NODE_TRANSCRIPT, WORKSPACE_TRANSCRIPTS } from "./transcripts";

function conversation(state: Workspace, node: string, run: string): Workspace {
  state = apply(state, { event: "created", dependencyLifetime: "continuous", node, name: "", command: "dialog ask", dependsOn: [], interactive: true });
  state = apply(state, { event: "node", constructionComplete: false, node, state: "running" });
  return apply(state, { event: "conversation", node, run, active: true });
}
function output(state: Workspace, node: string, run: string, text: string, omittedBytes = "0"): Workspace {
  return apply(state, { event: "output", node, run, text, omittedBytes });
}
it("ignores delayed old-run output and clears the old dialogue only when its run changes", () => {
  let state = conversation(emptyWorkspace, "n", "old");
  state = output(state, "n", "old", "first prompt");
  state = apply(state, { event: "conversation", node: "n", run: "old", active: false });
  expect(state.nodes[0]?.wrote).toBe("first prompt");
  state = conversation(state, "n", "new");
  expect(state.nodes[0]?.wrote).toBeUndefined();
  state = output(state, "n", "old", "stale output");
  expect(state.nodes[0]?.wrote).toBeUndefined();
  state = output(state, "n", "new", "new prompt");
  expect(state.nodes[0]?.wrote).toBe("new prompt");
});
it("keeps terminal output for the same run and reports both source loss and client trimming", () => {
  let state = conversation(emptyWorkspace, "n", "r");
  state = output(state, "n", "r", "🎉" + "x".repeat(NODE_TRANSCRIPT - 1), "9007199254740993");
  expect(state.nodes[0]?.wrote).toBe("x".repeat(NODE_TRANSCRIPT - 1));
  expect(state.nodes[0]?.wroteTrimmed).toBe(true);
  expect(state.nodes[0]?.outputLost).toBe(true);
  state = apply(state, { event: "ready", node: "n", type: "Text", handle: "h", bytes: 1, kept: false, provenance: {}, cautions: [] });
  state = apply(state, { event: "conversation", node: "n", run: "r", active: false });
  state = output(state, "n", "r", "tail");
  expect(state.nodes[0]?.wrote?.endsWith("tail")).toBe(true);
  expect(state.nodes[0]?.conversationActive).toBe(false);
});
it("bounds all transcripts together while preserving the current conversation and declaring omission", () => {
  let state = emptyWorkspace;
  for (let i = 0; i < 40; i++) {
    state = conversation(state, String(i), "r");
    state = output(state, String(i), "r", "x".repeat(NODE_TRANSCRIPT));
    expect(state.nodes.reduce((sum, node) => sum + (node.wrote?.length ?? 0), 0)).toBeLessThanOrEqual(WORKSPACE_TRANSCRIPTS);
  }
  expect(state.nodes[0]?.wrote).toBe("");
  expect(state.nodes[0]?.wroteTrimmed).toBe(true);
  expect(state.nodes[39]?.wrote).toHaveLength(NODE_TRANSCRIPT);
});
it("marks gaps even after a final state overtakes lost output and clears transient text on a workspace switch", () => {
  let state = conversation(emptyWorkspace, "n", "r");
  state = output(state, "n", "r", "prompt");
  state = apply(state, { event: "conversation", node: "n", run: "r", active: false });
  state = apply(state, { event: "output-gap" });
  expect(state.nodes[0]?.outputLost).toBe(true);
  expect(apply(state, { event: "session", workspace: null, generation: "other" })).toEqual(emptyWorkspace);
});
