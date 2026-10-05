import { expect, it } from "vitest";
import { applicationLog } from "./application-log";
import { EngineRefusal, recordEngineFailure } from "./engine-diagnostics";
it("keeps transport details in Logs while the transient notice explains the refusal", () => {
  applicationLog.clear();
  recordEngineFailure({request:"submit",cell:"test",text:":help",client:"fixture"}, new EngineRefusal(400,"STO003: plan expired","Submit command"), {workspace:"test"});
  const entry=applicationLog.snapshot().at(-1)!;
  expect(entry.message).toContain("HTTP 400"); expect(entry.detail).toBe("STO003: plan expired");
  expect(entry.notice).toContain("plan expired"); expect(entry.notice).not.toContain("HTTP");
  applicationLog.clear();
});
