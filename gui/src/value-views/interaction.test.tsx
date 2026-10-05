import { afterEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { InteractionController, type FrameScheduler, type InteractionPort } from "./interaction";
import { defineInteractiveView } from "./interactive";
import { valueViewModules } from "./registry";
import { counterView, counterFixture } from "../testing/view-modules/interactive";
import { ValueBlock } from "../surface/render/ValueBlock";
import { PeekScreen } from "../surface/screens/Peek";
import { OpenScreen } from "../surface/screens/Open";
const disposers: (() => void)[] = [];
afterEach(() => { disposers.splice(0).reverse().forEach(fn => fn()); vi.useRealTimers(); vi.restoreAllMocks(); });
const text = (tree: ReactTestRenderer) => JSON.stringify(tree.toJSON());
const flush = () => act(() => { vi.advanceTimersByTime(20); });

it("bounds messages, isolates reducer exceptions and coalesces notifications with cleanup", () => {
  let frame: (() => void) | undefined;
  const scheduler: FrameScheduler = { request: vi.fn(fn => { frame = fn; return 1; }), cancel: vi.fn() };
  const controller = new InteractionController(counterView.interaction!, null, scheduler);
  const notify = vi.fn(), off = controller.subscribe(notify);
  for (let i = 0; i < 1000; i++) expect(controller.emit({ kind: "increment" })).toBe(true);
  expect(scheduler.request).toHaveBeenCalledTimes(1);
  expect(controller.snapshot().state).toEqual({ count: 0 });
  frame!();
  expect(controller.snapshot().state).toEqual({ count: 1000 });
  expect(notify).toHaveBeenCalledTimes(1);
  expect(Object.isFrozen(controller.snapshot().state)).toBe(true);
  const cycle: Record<string, unknown> = {}; cycle.self = cycle;
  for (const malformed of [null, NaN, cycle, { kind: "unknown" }, { kind: "increment", large: "x".repeat(9000) }, { get kind() { throw Error("private"); } }]) expect(controller.emit(malformed)).toBe(false);
  expect(scheduler.request).toHaveBeenCalledTimes(2);
  frame!();
  expect(controller.snapshot().state).toEqual({ count: 1000 });
  expect(controller.snapshot().error).toBe("View interaction rejected");
  controller.emit({ kind: "increment" }); off();
  expect(scheduler.cancel).toHaveBeenCalledTimes(1);
  const failing = new InteractionController({ ...counterView.interaction!, reduce() { throw Error("private"); } }, null);
  expect(failing.emit({ kind: "increment" })).toBe(false);
  expect(JSON.stringify(failing.snapshot())).not.toContain("private");
});

it("works through cell, peek and open without prepare/present or engine work during interaction", async () => {
  vi.useFakeTimers();
  const present = vi.fn(counterView.present), prepare = vi.fn((_, data) => ({ data, pending: false }));
  disposers.push(valueViewModules.register({ ...counterView, present, prepare }));
  for (const element of [
    <ValueBlock value={counterFixture.value} cacheKey="cell" mode="preview" />,
    <PeekScreen what="value" value={counterFixture.value} top={[]} subject={[]} />,
    <OpenScreen value={counterFixture.value} viewing={{ value: counterFixture.value }} top={[]} subject={[]} tab="counter-demo" />,
  ]) {
    let tree!: ReactTestRenderer;
    act(() => { tree = create(element); });
    const calls = [present.mock.calls.length, prepare.mock.calls.length];
    act(() => { tree.root.findByProps({ "aria-label": "Increment local counter" }).props.onClick(); }); flush();
    expect(text(tree)).toContain('1');
    expect([present.mock.calls.length, prepare.mock.calls.length]).toEqual(calls);
    act(() => tree.unmount());
  }
});

it("keeps instances separate, survives data revisions and resets a workspace/source binding", () => {
  vi.useFakeTimers(); disposers.push(valueViewModules.register(counterView));
  let tree!: ReactTestRenderer;
  const view = (revision: string, binding: string) => <>
    <ValueBlock value={counterFixture.value} cacheKey={revision} bindingKey={binding} mode="expanded" />
    <ValueBlock value={counterFixture.value} cacheKey="other" mode="expanded" />
  </>;
  act(() => { tree = create(view("r1", "workspace-a:node")); });
  act(() => tree.root.findAllByType("button").find(b => b.props["aria-label"] === "Increment local counter")!.props.onClick()); flush();
  act(() => tree.update(view("r2", "workspace-a:node")));
  const counts = () => tree.root.findAllByProps({ "aria-label": "Increment local counter" }).map(b => b.children.join(""));
  expect(counts()).toEqual(["Count 1", "Count 0"]);
  act(() => tree.update(view("r3", "workspace-b:node")));
  expect(counts()).toEqual(["Count 0", "Count 0"]);
  act(() => tree.unmount());
});

it("only shares a controller through declared children and revokes an unmounted port", () => {
  vi.useFakeTimers();
  let held: InteractionPort<unknown, unknown> | undefined;
  const child = { ...counterView, Component: (props: import("./contract").ViewComponentProps) => { held = props.interaction; return <counterView.Component {...props} />; } };
  disposers.push(valueViewModules.register(child));
  const parent = defineInteractiveView<null, unknown, unknown>({
    id: "group-demo", matches: (_, data) => typeof data === "object" && data !== null && "group" in data, interaction: counterView.interaction!,
    present: (_, host) => ({ model: null, ownLines: 0, summary: [], children: ["a", "b"].map(id => host.child(id, counterFixture.value.type, counterFixture.value.data, { view: "counter-demo" })) }),
    Component: ({ children, renderChild }) => <>{children.map(child => renderChild(child, { interaction: "inherit" }))}</>,
  });
  disposers.push(valueViewModules.register(parent));
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<ValueBlock value={{ type: { kind: "record", name: "Group", fields: [] }, data: { group: true }, provenance: {} }} cacheKey="group-record" mode="expanded" />); });
  const buttons = () => tree.root.findAllByProps({ "aria-label": "Increment local counter" });
  expect(buttons()).toHaveLength(2);
  act(() => buttons()[0]!.props.onClick()); flush();
  expect(buttons().map(b => b.children.join(""))).toEqual(["Count 1", "Count 1"]);
  const old = held!; act(() => tree.unmount());
  expect(old.emit({ kind: "increment" })).toBe(false);
});
