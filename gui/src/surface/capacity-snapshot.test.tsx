import { expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { act, create } from "react-test-renderer";
import { CapacityStatus } from "./CapacityStatus";
import { SnapshotNotice } from "./SnapshotNotice";
import { emptyWorkspace, apply } from "../workspace";
import type { StoredValue } from "../protocol";

it("shows global slot usage and the different lifetimes without presenting saturation as failure", () => {
  const html = renderToStaticMarkup(<CapacityStatus capacity={{ operations: { used: 2, limit: 4 }, streams: { used: 7, limit: 7 } }} />);
  expect(html).toContain("operations 2/4");
  expect(html).toContain("streams 7/7");
  expect(html).toContain("Across all workspaces");
  expect(html).toContain("until cleanup finishes");
  expect(html).toContain("Interactive conversations keep their operation slot");
  expect(html).toContain('class="mono-warn">streams');
  expect(html).not.toContain("mono-bad");
  expect(html).toContain("--max-streams 7");
  expect(renderToStaticMarkup(<CapacityStatus />)).toBe("");
});

it("keeps capacity independent of node counts and updates the existing workspace state", () => {
  const workspace = apply(emptyWorkspace, { event: "execution-capacity", operations: { used: 1, limit: 4 }, streams: { used: 2, limit: 9 } });
  expect(workspace.nodes).toEqual([]);
  expect(workspace.capacity?.streams).toEqual({ used: 2, limit: 9 });
});

const value: StoredValue = { type: { kind: "unknown" }, data: [], provenance: { "snapshot.kind": "names", "snapshot.capturedAt": "2026-01-02T03:04:05Z" } };
it("displays the value's captured time without replacing unknown data or claiming live state", () => {
  const html = renderToStaticMarkup(<SnapshotNotice value={value} />);
  expect(html).toContain('dateTime="2026-01-02T03:04:05Z"');
  expect(html).toContain("Snapshot");
  expect(html).toContain("Pending/running results may not have a determined type yet");
  expect(value.data).toEqual([]);
  expect(renderToStaticMarkup(<SnapshotNotice value={{ ...value, provenance: {} }} />)).toBe("");
});

it("refreshes only on an explicit action and disables duplicate refresh while running", () => {
  const refresh = vi.fn();
  let tree!: ReturnType<typeof create>;
  act(() => { tree = create(<SnapshotNotice value={value} onRefresh={refresh} />); });
  expect(refresh).not.toHaveBeenCalled();
  act(() => tree.root.findByType("button").props.onClick());
  expect(refresh).toHaveBeenCalledTimes(1);
  act(() => tree.update(<SnapshotNotice value={value} onRefresh={refresh} refreshing />));
  expect(tree.root.findByType("button").props.disabled).toBe(true);
  expect(tree.root.findByType("time").props.dateTime).toBe(value.provenance["snapshot.capturedAt"]);
  act(() => tree.unmount());
});
