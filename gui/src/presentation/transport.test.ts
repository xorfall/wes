import { afterEach, describe, expect, it, vi } from "vitest";
import type { TypeShape } from "../protocol";
import { registryStore } from "./registry-store";
import { followPresentations } from "./transport";
import { Registry } from "./registry";

/* Synthetic listings only; no engine is running. */

const sample: TypeShape = { kind: "record", name: "Sample", fields: [{ name: "n", type: { kind: "primitive", name: "INT" } }] };
const entry = (kind: string) => `version: 1\ntype: Sample\nfields:\n  n: Int\npresent:\n  kind: ${kind}\n`;
const listing = (revision: string, files: { name: string; text: string }[], problems: { name: string; message: string }[] = []) =>
  new Response(JSON.stringify({ revision, files, problems }), { status: 200 });

afterEach(() => registryStore.reset());

describe("following the data home's presentations", () => {
  it("should_ReplaceTheHomeEntriesAtOnce_When_EachFiniteReadAnswers", async () => {
    // Arrange
    const controller = new AbortController();
    const asked: string[] = [];
    const answers = [
      listing("r1", [{ name: "sample.yaml", text: entry("table") }]),
      listing("r2", [{ name: "sample.yaml", text: entry("fields") }]),
    ];
    const kinds: string[] = [];
    const off = registryStore.subscribe(() => kinds.push(registryStore.get().match(sample)?.entry.kind ?? "none"));
    const fetcher = (async (url: string) => {
      asked.push(url);
      const next = answers.shift();
      if (!next) { controller.abort(); throw new DOMException("aborted", "AbortError"); }
      return next;
    }) as unknown as typeof fetch;
    // Act
    await followPresentations(controller.signal, fetcher, async () => {});
    off();
    // Assert
    expect(asked).toEqual(["/presentations", "/presentations", "/presentations"]);
    expect(kinds).toEqual(["table", "fields"]);
  });

  it("should_StopQuietly_When_TheEngineHasNoRoute", async () => {
    const fetcher = (async () => new Response("", { status: 404 })) as unknown as typeof fetch;
    await followPresentations(new AbortController().signal, fetcher);
    expect(registryStore.get().match(sample)).toBeUndefined();
  });

  it("should_KeepTheLastValidEntry_When_TheEngineCannotReadTheFileAnyMore", () => {
    // Arrange
    const valid = Registry.core().withHome([{ name: "sample.yaml", text: entry("table") }]);
    // Act
    const unreadable = valid.withHome([], [{ name: "sample.yaml", message: "larger than 64 KiB" }]);
    // Assert
    expect(unreadable.match(sample)?.entry.kind).toBe("table");
    expect(unreadable.noticesFor(sample)).toEqual(["presentation sample.yaml: larger than 64 KiB"]);
    expect(valid.withHome([]).match(sample)).toBeUndefined();
  });
});

it("leaves connections idle between reads, deduplicates revisions and cancels the idle wait", async () => {
  vi.useFakeTimers();
  const controller = new AbortController();
  const fetcher = vi.fn(async () => listing("same", []));
  const changed = vi.fn();
  const off = registryStore.subscribe(changed);
  try {
    const following = followPresentations(controller.signal, fetcher);
    await vi.advanceTimersByTimeAsync(0);
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(changed).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(4999);
    expect(fetcher).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(changed).toHaveBeenCalledTimes(1);
    controller.abort();
    await following;
    await vi.advanceTimersByTimeAsync(10000);
    expect(fetcher).toHaveBeenCalledTimes(2);
  } finally { controller.abort(); off(); vi.useRealTimers(); }
});

it("retains entries through transient failure, then accepts a later revision", async () => {
  const controller = new AbortController();
  const fetcher = vi.fn().mockResolvedValueOnce(listing("one", [{name: "sample.yaml", text: entry("table")}]))
    .mockResolvedValueOnce(new Response("busy", {status: 503}))
    .mockResolvedValueOnce(listing("two", [{name: "sample.yaml", text: entry("fields")}]))
    .mockResolvedValueOnce(new Response("", {status: 404}));
  const kinds: (string | undefined)[] = [];
  await followPresentations(controller.signal, fetcher, async () => {
    kinds.push(registryStore.get().match(sample)?.entry.kind);
  });
  expect(kinds).toEqual(["table", "table", "fields"]);
});
