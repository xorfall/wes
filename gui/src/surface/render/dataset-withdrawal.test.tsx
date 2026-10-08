import { expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { Engine } from "../../engine";
import type { DatasetRead } from "../../dataset-read";
import { decodeDatasetReference, type DatasetReference } from "../../presentation/dataset";
import type { StoredValue, TypeShape } from "../../protocol";
import { ELEMENT, field, metaWithin, sharedMeta, type ValueMeta } from "../../value-meta";
import { Cell, type CellBlock, type CellProps } from "../Cell";
import { ResultType } from "../ResultType";
import { datasetWithdrawals, withdrawnHeader, WITHDRAWN_FACTS, WITHDRAWN_TYPE_LABEL } from "./dataset-source";
import { ValueBlock } from "./ValueBlock";

/* Synthetic snapshots and contracts only: invented identifiers, digests and tone tables. */

const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const ROW: TypeShape = { kind: "record", name: "SyntheticRow", fields: [{ name: "label", type: TEXT }] };
const DATASET: TypeShape = { kind: "dataset", element: ROW };
const reference: DatasetReference = decodeDatasetReference({
  store: "00000000-0000-4000-8000-0000000000a1", dataset: "00000000-0000-4000-8000-0000000000b2",
  generation: "3", manifest: "00000000-0000-4000-8000-0000000000c3",
  manifestDigest: `sha256:${"0123456789abcdef".repeat(4)}`, manifestBytes: "512",
  schemaDigest: `sha256:${"fedcba9876543210".repeat(4)}`, records: "2", authorizationGeneration: "1",
})!;
const metaOf = (tones: Record<string, "ok" | "bad">): ValueMeta => ({
  version: 1, contract: { name: "SyntheticRow", digest: `sha256:${"ab".repeat(32)}` }, truncated: false,
  fields: { [field("label")]: { contract: { name: "SyntheticRow", digest: `sha256:${"ab".repeat(32)}` }, kind: "text", tones } },
});
const textOf = (node: ReactTestInstance): string => node.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
const roleOf = (tree: ReactTestRenderer, text: string) => tree.root.findAll(node => node.type === "span" && textOf(node) === text && typeof node.props.className === "string" && node.props.className.startsWith("mono-"))[0]?.props.className;
const flush = async () => { await act(async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); }); };

it("moves declarations under a holder without changing them and shares only identical metadata", () => {
  const meta = metaOf({ "synthetic 0": "ok" });
  expect(Object.keys(metaWithin(meta, ELEMENT).fields)).toEqual([`${ELEMENT}${field("label")}`]);
  expect(metaWithin(meta, ELEMENT).fields[`${ELEMENT}${field("label")}`]).toBe(meta.fields[field("label")]);
  expect(sharedMeta([{ meta }, { meta: metaOf({ "synthetic 0": "ok" }) }])).toBe(meta);
  expect(sharedMeta([{ meta }, { meta: metaOf({ "synthetic 0": "bad" }) }])).toBeUndefined();
  expect(sharedMeta([{ meta }, {}])).toBeUndefined();
});

it("clears a withdrawn result's type, metadata and facts in the cell header while keeping its place", () => {
  const stored = { handle: "stored-header", generation: "g-header" };
  const block: CellBlock = { key: "result", identity: { id: "result", label: "$result", glyph: "ready" }, open: true, hasValue: true,
    type: DATASET, meta: metaOf({}), typeLabel: "Dataset<SyntheticRow>", header: [{ text: "2 records", role: "mono-dim" }], stored, content: <span>body</span> };
  expect(withdrawnHeader(block)).toMatchObject({ key: "result", type: undefined, meta: undefined, typeLabel: WITHDRAWN_TYPE_LABEL, header: WITHDRAWN_FACTS, stored });
  const props: CellProps = { theme: "keys", state: "default", label: "c", rows: [{ segments: [{ text: "synthetic" }], nodes: [block.identity!] }], verdict: [], time: "09:14", blocks: [block] };
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<Cell {...props} />); });
  expect(textOf(tree.root)).toContain("2 records");
  expect(tree.root.findByType(ResultType).props.shape).toBe(DATASET);
  const headers = tree.root.findAll(node => node.type === "header" && node.props.className === "result-header").length;
  act(() => datasetWithdrawals.withdraw(stored));
  expect(textOf(tree.root)).not.toContain("2 records");
  expect(textOf(tree.root)).toContain("Access withdrawn");
  expect(tree.root.findByType(ResultType).props).toMatchObject({ shape: undefined, meta: undefined, label: WITHDRAWN_TYPE_LABEL });
  expect(tree.root.findAll(node => node.type === "header" && node.props.className === "result-header")).toHaveLength(headers);
  act(() => tree.unmount());
});

function pageWith(metas: readonly ValueMeta[]): DatasetRead {
  return { reference, stream: "outputs", lifecycle: "sealed", protected: false, persistence: "Durable", segmentBytes: "64",
    page: { first: "0", next: "2", extentExhausted: true, limitedBy: null, cursor: null,
      rows: metas.map((meta, at) => ({ ordinal: String(at), sourceStart: String(at), sourceEnd: String(at), value: { type: ROW, provenance: {}, data: { label: `synthetic ${at}` }, meta } as StoredValue })) } };
}

it.each([
  ["shared metadata", [metaOf({ "synthetic 0": "ok", "synthetic 1": "bad" }), metaOf({ "synthetic 0": "ok", "synthetic 1": "bad" })]],
  ["per-row metadata", [metaOf({ "synthetic 0": "ok" }), metaOf({ "synthetic 1": "bad" })]],
] as const)("draws declared tones from each row's own %s in the page grid", async (_name, metas) => {
  let resolve!: (read: DatasetRead) => void;
  const engine = { readDataset: vi.fn(() => new Promise<DatasetRead>(done => { resolve = done; })) } as unknown as Engine;
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<ValueBlock engine={engine} value={{ type: DATASET, provenance: {}, data: { kind: "dataset", reference: { ...reference } } }}
    stored={{ handle: `stored-tones-${_name}`, generation: "g-tones" }} cacheKey={`g-tones:${_name}`} mode="window" inCell={false} />); });
  resolve(pageWith(metas)); await flush();
  expect(roleOf(tree, "synthetic 0")).toBe("mono-ok");
  expect(roleOf(tree, "synthetic 1")).toBe("mono-bad");
  act(() => tree.unmount());
});
