import { expect, it } from "vitest";
import { complete } from "./complete";
import { emptyCatalogue, type Catalogue } from "./vocabulary";

const catalogue: Catalogue = { ...emptyCatalogue, templates: [{ name: "products", body: "catalog list category:?category",
  parameters: [{ name: "category", type: "Category", required: true, allowed: ["books", "board games", "$literal"], content: "" }] }] };
const suggest = (line: string) => complete(line, line.length, catalogue, ["source"]).items;

it("offers template names distinctly from providers and meta commands", () => {
  expect(suggest("prod")).toEqual([{ text: "products", kind: "template", detail: "catalog list category:?category", separate: true }]);
  expect(suggest(":prod")).toEqual([]);
});
it("offers template parameters and typed enum values", () => {
  expect(suggest("products ").map(item => item.text)).toEqual(["category:"]);
  expect(suggest("products category:b").map(item => item.text)).toEqual(["category:books", 'category:"board games"']);
  expect(suggest("products category:").map(item => item.text)).toContain('category:"$literal"');
  expect(suggest("products category:books ")).toEqual([]);
});
it("keeps reference and output-channel completion available", () => {
  expect(suggest("products category:$source::e").map(item => item.text)).toEqual(["category:$source::error"]);
});
