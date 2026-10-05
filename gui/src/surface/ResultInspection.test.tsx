import { ValueView } from "../views/Result";
import { expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { renderToStaticMarkup } from "react-dom/server";
import { httpValue } from "../testing/http-response";
import { ReadableJson } from "./ResultInspection";
import { OpenScreen, type OpenTab } from "./screens/Open";
import type { ViewSubject } from "./result-views";
import type { StoredValue } from "../protocol";

/** A result and nothing else: what the views are asked about when there is no node behind it. */
const viewing = (value: StoredValue): ViewSubject => ({ value });
import * as http from "../views/http-response";

it("bounds long body display while keeping complete original content available", async () => {
  const body = '<script>throw new Error("never execute")</script>' + "x".repeat(250_000) + "BODY-END";
  const value = httpValue(body, [{ name: "set-cookie", value: "synthetic=one" }, { name: "set-cookie", value: "synthetic=two" }]);
  for (const tab of ["result", "json", "http", "details"] as OpenTab[]) {
    let tree!: ReactTestRenderer;
    await act(async () => { tree = create(<OpenScreen top={[]} subject={[]} tab={tab} value={value} viewing={viewing(value)} />); });
    const contents = () => tree.root.findAllByType("pre").map(pre => pre.children.join(""));
    if (tab === "json") {
      expect(contents().find(text=>text.startsWith("{"))!.length).toBe(16_384);
      while(tree.root.findAllByType("button").some(it=>it.children.join("").startsWith("show more JSON"))) {
        await act(async()=>tree.root.findAllByType("button").find(it=>it.children.join("").startsWith("show more JSON"))!.props.onClick());
      }
      expect(JSON.parse(contents().find(text=>text.startsWith("{"))!).body).toBe((value.data as {body:string}).body);
    }
    if (tab === "http" || tab === "result") {
      if (tab === "result") expect(tree.root.findAllByType(ReadableJson)).toHaveLength(0);
      const section = (name: string) => tree.root.findAllByProps({ role: "tab" }).find(it => it.children.join("").startsWith(name))!;
      await act(async () => section("Headers").props.onClick());
      expect(tree.root.findAllByType("td").map(td => td.children.join(""))).toContain("synthetic=two");
      await act(async () => section("Raw").props.onClick());
      expect(tree.root.findByProps({ "aria-label": "Retained body bytes" }).children.join("").length).toBe(16_384);
      const writeText = vi.fn().mockResolvedValue(undefined);
      vi.stubGlobal("navigator", { clipboard: { writeText } });
      const copy = tree.root.findAllByType("button").find(it => it.children.join("") === "Copy original")!;
      await act(async () => copy.props.onClick());
      expect(writeText).toHaveBeenCalledWith(body);
      for (let i = 0; i < 20; i++) {
        const more = tree.root.findAllByType("button").find(it => it.children.join("").startsWith("Show more ·"));
        if (!more) break;
        await act(async () => more.props.onClick());
      }
      expect(contents()).toContain(body);
      vi.unstubAllGlobals();
    }
    if (tab === "details") {
      expect(contents().some(text => text.includes((value.data as { body: string }).body))).toBe(false);
      expect(tree.root.findByProps({ download: "result.json" }).props.href).toMatch(/^blob:/);
    }
    expect(tree.root.findAllByType("script")).toHaveLength(0);
    expect(tree.root.findAll(node => node.props.dangerouslySetInnerHTML !== undefined)).toHaveLength(0);
    await act(async () => tree.unmount());
  }
  expect(renderToStaticMarkup(<ValueView value={value} />)).not.toContain("<script>");
}, 20_000);
it("keeps Open's result tab the result, gives a view its own tab, and takes no child keys", async () => {
  const onTab = vi.fn(); const value = httpValue(); let tree!: ReactTestRenderer;
  await act(async () => { tree = create(<OpenScreen top={[]} subject={[]} tab="result" onTab={onTab} value={value} viewing={viewing(value)} />); });
  const tabs = () => tree.root.findAllByProps({ role: "tab" }).filter(it => !it.props["aria-controls"]).map(chip => chip.children.join(""));
  expect(tabs()).toEqual(["result", "json", "http", "details"]);
  expect(tree.root.findAllByProps({ "aria-label": "HTTP response" })).toHaveLength(1);
  await act(async () => tree.root.findAllByProps({ role: "tab" }).find(chip => chip.children.join("") === "http")!.props.onClick());
  expect(onTab).toHaveBeenCalledWith("http");
  await act(async () => tree.update(<OpenScreen top={[]} subject={[]} tab="http" onTab={onTab} value={value} viewing={viewing(value)} />));
  expect(tree.root.findAllByProps({ "aria-label": "HTTP response" })).toHaveLength(1);
  const preventDefault = vi.fn();
  tree.root.findByProps({ role: "tabpanel", "aria-label": "http" }).props.onKeyDown({ key: "Tab", target: {}, currentTarget: {}, preventDefault });
  expect(preventDefault).not.toHaveBeenCalled();
  const passive = tree.root.findAllByProps({ className: "value-presented" }).find(node => node.props.onKeyDown)!;
  const stopPropagation = vi.fn(); passive.props.onKeyDown({ key: "Enter", metaKey: true, stopPropagation, preventDefault });
  expect(stopPropagation).toHaveBeenCalled(); expect(preventDefault).toHaveBeenCalled();
  await act(async () => tree.unmount());
});
it("falls back to the result when an address names a view this result does not admit", async () => {
  let tree!: ReactTestRenderer;
  const plain = { type: { kind: "unknown" as const }, provenance: {}, data: "ordinary" };
  await act(async () => { tree = create(<OpenScreen top={[]} subject={[]} tab="http" value={plain} viewing={viewing(plain)} />); });
  expect(tree.root.findAllByProps({ role: "tab" }).map(chip => chip.children.join(""))).toEqual(["result", "json", "details"]);
  expect(tree.root.findByProps({ role: "tabpanel" }).props["aria-label"]).toBe("result");
  await act(async () => tree.unmount());
});
it("ignores late decoding of a previous result", async () => {
  let resolve!: (body: http.BodyDisplay) => void;
  const decode = vi.spyOn(http, "decodeBody").mockImplementationOnce(() => new Promise(done => { resolve = done; }));
  let tree!: ReactTestRenderer;
  try {
    await act(async () => { tree = create(<ValueView value={httpValue("old", [{ name: "content-encoding", value: "gzip" }])} />); });
    await act(async () => tree.update(<ValueView value={httpValue("new")} />));
    await act(async () => resolve({ text: "stale", bytes: 5 }));
    expect(JSON.stringify(tree.toJSON())).toContain("new");
    expect(JSON.stringify(tree.toJSON())).not.toContain("stale");
  } finally { act(() => tree.unmount()); decode.mockRestore(); }
});
it("retains the stored HTTP Bytes in JSON independently of the body charset", async () => {
  const value = httpValue();
  Object.assign(value.data as object, { headers: [{ name: "Content-Type", value: "text/plain; charset=windows-1254" }], body: Buffer.from([0xdd, 0xfe]).toString("base64") });
  let tree!: ReactTestRenderer;
  await act(async () => { tree = create(<ReadableJson value={value} />); });
  expect(JSON.parse(tree.root.findByType("pre").children.join("")).body).toBe("3f4=");
  act(() => tree.unmount());
});

it("defers hidden JSON serialization and handles invalid display data locally",()=>{
 const circular:Record<string,unknown>={};circular.self=circular;
 const value:StoredValue={type:{kind:"unknown"},data:circular,provenance:{}};
 let tree!:ReactTestRenderer;act(()=>{tree=create(<ReadableJson value={value} active={false}/>);});
 expect(tree.toJSON()).toBeNull();
 act(()=>tree.update(<ReadableJson value={value} active/>));
 expect(tree.root.findByProps({role:"alert"}).children.join("")).toContain("JSON display unavailable · Circular JSON value");
 act(()=>tree.unmount());
});
