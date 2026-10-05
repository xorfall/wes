import { expect, it } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { emptyWorkspace, type Workspace } from "../workspace";
import { conversationKey, createInteractiveState, useInteractiveStates, type InteractiveStates } from "./interactive-state";

it("isolates sessions and clears removed conversations and replaced generations", () => {
  const workspace: Workspace = { ...emptyWorkspace, nodes: [{ id: "n", command: "fixture", dependsOn: [], state: "running", provenance: {}, cautions: [], kept: false, interactive: true, conversationActive: true, run: "a" }] };
  const maps: InteractiveStates[] = [];
  function Read({ workspace, generation }: { workspace: Workspace; generation: string }) {
    maps[0] = useInteractiveStates(workspace, generation);
    maps[1] = useInteractiveStates(workspace, generation);
    return null;
  }
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<Read workspace={workspace} generation="g1" />); });
  maps[0]!.set(conversationKey("n", "a"), createInteractiveState());
  expect(maps[1]!.size).toBe(0);
  act(() => tree.update(<Read workspace={workspace} generation="g2" />));
  expect(maps[0]!.size).toBe(0);
  maps[0]!.set(conversationKey("n", "a"), createInteractiveState());
  act(() => tree.update(<Read workspace={emptyWorkspace} generation="g2" />));
  expect(maps[0]!.size).toBe(0);
  act(() => tree.unmount());
});
