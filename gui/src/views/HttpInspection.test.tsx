import { expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { HttpInspection, httpTrace, shouldPollTrace } from "./HttpInspection";
it("renders captured attempt identity and partial failed evidence safely", () => {
  const trace = { schema: 1, node: "node", run: "attempt-a", profile: "http", state: "failed", persistence: "private", dropped: 2, events: [
    { kind: "http.response", elapsedMs: 12, details: { status: 422, headers: [{ name: "set-cookie", value: "[REDACTED]" }] } },
    { kind: "http.body", elapsedMs: 20, details: { preview: "<script>secret</script>", complete: false } },
  ] };
  const html = renderToStaticMarkup(<HttpInspection data={{ trace }} />);
  expect(html).toContain("attempt-a"); expect(html).toContain("failed"); expect(html).toContain("private");
  expect(html).toContain("422"); expect(html).toContain("+12 ms"); expect(html).toContain("2 observations omitted");
  expect(html).not.toContain("<script>"); expect(html).toContain("&lt;script&gt;");
  expect(httpTrace({ events: [] })).toBeUndefined();
});

it("stops completed and missing inspection polls and follows active or refreshed attempts", () => {
  const trace = { schema: 1, profile: "http", events: [], state: "completed" };
  expect(shouldPollTrace("ready", trace)).toBe(false);
  expect(shouldPollTrace("failed", undefined)).toBe(false);
  expect(shouldPollTrace("running", trace)).toBe(true);
  expect(shouldPollTrace("cancelled", { ...trace, state: "running" })).toBe(true);
});
