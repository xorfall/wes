import { afterEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import type { Event } from "../protocol";
import { useWindowWorkspace } from "./window-workspace";

const connections = vi.hoisted(() => new Map<string | undefined, { emit(event: Event): void; stop: ReturnType<typeof vi.fn> }>());
vi.mock("../engine", () => ({ Engine: class {
  constructor(readonly binding?: string) {}
  listen(emit: (event: Event) => void) { const stop = vi.fn(); connections.set(this.binding, { emit, stop }); return stop; }
} }));
let tree: ReactTestRenderer | undefined;
let shown: ReturnType<typeof useWindowWorkspace>;
function Probe({ binding }: { binding: string }) { shown = useWindowWorkspace(binding); return <pre>{shown.workspace.identity?.name}:{shown.generation}:{shown.cells.length}</pre>; }
afterEach(() => { act(() => tree?.unmount()); connections.clear(); });
it("clears the old binding immediately and ignores events from a stopped subscription", () => {
  act(() => { tree = create(<Probe binding="alpha" />); });
  const old = connections.get("alpha")!;
  act(() => { old.emit({ event: "session", generation: "old", workspace: "alpha" }); old.emit({ event: "workspace-context", name: "alpha", saved: ["alpha", "beta"] }); });
  expect(shown.workspace.identity?.name).toBe("alpha");
  act(() => { tree!.update(<Probe binding="beta" />); });
  expect(old.stop).toHaveBeenCalledOnce();
  expect(shown.engine.binding).toBe("beta");
  expect(shown.generation).toBeUndefined();
  expect(shown.workspace.identity).toBeUndefined();
  act(() => { old.emit({ event: "workspace-context", name: "OLD PRIVATE CANARY", saved: [] }); old.emit({ event: "session", generation: "late", workspace: "alpha" }); });
  expect(JSON.stringify(tree!.toJSON())).not.toContain("CANARY");
  const current = connections.get("beta")!;
  act(() => { current.emit({ event: "session", generation: "new", workspace: "beta" }); current.emit({ event: "workspace-context", name: "beta", saved: ["beta"] }); });
  expect(shown.generation).toBe("new");
  act(() => current.emit({ event: "workspace-closed", workspace: "beta" }));
  expect(shown.generation).toBeUndefined();
  expect(shown.workspace.nodes).toHaveLength(0);
  expect(shown.cells).toHaveLength(0);
});
