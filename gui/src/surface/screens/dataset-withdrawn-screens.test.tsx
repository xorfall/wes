import { afterEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { decodeDatasetReference } from "../../presentation/dataset";
import type { StoredValue, TypeShape } from "../../protocol";
import type { WorkspaceNode } from "../../workspace";
import { datasetWithdrawals, storedIdentity, WITHDRAWN_TITLE } from "../render/dataset-source";
import { OpenScreen } from "./Open";
import { peekOf, PeekScreen } from "./Peek";

/* Synthetic result and descriptor only: invented identifiers and digests, no real store. */

afterEach(() => { vi.unstubAllGlobals(); });

const ROW: TypeShape = { kind: "record", name: "SyntheticRow", fields: [{ name: "label", type: { kind: "primitive", name: "TEXT" } }] };
const DATASET: TypeShape = { kind: "dataset", element: ROW };
const reference = decodeDatasetReference({
  store: "00000000-0000-4000-8000-0000000000a1", dataset: "00000000-0000-4000-8000-0000000000b2",
  generation: "3", manifest: "00000000-0000-4000-8000-0000000000c3",
  manifestDigest: `sha256:${"0123456789abcdef".repeat(4)}`, manifestBytes: "512",
  schemaDigest: `sha256:${"fedcba9876543210".repeat(4)}`, records: "9007199254740993", authorizationGeneration: "1",
})!;
const value: StoredValue = { type: DATASET, provenance: {}, data: { kind: "dataset", reference: { ...reference } } };
const node: WorkspaceNode = { id: "n1", name: "events", command: ":synthetic events", dependsOn: [], state: "ready", provenance: {}, cautions: [], kept: false, run: "r1", handle: "stored-screen" };
const textOf = (item: ReactTestInstance): string => item.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
const text = (tree: ReactTestRenderer) => textOf(tree.root);

it("carries a stored identity into peek material only with a value read from that handle", () => {
  const stored = storedIdentity(value, node.handle, "g-material")!;
  expect(stored).toEqual({ handle: "stored-screen", generation: "g-material" });
  expect(peekOf(node, value, stored)).toMatchObject({ value, stored });
  expect(peekOf(node, undefined, stored)).not.toHaveProperty("stored");
  expect(storedIdentity(undefined, node.handle, "g-material")).toBeUndefined();
  expect(storedIdentity(value, undefined, "g-material")).toBeUndefined();
  expect(storedIdentity(value, node.handle, undefined)).toBeUndefined();
});

it("clears a withdrawn result's type and copy text in a peek while its subject, source and copy action stay", async () => {
  const writeText = vi.fn(async () => undefined);
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  const stored = { handle: "stored-peek", generation: "g-peek" };
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<PeekScreen top={[]} subject={[{ text: "$events", role: "mono-ref" }]} what="type" {...peekOf(node, value, stored)} />); });
  expect(text(tree)).toContain("SyntheticRow");
  // A withdrawal recorded for another result changes nothing here.
  act(() => datasetWithdrawals.withdraw({ handle: "other", generation: "g-peek" }));
  expect(text(tree)).toContain("SyntheticRow");
  act(() => datasetWithdrawals.withdraw(stored));
  expect(text(tree)).not.toContain("SyntheticRow");
  expect(text(tree)).toContain(WITHDRAWN_TITLE);
  expect(text(tree)).toContain("$events");
  await act(async () => { tree.root.findByProps({ "aria-label": "Copy to the clipboard" }).props.onClick(); });
  expect(writeText).toHaveBeenCalledWith(WITHDRAWN_TITLE);
  // The command is the engine's record of what ran, not data read from the result.
  // The read-only source view holds the command as its field value, not as text children.
  act(() => tree.update(<PeekScreen top={[]} subject={[{ text: "$events", role: "mono-ref" }]} what="source" {...peekOf(node, value, stored)} />));
  expect(tree.root.findByProps({ "aria-label": "Command source" }).props.value).toContain(":synthetic events");
  expect(text(tree)).not.toContain(WITHDRAWN_TITLE);
  writeText.mockClear();
  await act(async () => { tree.root.findByProps({ "aria-label": "Copy to the clipboard" }).props.onClick(); });
  expect(writeText).toHaveBeenCalledWith(expect.stringContaining(":synthetic events"));
  expect(writeText).not.toHaveBeenCalledWith(expect.stringContaining("SyntheticRow"));
  act(() => tree.unmount());
});

it("clears a withdrawn result's value, JSON, facts and subject type in /open while the tab strip stays", () => {
  const stored = { handle: "stored-open", generation: "g-open" };
  const subject = [{ text: "$events", role: "mono-ref" as const }, { text: "  ·  " }, { text: "Dataset<SyntheticRow>", role: "mono-ink" as const }];
  const draw = (tab: string) => <OpenScreen top={[]} subject={subject} tab={tab} value={value} json={'{"records":"9007199254740993"}'}
    details={[[{ text: "records 9007199254740993" }]]} viewing={{ value, node, stored }} />;
  let tree!: ReactTestRenderer;
  act(() => { tree = create(draw("json")); });
  const tabs = () => tree.root.findAll(item => item.type === "button" && item.props.role === "tab").map(item => textOf(item));
  const before = tabs();
  expect(text(tree)).toContain("SyntheticRow");
  act(() => datasetWithdrawals.withdraw(stored));
  expect(tabs()).toEqual(before);
  for (const tab of ["result", "json", "details"]) {
    act(() => tree.update(draw(tab)));
    const shown = text(tree);
    for (const gone of ["SyntheticRow", "9007199254740993", "9 007 199", "sha256", "records"]) expect(shown, `${tab}: ${gone}`).not.toContain(gone);
    expect(shown).toContain(WITHDRAWN_TITLE);
    expect(shown).toContain("$events");
  }
  act(() => tree.unmount());
});
