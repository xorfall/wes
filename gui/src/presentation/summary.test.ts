import { expect, it } from "vitest";
import { httpValue } from "../testing/http-response";
import { matchesHttp } from "../views/http-response";
import { summarize } from "./summary";

it("uses the HTTP renderer's structural contract for parsed bodies and keeps adverse status as data", () => {
  const value = httpValue();
  const type = value.type.kind === "record" ? { ...value.type, fields: value.type.fields.map(field => field.name === "body" ? { name: "body", type: { kind: "record" as const, name: "", fields: [] } } : field) } : value.type;
  const data = { ...(value.data as object), status: 503, body: { message: "synthetic unavailable" } };
  expect(matchesHttp(type, data)).toBe(true);
  expect(summarize(type, data)).toEqual([{ text: "503", tone: "warn" }]);
  expect(matchesHttp(type, { ...data, version: undefined })).toBe(false);
  expect(matchesHttp(type, { ...data, status: 999 })).toBe(false);
});
