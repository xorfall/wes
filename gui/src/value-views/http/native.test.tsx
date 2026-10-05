import { useEffect, useState } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, expect, it, vi } from "vitest";
import { response } from "../../../../tools/view-dev/work/fixtures";
import { prepareSync, DecodedBytes } from "../../presentation/prepare";
import { present } from "../../presentation/present";
import { Registry } from "../../presentation/registry";
import { httpViewModule } from "./module";
import type { StoredValue } from "../../protocol";
import type { Context } from "../../presentation/types";

afterEach(() => { vi.restoreAllMocks(); });
const context: Context = { mode: "window", columns: 100, lines: 4000, density: "normal", locale: "en-GB", timeZone: "UTC" };
const typed = (): StoredValue => {
  const source = response(200, "application/json", ' { "amount" : 17 }\n');
  const type = source.type as Extract<StoredValue["type"], { kind: "record" }>;
  return { ...source, type: { ...type, fields: [...type.fields.map(f => f.name === "body" ? { ...f, type: { kind: "unknown" as const } } : f), { name: "originalBody", type: { kind: "primitive", name: "BYTES" } }] },
    data: { ...(source.data as object), body: { amount: 17 }, originalBody: (source.data as { body: string }).body, bodyKind: "json", validation: { state: "validated", issues: [] } } };
};
const shown = (value: StoredValue, overrides: Partial<Context> = {}) => {
  const node = present({ prepared: prepareSync(value), facts: { state: "ready" }, context: { ...context, ...overrides }, registry: Registry.core() }).root;
  if (node.kind !== "view") throw new Error("Expected HTTP presentation");
  return node;
};
const contents = (tree: ReactTestRenderer) => JSON.stringify(tree.toJSON());
const select = (tree: ReactTestRenderer, name: string) => {
  const button = tree.root.findAllByType("button").find(it => it.children.join("").replace(/^[▾▸] /, "").startsWith(name));
  if (!button) throw new Error(`Missing section ${name}`);
  button.props.onClick();
};

it("defers retained byte decoding until Raw and preserves body state across tabs and pane width", async () => {
  const value = typed(), atob = vi.spyOn(globalThis, "atob");
  const prepared = prepareSync(value);
  expect(atob).not.toHaveBeenCalled();
  expect((prepared.data as { originalBody: DecodedBytes }).originalBody.text).toBeUndefined();
  const url = vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:synthetic-http");
  const revoke = vi.spyOn(URL, "revokeObjectURL");
  const mounted = vi.fn();
  function StatefulBody() {
    const [count, setCount] = useState(0);
    useEffect(() => { mounted(); }, []);
    return <button onClick={() => setCount(n => n + 1)}>Body count {count}</button>;
  }
  const Component = httpViewModule.Component;
  const draw = (columns: number) => { const node = shown(value, { columns }); return <Component model={node.model} children={node.children} renderChild={() => <StatefulBody />} />; };
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(draw(100)); });
  expect(url).not.toHaveBeenCalled();
  expect(contents(tree)).not.toContain("contract ✓");
  expect(tree.root.findAllByType("iframe")).toHaveLength(0);
  act(() => tree.root.findAllByType("button").find(it => it.children.join("") === "Body count 0")!.props.onClick());
  await act(async () => select(tree, "Raw"));
  expect(url).toHaveBeenCalledOnce();
  expect(new TextDecoder().decode(await (url.mock.calls[0]![0] as Blob).arrayBuffer())).toBe(' { "amount" : 17 }\n');
  expect(contents(tree)).toContain("not the original HTTP message");
  await act(async () => tree.update(draw(35)));
  expect(tree.root.findByProps({ role: "region", "aria-label": "Raw" }).props.hidden).toBe(false);
  expect(url).toHaveBeenCalledOnce();
  await act(async () => select(tree, "Body"));
  expect(tree.root.findAllByType("button").some(it => it.children.join("") === "Body count 1")).toBe(true);
  expect(mounted).toHaveBeenCalledOnce();
  expect(tree.root.findAll(node => node.props.title !== undefined)).toHaveLength(0);
  act(() => tree.unmount());
  expect(revoke).toHaveBeenCalledWith("blob:synthetic-http");
});

it("keeps a one-line response inside its budget without rendering the body", async () => {
  const node = shown(typed(), { mode: "preview", lines: 1 });
  const renderChild = vi.fn(); const Component = httpViewModule.Component;
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(<Component model={node.model} children={node.children} renderChild={renderChild} />); });
  expect(contents(tree)).toContain("200 OK");
  expect(renderChild).not.toHaveBeenCalled();
  expect(tree.root.findAllByProps({ role: "tablist" })).toHaveLength(0);
  act(() => tree.unmount());
});

it("does not add an unbudgeted empty-body row to a two-line preview", async () => {
  const node = shown(response(204, "application/json", ""), { mode: "preview", lines: 2 });
  const Component = httpViewModule.Component;
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(<Component model={node.model} children={node.children} renderChild={() => null} />); });
  expect(contents(tree)).toContain("204 No Content");
  expect(tree.root.findAllByProps({ "aria-label": "Response body" })).toHaveLength(0);
  act(() => tree.unmount());
});

it("uses section navigation keys without overriding repeat or history", async () => {
  const node = shown(typed()), Component = httpViewModule.Component;
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(<Component model={node.model} children={node.children} renderChild={() => null} />); });
  const focus = vi.fn(), preventDefault = vi.fn(), stopPropagation = vi.fn();
  const currentTarget = { querySelectorAll: () => [{ focus }, { focus }, { focus }] };
  const press = (key: string) => tree.root.findByProps({ role: "tablist" }).props.onKeyDown({ key, preventDefault, stopPropagation, currentTarget });
  act(() => { press("r"); press("h"); });
  expect(preventDefault).not.toHaveBeenCalled();
  await act(async () => press("ArrowRight"));
  expect(tree.root.findByProps({ role: "tabpanel", "aria-label": "Headers · 4" }).props.hidden).toBe(false);
  await act(async () => press("End"));
  expect(tree.root.findByProps({ role: "tabpanel", "aria-label": "Raw" }).props.hidden).toBe(false);
  await act(async () => press("Home"));
  expect(tree.root.findByProps({ role: "tabpanel", "aria-label": "Body" }).props.hidden).toBe(false);
  expect(focus).toHaveBeenCalledTimes(3);
  expect(stopPropagation).toHaveBeenCalledTimes(3);
  act(() => tree.unmount());
});

it.each([
  [201, "location", "/resources/new", "Location: /resources/new"],
  [429, "retry-after", "12", "Retry-After: 12"],
  [401, "www-authenticate", 'Bearer realm="synthetic"', 'WWW-Authenticate: Bearer realm='],
] as const)("shows useful response facts for HTTP %s without tinting the status text or running anything", async (status, name, value, note) => {
  const source = response(status, "application/json", "{}");
  (source.data as { headers: unknown[] }).headers.push({ name, value });
  const node = shown(source); const Component = httpViewModule.Component;
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(<Component model={node.model} children={node.children} renderChild={() => null} />); });
  expect(contents(tree)).toContain(note.replaceAll('"', '\\"'));
  expect(tree.root.findByType("strong").props.className).toBeUndefined();
  expect(tree.root.findAllByProps({ className: `http-status-dot mono-${status >= 400 ? "warn" : "ok"}` })).toHaveLength(1);
  act(() => tree.unmount());
});
