import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { ViewDatasetError, type DatasetPage, type DatasetPosition, type ViewDatasets } from "@wes/view-sdk";
import { CiRecordsView } from "../../../examples/ci-investigation/record-view/CiRecordsView";
import { clock, previousFrom, receiptLines, type CiLogLine, type CiRecordsInput } from "../../../examples/ci-investigation/record-view/model";

/*
 * Independently authored synthetic records for the CI investigation View: an invented job, steps,
 * texts, times and byte spans. No captured CI log, identifier or payload is used.
 */
afterEach(() => { vi.restoreAllMocks(); });

const BEYOND_SAFE = "9007199254740995";
const artifact = { run: "run-synthetic", attempt: 1, job: "job-synthetic", origin: "failed-jobs", digest: "0".repeat(64) };
const line = (ordinal: number, text: string, level: string | null = null): CiLogLine => ({
  artifact, ordinal, job: "job-synthetic", jobName: "Synthetic checks", step: "step-2", stepName: "Run unit tests",
  time: "2026-10-07T08:00:01.250Z", raw: text, text, level, group: null, byteStart: ordinal * 100, byteEnd: ordinal * 100 + 40, delimiterEnd: ordinal * 100 + 41,
});
const input = (records = BEYOND_SAFE, generation = "4"): CiRecordsInput => ({
  state: { group: null, job: "Synthetic checks", step: "Run unit tests" },
  outputs: { kind: "dataset", reference: { store: "00000000-0000-4000-8000-0000000000a1", dataset: "00000000-0000-4000-8000-0000000000b2",
    generation, manifest: "00000000-0000-4000-8000-0000000000c3", manifestDigest: `sha256:${"1".repeat(64)}`, manifestBytes: "512",
    schemaDigest: `sha256:${"2".repeat(64)}`, records, authorizationGeneration: "1" } },
  receipt: { status: "stopped", position: 4096, readPosition: 4200, extent: 9000, positionUnit: "bytes", inputRecords: 61, outputRecords: 61,
    finishApplied: false, sourceComplete: null, failureCode: null, failureMessage: null, exhausted: "work" },
});
const page = (first: string, rows: CiLogLine[], records = BEYOND_SAFE, cursor: string | null = "bmV4dA"): DatasetPage<CiLogLine> => ({
  generation: "4", records, lifecycle: "open", first, next: (BigInt(first) + BigInt(rows.length)).toString(), extentExhausted: cursor === null, cursor,
  rows: rows.map((value, at) => ({ ordinal: (BigInt(first) + BigInt(at)).toString(), sourceStart: String(value.byteStart), sourceEnd: String(value.byteEnd), value })),
});

type Call = { select: string; position?: DatasetPosition; limit?: number; resolve: (page: DatasetPage<CiLogLine>) => void; reject: (error: unknown) => void };
function reader() {
  const calls: Call[] = [];
  const datasets = {
    page: vi.fn((select: string, position?: DatasetPosition, limit?: number) => new Promise((resolve, reject) => { calls.push({ select, position, limit, resolve, reject } as Call); })),
    inspect: vi.fn(),
  } as unknown as ViewDatasets;
  return { datasets, calls };
}
const textOf = (item: ReactTestInstance): string => item.children.map(child => typeof child === "string" ? child : textOf(child)).join("");
const button = (tree: ReactTestRenderer, label: string) => tree.root.findAll(item => item.type === "button" && textOf(item) === label)[0]!;
const flush = async () => { await act(async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); }); };
const mount = (datasets: ViewDatasets | undefined, mode: "preview" | "expanded" | "window" = "expanded", columns = 120, value = input()) => {
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<CiRecordsView input={value} context={{ mode, allocation: { columns }, ...(datasets ? { datasets } : {}) }} />); });
  return tree;
};

describe("CiRecords", () => {
  it("reads one bounded page of /outputs and shows each record's ordinal, original line, bytes, time, level, job and step", async () => {
    const { datasets, calls } = reader();
    const tree = mount(datasets);
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ select: "/outputs", position: { from: "0" }, limit: 20 });
    calls[0]!.resolve(page("0", [line(7, "compiling synthetic module"), line(8, "assertion failed: synthetic expectation", "error")]));
    await flush();
    const rows = tree.root.findAll(item => item.type === "li" && String(item.props.className).split(" ").includes("ci-records-row"));
    expect(rows).toHaveLength(2);
    expect(textOf(rows[1]!)).toContain("assertion failed: synthetic expectation");
    expect(textOf(rows[1]!)).toContain("800–840");
    expect(textOf(rows[1]!)).toContain("08:00:01.250");
    expect(textOf(rows[1]!)).toContain("Synthetic checks · Run unit tests");
    expect(rows[1]!.props.className).toContain("status-bad");
    expect(textOf(tree.root)).toContain("records 0–1 of 9 007 199 254 740 995");
    act(() => tree.unmount());
  });

  it("states the receipt in its own words and numbers without inventing a status", () => {
    const lines = receiptLines(input().receipt).map(item => item.text);
    expect(lines).toEqual([
      "stopped · committed through 4 096 of 9 000 bytes · read through 4 200",
      "61 input records · 61 output records",
      "source completion unknown",
      "stopped at its work bound",
    ]);
    expect(clock(null)).toBe("");
  });

  it("pages by cursor and steps back exactly beyond the safe integer range, keeping the controls in place", async () => {
    const { datasets, calls } = reader();
    const tree = mount(datasets, "preview");
    expect(calls[0]!.limit).toBe(5);
    calls[0]!.resolve(page("9007199254740990", [line(1, "a"), line(2, "b"), line(3, "c"), line(4, "d"), line(5, "e")]));
    await flush();
    const next = button(tree, "next ›");
    act(() => next.props.onClick());
    expect(calls[1]!.position).toEqual({ cursor: "bmV4dA" });
    calls[1]!.resolve(page("9007199254740995", [], BEYOND_SAFE, null));
    await flush();
    expect(button(tree, "next ›")).toBe(next);
    act(() => button(tree, "‹ previous").props.onClick());
    expect(calls[2]!.position).toEqual({ from: "9007199254740990" });
    expect(previousFrom("3", 5)).toBe("0");
    act(() => tree.unmount());
  });

  it("says the end of committed records is not the end of the analysis", async () => {
    const { datasets, calls } = reader();
    const tree = mount(datasets);
    calls[0]!.resolve(page("0", [line(1, "only line")], "1", null));
    await flush();
    expect(textOf(tree.root)).toContain("end of the committed records · the analysis may still commit more");
    act(() => tree.unmount());
  });

  it("clears every record on withdrawal, keeps a labelled page while busy, and offers reading again only where it can help", async () => {
    const { datasets, calls } = reader();
    const tree = mount(datasets);
    calls[0]!.resolve(page("0", [line(1, "synthetic visible record")]));
    await flush();
    act(() => button(tree, "next ›").props.onClick());
    calls[1]!.reject(new ViewDatasetError("busy"));
    await flush();
    expect(textOf(tree.root)).toContain("Readers are busy · previous page shown");
    expect(textOf(tree.root)).toContain("synthetic visible record");
    act(() => button(tree, "read again").props.onClick());
    calls[2]!.reject(new ViewDatasetError("withdrawn"));
    await flush();
    expect(textOf(tree.root)).toContain("Access withdrawn");
    expect(textOf(tree.root)).not.toContain("synthetic visible record");
    expect(tree.root.findAll(item => item.type === "button" && textOf(item) === "read again")).toHaveLength(0);
    act(() => tree.unmount());
  });

  it("says so when the host cannot read records, and reads nothing", () => {
    const tree = mount(undefined);
    expect(textOf(tree.root)).toContain("Records cannot be read here");
    act(() => tree.unmount());
  });

  it("starts again at the first record of a new snapshot", async () => {
    const { datasets, calls } = reader();
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<CiRecordsView input={input()} context={{ mode: "expanded", datasets }} />); });
    calls[0]!.resolve(page("0", [line(1, "first")]));
    await flush();
    act(() => button(tree, "next ›").props.onClick());
    act(() => tree.update(<CiRecordsView input={input(BEYOND_SAFE, "5")} context={{ mode: "expanded", datasets }} />));
    expect(calls.at(-1)!.position).toEqual({ from: "0" });
    act(() => tree.unmount());
  });

  it("gives narrow allocations the text instead of the time and byte columns", async () => {
    const { datasets, calls } = reader();
    const tree = mount(datasets, "expanded", 48);
    calls[0]!.resolve(page("0", [line(3, "narrow record")]));
    await flush();
    expect(tree.root.findAll(item => item.props.className === "ci-records-bytes screen-label")).toHaveLength(0);
    expect(textOf(tree.root)).toContain("narrow record");
    act(() => tree.unmount());
  });
});
