import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, expect, it, vi } from "vitest";
import { LimitsSettings } from "./LimitsSettings";

afterEach(() => vi.unstubAllGlobals());

it("explains an unsupported host in the actual settings panel and offers no budget save", async () => {
  const fetcher = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => new Response("Not configured", { status: 501 }));
  vi.stubGlobal("fetch", fetcher);
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(<LimitsSettings />); });
  try {
    const alert = tree.root.findByProps({ role: "alert" }).children.join("");
    expect(alert).toContain("WesDesk desktop");
    expect(alert).toContain("wes --serve");
    expect(alert).toContain("cannot apply changes here");
    const save = tree.root.findAllByType("button").find(button => button.children.join("") === "Save")!;
    expect(save.props.disabled).toBe(true);
    expect(tree.root.findAllByProps({ inputMode: "decimal" })).toHaveLength(0);
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(fetcher.mock.calls[0]?.[0]).toBe("/operating-budgets");
  } finally { act(() => tree.unmount()); }
});
