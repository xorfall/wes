import { afterEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "../engine";
import type { WorkspaceNode } from "../workspace";
import { GraphPreview, GRAPH_PREVIEW_BYTES, previewText } from "./graph-preview";
import { ExactNumber } from "../exact-json";
const limits = vi.hoisted(() => ({ codec: undefined as number | undefined }));
vi.mock("../limits/policy", async original => {
  const policy = await original<typeof import("../limits/policy")>();
  return { ...policy, budget: (id: string) => id === "codec.bytes" && limits.codec !== undefined ? limits.codec : policy.budget(id) };
});
const node = (over: Partial<WorkspaceNode> = {}): WorkspaceNode => ({ id: "a", command: "fixture", state: "ready", dependsOn: [], provenance: {}, cautions: [], kept: false, handle: "h", bytes: 100, ...over });
let tree: ReactTestRenderer | undefined;
afterEach(() => { act(() => tree?.unmount()); limits.codec = undefined; });
it("reads only the selected handle with a server-enforced cap and drops superseded completions", async () => {
  const finish: ((value: unknown) => void)[] = [];
  const fetch = vi.fn(() => new Promise(resolve => finish.push(resolve)));
  const engine = { fetch } as unknown as Engine;
  await act(async () => { tree = create(<GraphPreview engine={engine} generation="g1" node={node()} />); });
  expect(fetch).toHaveBeenCalledWith("h", GRAPH_PREVIEW_BYTES);
  await act(async () => { tree!.update(<GraphPreview engine={engine} generation="g2" node={node({ handle: "next" })} />); });
  await act(async () => finish[0]!({ data: "OLD PRIVATE CANARY", provenance: {} }));
  expect(JSON.stringify(tree!.toJSON())).not.toContain("OLD PRIVATE CANARY");
  await act(async () => finish[1]!({ data: "current", provenance: {} }));
  expect(JSON.stringify(tree!.toJSON())).toContain("current");
});
it.each<Partial<WorkspaceNode>>([{ bytes: GRAPH_PREVIEW_BYTES + 1 }, { accessWithdrawn: true }, { streamOutput: true }, { doubt: { capability: "fixture", safe: false, when: "now" } }])("does not read ineligible previews: %j", async over => {
  const fetch = vi.fn();
  await act(async () => { tree = create(<GraphPreview engine={{ fetch } as unknown as Engine} generation="g" node={node(over)} />); });
  expect(fetch).not.toHaveBeenCalled();
});
it("requires an explicit bounded read when the size is unknown", async () => {
  const fetch = vi.fn(async () => ({ data: "synthetic", provenance: {} }));
  await act(async () => { tree = create(<GraphPreview engine={{ fetch } as unknown as Engine} generation="g" node={node({ bytes: undefined })} />); });
  expect(fetch).not.toHaveBeenCalled();
  await act(async () => tree!.root.findByType("button").props.onClick());
  expect(fetch).toHaveBeenCalledWith("h", GRAPH_PREVIEW_BYTES);
});
it("respects a configured display budget below the preview ceiling", async () => {
  limits.codec = 4096;
  const fetch = vi.fn(async () => ({ type: { kind: "unknown" }, data: "synthetic", provenance: {} }));
  await act(async () => { tree = create(<GraphPreview engine={{ fetch } as unknown as Engine} generation="g" node={node()} />); });
  expect(fetch).toHaveBeenCalledExactlyOnceWith("h", 4096);
});
it("bounds display entries and characters without making a partial excerpt into a value", () => {
  const value = { type: { kind: "unknown" as const }, provenance: {}, data: Array.from({ length: 20 }, (_, i) => ({ i, log: "x".repeat(5000) })) };
  const result = previewText(value);
  expect(result.text.length).toBeLessThanOrEqual(4096);
  expect(result.note).toBe("Showing 6 of 20 items");
  expect(value.data).toHaveLength(20);
});
it("shows exact numeric scalars as numbers rather than their implementation fields", () => {
  expect(previewText({ type: { kind: "unknown" }, provenance: {}, data: new ExactNumber("9007199254740993") })).toEqual({ text: "9007199254740993", note: undefined });
});
