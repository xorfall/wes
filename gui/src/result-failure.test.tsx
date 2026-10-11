import { afterEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { resultFailure } from "./result-failure";
import { ResultReadError, ResultReader } from "./result-reader";
import { applicationLog } from "./application-log";
import { recordExecutionFailure } from "./engine-diagnostics";
import { ReadStatus } from "./surface/ReadStatus";
import { ResultDemand } from "./surface/results";
import { Engine } from "./engine";
import { StorageError } from "./storage-error";

const trees: ReactTestRenderer[] = [];
afterEach(() => { act(() => trees.splice(0).forEach(tree => tree.unmount())); vi.unstubAllGlobals(); vi.useRealTimers(); applicationLog.clear(); });
const cases = [
  ["RUN007", "VALUE_CAPACITY", "capacity", 413],
  ["RUN008", "VALUE_UNAVAILABLE", "unavailable", 404],
  ["RUN009", "VALUE_ACCESS_REFUSED", "forbidden", 403],
  ["RUN010", "VALUE_REVISION_CONFLICT", "stale", 409],
  ["RUN011", "VALUE_OWNER_UNAVAILABLE", "closed", 503],
  ["RUN012", "VALUE_CORRUPT", "corrupt", 500],
  ["RUN013", "VALUE_STORAGE_UNCONFIRMED", "uncertain", 409],
  ["RUN014", "VALUE_READ_FAILED", "failed", 500],
] as const;

it.each(cases)("keeps %s distinct through result reads and execution guidance", async (code, readCode, kind, status) => {
  const problem = { code: readCode, message: "Synthetic read explanation.", retryable: false, context: { operation: "read-value" as const, handle: "exact-result" } };
  const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: problem }), { status }));
  vi.stubGlobal("window", { fetch });
  const reader = new ResultReader(() => "g", () => 10000);
  const error = await reader.read("exact-result").catch(error => error);
  expect(error).toBeInstanceOf(ResultReadError);
  expect(error.kind).toBe(kind);
  expect(reader.isWithdrawn("exact-result")).toBe(false);
  expect(fetch).toHaveBeenCalledTimes(1);
  const demand = new ResultDemand(() => Promise.reject(error), () => {});
  demand.update(["exact-result"]);
  await demand.reads.get("exact-result")!.pending;
  expect(demand.reads.get("exact-result")!.problemCode).toBe(readCode);
  const retry = vi.fn();
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<ReadStatus problem={error.message} problemCode={readCode} onRetry={retry} />); }); trees.push(tree);
  const presentation = resultFailure(readCode)!;
  expect(resultFailure(code)?.kind).toBe(presentation.kind);
  expect(JSON.stringify(tree.toJSON())).toContain(presentation.title);
  expect(JSON.stringify(tree.toJSON())).toContain(presentation.next);
  expect(tree.root.findAllByType("button")).toHaveLength(kind === "failed" ? 1 : 0);
  recordExecutionFailure({ event: "failed", node: "fixture", reason: "fixture", error: { id: code, code, message: "Synthetic read explanation.", causeId: "", issues: [] } }, { workspace: "fixture" });
  expect(applicationLog.snapshot().at(-1)?.next).toBe(presentation.next);
  expect(retry).not.toHaveBeenCalled();
  demand.dispose();
});

it("does not classify arbitrary text, inherited object keys or an unexplained HTTP status", () => {
  for (const code of [undefined, "toString", "__proto__", "SYN001", "RUN011: missing"]) expect(resultFailure(code)).toBeUndefined();
  expect(new ResultReadError(503).kind).toBeUndefined();
  expect(resultFailure("VALUE_SESSION_CHANGED")?.kind).toBe("stale");
});

it("keeps an analysis read's structured corruption code and public message", async () => {
  const generation = "g";
  const engine = Object.assign(new Engine(), { generation }) as Engine;
  const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: {
    code: "ANALYSIS_CORRUPT", message: "Stored result validation failed.", retryable: false,
  } }), { status: 500 }));
  vi.stubGlobal("fetch", fetch);
  const error = await engine.readSandbox("saved", false, generation, new AbortController().signal).catch(error => error);
  expect(error).toBeInstanceOf(StorageError);
  expect(error.code).toBe("ANALYSIS_CORRUPT");
  expect(error.message).toBe("ANALYSIS_CORRUPT: Stored result validation failed.");
  expect(fetch).toHaveBeenCalledTimes(1);
});
