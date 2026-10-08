import { describe, expect, it } from "vitest";
import { calculationAt } from "./calculation-completion";
import { complete, foreignAt } from "./complete";
import { emptyCatalogue } from "./vocabulary";
const catalogue = { ...emptyCatalogue, calculation: { keywords: ["return", "const", "let"], operations: ["map", "filter", "reduce", "length", "call", "parseJson"], methods: ["map", "filter", "reduce", "length"] }, providers: [{ name: "catalog", ready: true, credentials: [], capabilities: [{ path: ["get", "row"], summary: "", result: "Unknown", safe: true, parameters: [] }] }] };
const suggestions = (text: string) => complete(text, text.length, catalogue, ["rows", "other"]);
it("completes the current pipe stage with original offsets and a local input", () => {
  const provider = ':calc { return "😀 |"; }|cat';
  expect(suggestions(provider).items).toContainEqual(expect.objectContaining({ text: "catalog", kind: "provider" }));
  expect(suggestions(provider).from).toBe(provider.length - 3);
  const calc = ':calc { return true || false; } |\n :calc { return inp';
  expect(suggestions(calc).items).toContainEqual({ text: "input", kind: "local" });
  expect(suggestions(calc).from).toBe(calc.length - 3);
  expect(suggestions(':calc { return inp').items).not.toContainEqual({ text: "input", kind: "local" });
  expect(suggestions(':calc { return 1; } | :calc { return input; }\n:calc { return inp').items).not.toContainEqual({ text: "input", kind: "local" });
  const quoted = 'sh run cmd:"ls | grep cat';
  expect(suggestions(quoted).items).not.toContainEqual(expect.objectContaining({ text: "catalog" }));
});
describe("calculation editing", () => {
  it("offers package operations, locals and references in incomplete multiline blocks", () => {
    expect(suggestions(":calc {\n const rows=[1,2];\n return ro").items).toContainEqual({ text: "rows", kind: "local" });
    expect(suggestions(":calc { return $ro").items).toContainEqual({ text: "$rows", kind: "reference" });
    expect(suggestions(":calc { return fil").items).toContainEqual({ text: "filter", kind: "operation" });
    expect(suggestions(":calc { function f(öğe) { return öğ").items).toContainEqual({ text: "öğe", kind: "local" });
  });
  it("ignores braces in strings/comments and closes at the real boundary", () => {
    expect(calculationAt(":calc { const x='}'; /* } */ return ")).toBeDefined();
    expect(calculationAt(":calc { return 1; } > output")).toBeUndefined();
    expect(suggestions(":calc { // return").items).toEqual([]);
    expect(suggestions(":calc { return 'fil").items).toEqual([]);
    expect(calculationAt("catalog echo value:\":calc {\"")).toBeUndefined();
  });
  it("offers static provider paths and local fields without foreign execution completion", () => {
    expect(suggestions(":calc { call('cat").items).toContainEqual({ text: "catalog", kind: "provider" });
    expect(suggestions(":calc { call('catalog',['get','r").items).toContainEqual({ text: "row", kind: "capability" });
    expect(suggestions(":calc { const row={amount:2}; return row.am").items).toContainEqual({ text: "amount", kind: "field" });
    const text=":calc { return 'cmd:fil";
    expect(foreignAt(text,text.length,catalogue)).toBeUndefined();
  });
  it("returns UTF-16 replacement positions after astral characters", () => {
    const text=":calc { const emoji='😀'; return fil";
    expect(suggestions(text).from).toBe(text.length-3);
  });
});

it("offers Iter namespace and methods only from announced vocabulary",()=>{
 const vocab={...catalogue,calculation:{...catalogue.calculation,operations:[...catalogue.calculation.operations,"iter.lines","iter.items","take","collect"],methods:[...catalogue.calculation.methods,"take","collect"]}};
 const source=":calc { return iter.li";
 expect(complete(source,source.length,vocab,[]).items).toContainEqual({text:"lines",kind:"operation"});
 const method=":calc { const rows=iter.lines('x'); return rows.ta";
 expect(complete(method,method.length,vocab,[]).items).toContainEqual({text:"take",kind:"operation"});
});

describe("text operations from the announced vocabulary", () => {
  // Synthetic vocabulary records; nothing here depends on what the engine currently announces.
  const announced = { ...catalogue, calculation: { ...catalogue.calculation, operations: [...catalogue.calculation.operations, "regexTest", "stripAnsi", "text", "take"], methods: [...catalogue.calculation.methods, "take"] } };
  const offered = (text: string, vocab: typeof catalogue = announced) => complete(text, text.length, vocab, ["raw"]).items;
  it("takes method permission only from the announced methods, never from an operation's name", () => {
    // `take` is announced as an operation here but not as a method: absence means function-only.
    const undeclared = { ...announced, calculation: { ...announced.calculation, methods: ["map"] } };
    expect(offered(":calc { return $raw.ta", undeclared)).toEqual([]);
    expect(offered(":calc { return $raw.ma", undeclared)).toEqual([{ text: "map", kind: "operation" }]);
    // An alias the vocabulary announces is offered under its exact announced name.
    const aliased = { ...announced, calculation: { ...announced.calculation, operations: [...announced.calculation.operations, "first"], methods: ["first"] } };
    expect(offered(":calc { return $raw.fi", aliased)).toEqual([{ text: "first", kind: "operation" }]);
    expect(offered(":calc { return $raw.fil", aliased)).toEqual([]);
  });
  it("offers them as functions only when the vocabulary announces them", () => {
    expect(offered(":calc pure { return reg")).toContainEqual({ text: "regexTest", kind: "operation" });
    expect(offered(":def clean(line: Text) -> Text as :calc pure { return strip")).toContainEqual({ text: "stripAnsi", kind: "operation" });
    expect(offered(":calc { return reg", catalogue)).not.toContainEqual(expect.objectContaining({ text: "regexTest" }));
    expect(offered(":calc { return strip", catalogue)).toEqual([]);
  });
  it("never offers them after a receiver, including a call result whose field is read", () => {
    expect(offered(":calc { return $raw.str")).toEqual([]);
    expect(offered(":calc { const line = 'x'; return line.reg")).toEqual([]);
    expect(offered(":calc { return stripAnsi($raw).te")).toEqual([]);
    expect(offered(":calc { return [$raw][0].str")).toEqual([]);
    // Announced methods remain available on a call result.
    expect(offered(":calc { return stripAnsi($raw).spans.ta")).toEqual([{ text: "take", kind: "operation" }]);
    expect(offered(":calc { return stripAnsi($raw).ta")).toEqual([{ text: "take", kind: "operation" }]);
  });
  it("lets locals and parameters shadow them and offers nothing inside quoted source", () => {
    expect(offered(":calc { const stripAnsi = x => x; return strip").filter(item => item.text === "stripAnsi")).toEqual([{ text: "stripAnsi", kind: "local" }]);
    expect(offered(":def scan(regexTest: Bool) -> Bool as :calc pure { return regex").filter(item => item.text === "regexTest")).toEqual([{ text: "regexTest", kind: "local" }]);
    expect(offered(":calc { return regexTest($raw, 'strip")).toEqual([]);
    expect(offered(":calc { return regexTest($raw, \"^reg")).toEqual([]);
    expect(offered(":calc { /* strip */ return reg")).toContainEqual({ text: "regexTest", kind: "operation" });
    expect(offered(":calc { // strip")).toEqual([]);
  });
});

it("completes pure calc bodies and explicit definition parameters", () => {
  expect(suggestions(":calc pure { return fil").items).toContainEqual({text:"filter",kind:"operation"});
  expect(suggestions(":def scale(input: Int, rate: Int) -> Int as :calc pure { return rat").items).toContainEqual({text:"rate",kind:"local"});
});


it("offers shadowing bindings and definition parameters as locals, preserving receiver methods", () => {
  for (const source of [
    ":calc { const length=4; return len",
    ":calc { return [1].map(length=>len",
    ":calc { function length(x){return x;} return len",
    ":def size(length:Int)->Int as :calc { return len",
  ]) {
    const offered = suggestions(source).items.filter(item => item.text === "length");
    expect(offered).toEqual([{text:"length",kind:"local"}]);
  }
  expect(suggestions(":calc { const length=4; return rows.len").items).toContainEqual({text:"length",kind:"operation"});
  expect(suggestions(":calc { const call=x=>x; return cal").items).toContainEqual({text:"call",kind:"local"});
});

it("does not offer provider metadata positions for a locally declared call function", () => {
  for (const source of [
    ":calc { const call=x=>x; return call('cat",
    ":calc { function call(x){return x;} return call('catalog',['get','r",
    ":calc { function f(call){ return call('cat",
    ":def echo(call:Text)->Text as :calc { return call('cat",
  ]) {
    expect(suggestions(source).items).toEqual([]);
  }
  expect(suggestions(":calc { return call('cat").items).toContainEqual(expect.objectContaining({text:"catalog",kind:"provider"}));
});
