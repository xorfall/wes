import { afterEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { SandboxPanel } from "./SandboxPanel";
import type { Engine, SandboxObservation } from "../engine";

vi.mock("../views/Result", () => ({ ValueView: ({ value }: { value: unknown }) => <pre>{JSON.stringify(value)}</pre> }));
let tree: ReactTestRenderer | undefined;
afterEach(async () => { if (tree) await act(async () => tree!.unmount()); tree=undefined; vi.useRealTimers(); });
const opened: SandboxObservation = { name:"preview", reference:"preview", inspect:false, state:"active", generation:"g", value:{ type:{kind:"unknown"}, provenance:{}, data:{count:1} } };
it("updates one observation without submitting commands and closing does not cancel", async () => {
  vi.useFakeTimers();
  const readSandbox=vi.fn().mockResolvedValue({...opened,value:{...opened.value,data:{count:2}}});
  const submit=vi.fn(); const cancel=vi.fn(); const close=vi.fn();
  const engine={readSandbox,submit,cancel} as unknown as Engine;
  await act(async () => { tree=create(<SandboxPanel engine={engine} opened={opened} onClose={close}/>); });
  await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
  expect(readSandbox).toHaveBeenCalledTimes(1);
  expect(JSON.stringify(tree!.toJSON())).toContain('count');
  expect(submit).not.toHaveBeenCalled(); expect(cancel).not.toHaveBeenCalled();
  tree!.root.findByType("button").props.onClick(); expect(close).toHaveBeenCalledOnce();
  const signal=readSandbox.mock.calls[0]![3] as AbortSignal;
  await act(async () => tree!.unmount()); tree=undefined;
  expect(signal.aborted).toBe(true);
  await vi.advanceTimersByTimeAsync(5000); expect(readSandbox).toHaveBeenCalledTimes(1);
});
it("waits for each read and labels a failed observation without claiming new values", async () => {
  vi.useFakeTimers(); let reject!: (error: Error) => void;
  const readSandbox=vi.fn().mockImplementation(() => new Promise((_,fail)=>{reject=fail;}));
  await act(async () => { tree=create(<SandboxPanel engine={{readSandbox} as unknown as Engine} opened={opened} onClose={()=>{}}/>); });
  await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
  expect(readSandbox).toHaveBeenCalledTimes(1);
  await act(async () => reject(new Error("Sandbox is restarting")));
  expect(tree!.root.findByProps({role:"alert"}).children.join("")).toContain("Previous observation");
});

it.each(["stopped", "not run"] as const)("describes %s without claiming work is running and explains real provider effects", async state => {
  const observation = {...opened, state, reference:null, value:{...opened.value,data:{state,members:[]}}};
  await act(async () => { tree=create(<SandboxPanel engine={{} as Engine} opened={observation} onClose={()=>{}}/>); });
  const text=JSON.stringify(tree!.toJSON());
  expect(text).toContain(state === "stopped" ? "Sandbox stopped" : "Only the definition was restored");
  expect(text).not.toContain("leaves the sandbox active");
  expect(text).toContain("Providers use their real configured targets");
});

it("does not confuse user member data with sandbox lifecycle metadata", async () => {
  const observation={...opened, reference:null, value:{...opened.value,data:{kind:"Sandbox",state:"stopped"}}};
  await act(async () => { tree=create(<SandboxPanel engine={{} as Engine} opened={observation} onClose={()=>{}}/>); });
  const text=JSON.stringify(tree!.toJSON());
  expect(text).toContain("leaves the sandbox active");
  expect(text).not.toContain("Sandbox stopped.");
  expect(tree!.root.findAllByType("table")).toHaveLength(0);
});

/* Synthetic full observations in the backend shape: kind, persistence, state, members. */
const full = (members: unknown[], state = "active") => ({ kind:"Sandbox", persistence:"definition only", state, members });
const observe = (data: unknown, over: Partial<SandboxObservation> = {}): SandboxObservation => ({ ...opened, reference:"preview", value:{ type:{kind:"unknown"}, provenance:{}, data }, ...over });
async function show(observation: SandboxObservation, engine = {} as Engine) {
  if (tree) await act(async () => tree!.unmount());
  await act(async () => { tree=create(<SandboxPanel engine={engine} opened={observation} onClose={()=>{}}/>); });
  return tree!;
}
type Node = ReactTestRenderer["root"];
const rows = (tree: ReactTestRenderer) => tree.root.findAllByType("tr").filter(row => row.parent?.type === "tbody");
const textOf = (node: Node) => node.findAll(() => true).flatMap(item => item.children.filter((child): child is string => typeof child === "string")).join(" ");

it("renders every named member of a full read immediately, with type, state and readable values", async () => {
  const engine = {} as Engine;
  const tree = await show(observe(full([
    { name:"count", type:"Int", state:"ready", value:3 },
    { name:"label", type:"String", state:"ready", value:"three" },
    { name:"rows", type:"List<Record>", state:"ready", value:[{id:1},{id:2}] },
  ])), engine);
  expect(rows(tree)).toHaveLength(3);
  const [count, label, list] = rows(tree).map(textOf);
  expect(count).toContain("count"); expect(count).toContain("Int"); expect(count).toContain("ready"); expect(count).toContain("3");
  expect(label).toContain("String"); expect(label).toContain('"three"');
  // Complex values use the bounded value view with the engine, never a re-read or a run.
  const view = rows(tree)[2]!.findByType("pre");
  expect(view.children.join("")).toContain('"data":[{"id":1},{"id":2}]');
  expect(list).toContain("List<Record>");
  expect(tree.root.findAllByProps({ className:"sandbox-member-view" })).toHaveLength(1);
  expect(tree.root.findAllByType("caption")[0]!.children.join("")).toBe("3 members");
});

it("shows member errors and says why an inspection carries no values", async () => {
  const tree = await show(observe(full([
    { name:"broken", type:"Int", state:"failed", error:{ code:"SYN001", message:"synthetic failure" } },
    { name:"count", type:"Int", state:"ready" },
  ]), { inspect:true }));
  const [broken, count] = rows(tree).map(textOf);
  expect(broken).toContain("SYN001 · synthetic failure");
  expect(count).toContain("not read when inspecting");
  expect(JSON.stringify(tree.toJSON())).toContain("inspect");
});

it("keeps a structured member error's summary and reaches its full record in a disclosure", async () => {
  const error = { code:"SYN003", message:"synthetic structured failure", issues:[{ path:"count", reason:"synthetic issue" }], locations:[{ start:2, end:5 }], retryable:false };
  const tree = await show(observe(full([{ name:"broken", type:"Int", state:"failed", error }])));
  const row = rows(tree)[0]!;
  expect(row.findByProps({ role:"note" }).children.join("")).toBe("SYN003 · synthetic structured failure");
  const details = row.findByType("details");
  expect(details.findByType("summary").children.join("")).toBe("error details");
  const record = details.findByType("pre").children.join("");
  expect(record).toContain('"issues":[{"path":"count","reason":"synthetic issue"}]');
  expect(record).toContain('"locations":[{"start":2,"end":5}]');
  expect(record).toContain('"retryable":false');
  // A code and message alone are fully said by the summary; no empty disclosure is offered.
  const plain = await show(observe(full([{ name:"broken", type:"Int", state:"failed", error:{ code:"SYN001", message:"synthetic failure" } }])));
  expect(plain.root.findAllByType("details")).toHaveLength(0);
});

it("says a removed sandbox is not active and offers no cancel, from the observation's own state", async () => {
  const tree = await show(observe(full([{ name:"count", type:"Int", state:"ready", value:1 }], "active"), { state:"removed", reference:null }));
  const text = JSON.stringify(tree.toJSON());
  expect(text).toContain("Sandbox removed.");
  expect(text).not.toContain("leaves the sandbox active");
  expect(text).not.toContain(":cancel");
});

it("reveals nothing but name and state for a withheld member", async () => {
  const tree = await show(observe(full([{ name:"secret", state:"withheld" }, { name:"open", type:"Int", state:"ready", value:1 }])));
  const [secret] = rows(tree);
  expect(textOf(secret!)).toContain("secret"); expect(textOf(secret!)).toContain("withheld");
  expect(secret!.findAllByType("pre")).toHaveLength(0);
  expect(secret!.findByProps({ "data-label":"Type" }).findAllByType("code")).toHaveLength(0);
  // A row that claims to be withheld yet carries a type or value is not trusted as a full observation.
  const leaky = await show(observe(full([{ name:"secret", state:"withheld", type:"Password", value:"x" }])));
  expect(leaky.root.findAllByType("table")).toHaveLength(0);
});

it("never interprets a member read as a sandbox, even when its user data looks like one", async () => {
  const lookalike = full([{ name:"fake", type:"Int", state:"stopped", value:9 }], "stopped");
  const tree = await show(observe(lookalike, { reference:"preview.member", inspect:false }));
  expect(tree.root.findAllByType("table")).toHaveLength(0);
  expect(tree.root.findByType("pre").children.join("")).toContain('"kind":"Sandbox"');
  const text = JSON.stringify(tree.toJSON());
  expect(text).toContain("leaves the sandbox active"); expect(text).not.toContain("Sandbox stopped.");
  // An inspection of a member is a full observation filtered to that member.
  const inspected = await show(observe(full([{ name:"member", type:"Record", state:"ready" }]), { reference:"preview.member", inspect:true }));
  expect(rows(inspected)).toHaveLength(1);
});

it.each(["stopped", "not run"] as const)("lists %s members without values and keeps the lifecycle notice", async state => {
  const tree = await show(observe(full([{ name:"count", type:"Int", state }], state), { state }));
  expect(textOf(rows(tree)[0]!)).toContain("no current value");
  const text = JSON.stringify(tree.toJSON());
  expect(text).toContain(state === "stopped" ? "Sandbox stopped" : "Only the definition was restored");
  expect(text).not.toContain("leaves the sandbox active");
});

it("says a sandbox has no named members instead of drawing an empty table", async () => {
  const tree = await show(observe(full([])));
  expect(tree.root.findAllByType("table")).toHaveLength(0);
  expect(JSON.stringify(tree.toJSON())).toContain("no named members");
});

it("replaces polled member rows in place and stops polling on close", async () => {
  vi.useFakeTimers();
  const first = observe(full([{ name:"count", type:"Int", state:"ready", value:1 }]));
  const readSandbox = vi.fn().mockResolvedValue(observe(full([{ name:"count", type:"Int", state:"ready", value:2 }])));
  const close = vi.fn();
  await act(async () => { tree=create(<SandboxPanel engine={{ readSandbox } as unknown as Engine} opened={first} onClose={close}/>); });
  await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
  expect(readSandbox).toHaveBeenCalledWith("preview", false, "g", expect.any(AbortSignal));
  expect(textOf(rows(tree!)[0]!)).toContain("2");
  act(() => tree!.root.findByType("section").props.onKeyDown({ key:"Escape", stopPropagation(){} }));
  expect(close).toHaveBeenCalledOnce();
  const signal = readSandbox.mock.calls[0]![3] as AbortSignal;
  await act(async () => tree!.unmount()); tree = undefined;
  expect(signal.aborted).toBe(true);
  await vi.advanceTimersByTimeAsync(5000); expect(readSandbox).toHaveBeenCalledTimes(1);
});
