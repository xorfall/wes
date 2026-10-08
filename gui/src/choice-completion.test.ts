import { describe, expect, it } from "vitest";
import { complete } from "./complete";
import { emptyCatalogue, type Catalogue, type Choices, type Parameter } from "./vocabulary";

const parameter = (name: string, type: string, choices?: Choices, allowed: string[] = []): Parameter =>
  ({ name, type, required: false, allowed, content: "", ...(choices ? { choices } : {}) });

const catalogue: Catalogue = {
  ...emptyCatalogue,
  providers: [{
    name: "orders", credentials: [], ready: true,
    capabilities: [{
      path: ["listOrders"], summary: "", result: "HttpResponse", safe: true,
      parameters: [
        parameter("status", "Text", { kind: "text", members: ["open", "true", "123", "on hold", 'say "hi"'], total: 5, complete: true }),
        parameter("limit", "Int", { kind: "int", members: ["10", "25", "50"], total: 3, complete: true }),
        parameter("rate", "Decimal", { kind: "decimal", members: ["0.10", "1.5000000000000000001"], total: 2, complete: true }),
        parameter("paid", "Bool", { kind: "bool", members: ["true", "false"], total: 2, complete: true }),
        parameter("region", "Text", { kind: "text", members: Array.from({ length: 64 }, (_, i) => `r${String(i).padStart(3, "0")}`), total: 200, complete: false }),
        parameter("none", "Text", { kind: "text", members: [], total: 0, complete: true }),
        parameter("legacy", "Text", undefined, ["a", "b"]),
        parameter("both", "Text", { kind: "text", members: ["x"], total: 1, complete: true }, ["y"]),
      ],
    }],
  }],
  templates: [{ name: "report", body: "orders listOrders status:?status", parameters: [parameter("status", "Text", { kind: "text", members: ["open", "closed"], total: 2, complete: true })] }],
};
const at = (line: string) => complete(line, line.length, catalogue, ["recent"]);
const texts = (line: string) => at(line).items.map((item) => item.text);

describe("declared contract choices", () => {
  it("always quotes text members so they stay Text", () => {
    expect(texts("orders listOrders status:")).toEqual([
      'status:"open"', 'status:"true"', 'status:"123"', 'status:"on hold"', 'status:"say \\"hi\\""',
    ]);
  });

  it("writes numbers and booleans exactly as the engine spelled them", () => {
    expect(texts("orders listOrders limit:")).toEqual(["limit:10", "limit:25", "limit:50"]);
    expect(texts("orders listOrders rate:1")).toEqual(["rate:1.5000000000000000001"]);
    expect(texts("orders listOrders paid:")).toEqual(["paid:true", "paid:false"]);
  });

  it("narrows by what has been typed, with or without an opening quote", () => {
    expect(texts("orders listOrders status:o")).toEqual(['status:"open"', 'status:"on hold"']);
    expect(texts('orders listOrders status:"o')).toEqual(['status:"open"', 'status:"on hold"']);
  });

  it("says how much a preview left out, even when nothing in it matches, without promising a wider search", () => {
    const shown = at("orders listOrders region:");
    expect(shown.items).toHaveLength(64);
    expect(shown.hint).toBe("Only 64 of 200 declared values are listed; write any other in full.");
    const none = at("orders listOrders region:r199");
    expect(none.items).toEqual([]);
    expect(none.hint).toBe("Only 64 of 200 declared values are listed; write any other in full.");
  });

  it("reports an empty intersection instead of offering nothing silently", () => {
    const shown = at("orders listOrders none:");
    expect(shown.items).toEqual([]);
    expect(shown.hint).toBe("No value satisfies every rule for this parameter.");
  });

  it("prefers declared choices over legacy allowed values, and keeps allowed when there are none", () => {
    expect(texts("orders listOrders both:")).toEqual(['both:"x"']);
    expect(texts("orders listOrders legacy:")).toEqual(["legacy:a", "legacy:b"]);
  });

  it("keeps references available on a parameter with choices", () => {
    expect(texts("orders listOrders status:$re")).toEqual(["status:$recent"]);
  });

  it("offers template parameter choices the same way", () => {
    expect(texts("report status:")).toEqual(['status:"open"', 'status:"closed"']);
  });
});
