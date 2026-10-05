import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { gzipSync } from "node:zlib";
import { fixtures, response } from "../../../tools/view-dev/work/fixtures";
import { durationView } from "../testing/view-modules/view";
import { valueViewModules, createValueViewRegistry } from "./registry";
import { matchesHttp, httpViewModule } from "./http/module";
import { prepareSync, PreparedCache } from "../presentation/prepare";
import { present } from "../presentation/present";
import { Registry } from "../presentation/registry";
import type { Context } from "../presentation/types";
import { ValueBlock } from "../surface/render/ValueBlock";
import { PeekScreen } from "../surface/screens/Peek";
import { OpenScreen } from "../surface/screens/Open";
import { openRoute, readOpenRoute } from "../surface/open-route";
import { viewsFor } from "../surface/result-views";
import type { StoredValue } from "../protocol";

const context: Context = { mode: "preview", columns: 80, lines: 6, density: "normal", locale: "en-GB", timeZone: "UTC" };
const sample = (id: string) => fixtures.find(it => it.id === id)!.value;
const content = (tree: ReactTestRenderer) => JSON.stringify(tree.toJSON());
const disposers: (() => void)[] = [];
afterEach(() => { disposers.splice(0).reverse().forEach(fn => fn()); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe("real view host and tool-authored modules", () => {
  it("registers another module once for cell, peek, open and routing without edits to those surfaces", async () => {
    disposers.push(valueViewModules.register(durationView));
    const value = sample("module");
    expect(viewsFor({ value }).map(it => it.name)).toEqual(["duration-demo"]);
    expect(readOpenRoute(openRoute("n1", "duration-demo"))?.tab).toBe("duration-demo");
    for (const element of [
      <ValueBlock value={value} cacheKey="example:1" mode="preview" />,
      <PeekScreen what="value" value={value} top={[]} subject={[]} />,
      <OpenScreen value={value} viewing={{ value }} top={[]} subject={[]} tab="duration-demo" />,
    ]) {
      let tree!: ReactTestRenderer;
      await act(async () => { tree = create(element); });
      expect(content(tree)).toContain("seconds");
      expect(content(tree)).toContain("1.84");
      act(() => tree.unmount());
    }
  });

  it("uses deterministic matching, rejects duplicate/reserved ids and disposes registration", () => {
    const registry = createValueViewRegistry();
    const notify = vi.fn(), off = registry.subscribe(notify);
    const remove = registry.register(durationView);
    expect(() => registry.register(durationView)).toThrow("unique");
    expect(() => registry.register({ ...durationView, id: "result" })).toThrow();
    expect(registry.find(sample("module").type, sample("module").data)?.id).toBe("duration-demo");
    remove(); off();
    expect(registry.get()).toEqual([]);
    expect(notify).toHaveBeenCalledTimes(2);
  });

  it("contains a module rendering failure and leaves the structural value readable", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    disposers.push(valueViewModules.register({ ...durationView, Component() { throw new Error("synthetic renderer bug"); } }));
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<ValueBlock value={sample("module")} mode="window" cacheKey="failed-renderer" />); });
    expect(content(tree)).toContain("View unavailable");
    expect(content(tree)).toContain("milliseconds");
    expect(content(tree)).toContain("1840");
    act(() => tree.unmount());
  });

  it("falls back when matching/presentation throws and does not expose the thrown payload", () => {
    disposers.push(valueViewModules.register({ ...durationView, present() { throw new Error("private marker"); } }));
    const view = present({ prepared: prepareSync(sample("module")), context, registry: Registry.core() });
    expect(view.root.kind).toBe("fields");
    expect(view.notices.join()).toContain("showing data");
    expect(JSON.stringify(view)).not.toContain("private marker");
  });
});

describe("HTTP view contract", () => {
  it("finishes failed preparation without leaving a decoding spinner or losing stored bytes", async () => {
    const value = response(200, "application/json", gzipSync('{"value":7}'));
    (value.data as { headers: unknown[] }).headers.push({ name: "content-encoding", value: "gzip" });
    vi.spyOn(httpViewModule, "prepareAsync").mockRejectedValueOnce(new Error("synthetic failure"));
    const cache = new PreparedCache();
    await new Promise<void>(done => { cache.read("failure", value, done); });
    const prepared = cache.read("failure", value);
    expect(prepared.pending).toBe(false);
    expect(JSON.stringify(prepared.data)).toContain((value.data as { body: string }).body);
    expect(JSON.stringify(prepared.data)).not.toContain('"pending":true');
  });

  it("shares HTTP decoding with view discovery instead of decoding on each tab check", () => {
    const value = response(200, "application/json", '{"ok":true}');
    const prep = vi.spyOn(httpViewModule, "prepare");
    viewsFor({ value }); viewsFor({ value });
    expect(prep).toHaveBeenCalledTimes(1);
  });

  it.each(fixtures.filter(it => it.id !== "module"))("renders the actual $id example without an engine or network", async ({ value }) => {
    const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
    const original = JSON.stringify(value);
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<PeekScreen what="value" value={value} top={[]} subject={[]} />); });
    expect(tree.root.findAllByProps({ "aria-label": "HTTP response" })).toHaveLength(1);
    expect(tree.root.findAllByType("script")).toHaveLength(0);
    expect(tree.root.findAllByType("img")).toHaveLength(0);
    expect(fetch).not.toHaveBeenCalled();
    expect(JSON.stringify(value)).toBe(original);
    act(() => tree.unmount());
  });

  it("matches structure rather than a nominal name and rejects malformed envelopes", () => {
    const value = sample("ok");
    expect(matchesHttp({ ...value.type, name: "OtherName" } as StoredValue["type"], value.data)).toBe(true);
    expect(matchesHttp({ kind: "record", name: "HttpResponse", fields: [] }, value.data)).toBe(false);
    expect(matchesHttp(value.type, { ...(value.data as object), status: 0 })).toBe(false);
    expect(matchesHttp(value.type, { ...(value.data as object), headers: [{ name: "x", value: 2 }] })).toBe(false);
  });

  it("keeps status separate from run state and respects even a one-line preview budget", () => {
    for (const lines of [1, 2, 3, 6]) {
      const shown = present({ prepared: prepareSync(sample("rejected")), facts: { state: "ready" }, context: { ...context, lines }, registry: Registry.core() });
      expect(shown.root.kind).toBe("view");
      expect(shown.summary.facts).toEqual([{ text: "400", tone: "ink" }]);
      expect(shown.lines).toBeLessThanOrEqual(lines);
    }
  });

  it("renders natively, keeps duplicate headers and exposes original bytes on demand", async () => {
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<ValueBlock value={sample("ok")} cacheKey="http:full" mode="window" />); });
    expect(tree.root.findByProps({ role: "tabpanel", "aria-label": "Body" }).props.hidden).toBe(false);
    expect(tree.root.findByProps({ role: "tabpanel", "aria-label": "Headers · 4" }).props.hidden).toBe(true);
    expect(tree.root.findAllByType("iframe")).toHaveLength(0);
    expect(tree.root.findAllByProps({ download: "body.bin" })).toHaveLength(0);
    await act(async () => tree.root.findAllByProps({ role: "tab" }).find(it => it.children.join("").startsWith("Headers"))!.props.onClick());
    expect(tree.root.findAllByType("th").map(it => it.children.join(""))).toEqual(["content-type", "x-request-id", "set-cookie", "set-cookie"]);
    await act(async () => tree.root.findAllByProps({ role: "tab" }).find(it => it.children.join("") === "Raw")!.props.onClick());
    expect(tree.root.findAllByProps({ download: "body.bin" })).toHaveLength(1);
    expect(content(tree)).toContain("Copy original");
    act(() => tree.unmount());
  });

  it("does not invent original bytes for structural bodies or swallow JSON errors", async () => {
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<ValueBlock value={sample("structured")} cacheKey="http:structural" mode="window" />); });
    await act(async () => tree.root.findAllByProps({ role: "tab" }).find(it => it.children.join("") === "Raw")!.props.onClick());
    expect(content(tree)).toContain("not retained");
    expect(content(tree)).toContain("BIN-42");
    await act(async () => tree.update(<ValueBlock value={sample("invalid")} cacheKey="http:invalid" mode="window" />));
    expect(content(tree)).toContain("Body is not valid JSON");
    expect(content(tree)).toContain("incomplete");
    act(() => tree.unmount());
  });

  it("refuses ambiguous content decoding, retains both declared headers", () => {
    const value = response(200, "application/json", "{}");
    const data = value.data as { headers: { name: string; value: string }[] };
    data.headers.push({ name: "Content-Type", value: "text/html" });
    const prepared = prepareSync(value);
    expect(JSON.stringify(prepared)).toContain("Conflicting Content-Type");
    expect(data.headers.filter(it => it.name.toLowerCase() === "content-type")).toHaveLength(2);
  });

  it("shares pending preparation and completion with every reader of the same immutable value", async () => {
    const value = response(200, "application/json", gzipSync('{"value":7}'));
    (value.data as { headers: unknown[] }).headers.push({ name: "content-encoding", value: "gzip" });
    const cache = new PreparedCache();
    let done!: () => void; const landed = new Promise<void>(resolve => { done = resolve; });
    const a = vi.fn(), b = vi.fn(done);
    expect(cache.read("cell", value, a).pending).toBe(true);
    expect(cache.read("peek", value, b).pending).toBe(true);
    await landed;
    expect(a).toHaveBeenCalledTimes(1); expect(b).toHaveBeenCalledTimes(1);
    expect(cache.read("cell", value)).toBe(cache.read("peek", value));
    expect(JSON.stringify(cache.read("cell", value))).toContain('"value":7');
  });
});

it("renders received contract mismatches with structured body and original-byte evidence", async () => {
  const source = response(200, "application/json", '{"title":42}');
  const recordType = source.type as Extract<StoredValue["type"], { kind: "record" }>;
  const value: StoredValue = { ...source,
    type: { ...recordType, fields: [...recordType.fields.map(f => f.name === "body" ? { ...f, type: { kind: "unknown" as const } } : f), { name: "originalBody", type: { kind: "primitive", name: "BYTES" } }] },
    data: { ...(source.data as object), originalBody: (source.data as { body: string }).body, body: { title: 42 }, bodyKind: "json", validation: { state: "mismatch", issues: [{ path: "/title", code: "TYP005", message: "Expected Text" }] } },
  };
  let tree!: ReactTestRenderer;
  await act(async () => { tree=create(<PeekScreen what="value" value={value} top={[]} subject={[]} />); });
  expect(content(tree)).toContain("Documented shape differs");
  expect(content(tree)).not.toContain("TYP005");
  await act(async () => tree.root.findAllByType("button").find(it => it.children.join("") === "Show fields")!.props.onClick());
  expect(content(tree)).toContain("Expected Text");
  expect(content(tree)).toContain("/title"); expect(content(tree)).toContain("42");
  expect(content(tree)).not.toContain("not retained");
  act(() => tree.unmount());
});

it("formats retained JSON only on request and copies the exact original text",async()=>{
 const original='{"number":9007199254740993,"items":[1,2]}';
 const value=response(200,"application/json",original);let tree!:ReactTestRenderer;
 const writeText=vi.fn().mockResolvedValue(undefined);vi.stubGlobal("navigator",{clipboard:{writeText}});
 await act(async()=>{tree=create(<ValueBlock value={value} cacheKey="raw-format" mode="window"/>);});
 const button=(name:string)=>tree.root.findAllByType("button").find(it=>it.children.join("")===name)!;
 await act(async()=>tree.root.findAllByProps({role:"tab"}).find(it=>it.children.join("")==="Raw")!.props.onClick());
 expect(tree.root.findByProps({"aria-label":"Retained body bytes"}).children.join("")).toBe(original);
 act(()=>button("Formatted JSON").props.onClick());
 const formatted=tree.root.findByProps({"aria-label":"Formatted body JSON"}).children.join("");
 expect(formatted).toContain('  "number": 9007199254740993');expect(formatted).toContain("\n");
 await act(async()=>button("Copy original").props.onClick());expect(writeText).toHaveBeenCalledWith(original);
 act(()=>button("Original").props.onClick());
 expect(tree.root.findByProps({"aria-label":"Retained body bytes"}).children.join("")).toBe(original);
 act(()=>tree.unmount());
});
it("keeps malformed text visible when JSON formatting cannot be used",async()=>{
 const original='{"incomplete":';let tree!:ReactTestRenderer;
 await act(async()=>{tree=create(<ValueBlock value={response(200,"application/json",original)} cacheKey="raw-invalid" mode="window"/>);});
 act(()=>tree.root.findAllByProps({role:"tab"}).find(it=>it.children.join("")==="Raw")!.props.onClick());
 act(()=>tree.root.findAllByType("button").find(it=>it.children.join("")==="Formatted JSON")!.props.onClick());
 expect(tree.root.findByProps({"aria-label":"Retained body bytes"}).children.join("")).toBe(original);
 expect(content(tree)).toContain("JSON formatting unavailable");
 act(()=>tree.unmount());
});
