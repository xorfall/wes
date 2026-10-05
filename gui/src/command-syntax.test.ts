import { expect, it } from "vitest";
import { commandTokens, type SyntaxRole } from "./command-syntax";

const commands = [{ name: "list", takes: ["types", "nodes"] }, { name: "type", takes: ["load", "check"] }, { name: "node", takes: ["refresh"] }];
const role = (source: string, selected: SyntaxRole) => commandTokens(source, commands)
  .filter(token => token.role === selected).map(token => token.text);

it("separates the screenshot's meta command and as key from reference, value and binding", () => {
  const source = ":type check $demo_trace as:Text > checked";
  expect(role(source, "meta")).toEqual([":type", "check"]);
  expect(role(source, "arg")).toEqual(["as:"]);
  expect(commandTokens(source).filter(token => token.role).map(token => token.text)).toEqual([":type", "as:"]);
});

it("recognizes annotations, L0 paths, L1 lines and UI commands independently", () => {
  const source = '@trace(http) @env{dev}\ncatalog echo value:"yes"\n:list types\n/theme paper';
  expect(role(source, "note")).toEqual(["@trace(http)", "@env{dev}"]);
  expect(role(source, "verb")).toEqual(["catalog", "echo"]);
  expect(role(source, "meta")).toEqual([":list", "types"]);
  expect(role(source, "ui")).toEqual(["/theme"]);
  expect(role(source, "str")).toEqual(['"yes"']);
});

it("separates help's provider path from the meta command, also after annotations", () => {
  for (const source of [":help sensor-demo history", "@env{dev} :help sensor-demo history"]) {
    expect(role(source, "meta")).toEqual([":help"]);
    expect(role(source, "verb")).toEqual(["sensor-demo", "history"]);
  }
  expect(role(":help type", "meta")).toEqual([":help", "type"]);
  expect(role(":help node refresh", "meta")).toEqual([":help", "node", "refresh"]);
});

it("only treats advertised L1 subcommands as keywords, leaving positional names plain", () => {
  expect(role(":type check $data as:Customer", "meta")).toEqual([":type", "check"]);
  expect(role(":save my-workspace\n:load my-workspace\n:refresh result", "meta"))
    .toEqual([":save", ":load", ":refresh"]);
  expect(role(":list types\n:help sensor-demo history", "meta")).toEqual([":list", "types", ":help"]);
  expect(commandTokens(":list types").filter(token => token.role === "meta").map(token => token.text)).toEqual([":list"]);
});

it("handles escaped strings and treats URLs, references and string content as values", () => {
  const source = String.raw`service call url:https://example.invalid/a:b value:"a \"quoted\" @trace(http) as:word" input:$result::error`;
  expect(role(source, "arg")).toEqual(["url:", "value:", "input:"]);
  expect(role(source, "str")).toEqual([String.raw`"a \"quoted\" @trace(http) as:word"`]);
  expect(role(source, "note")).toEqual([]);
});

it("keeps multiline calculation body words and comments out of command roles", () => {
  const source = ':calc {\n // @trace as:no\n return [{item: "green", total: 42}];\n /* :list types */\n} > rows\n:type check $rows as:List<Unknown>';
  expect(role(source, "meta")).toEqual([":calc", ":type", "check"]);
  expect(role(source, "verb")).toEqual([]);
  expect(role(source, "arg")).toEqual(["item:", "total:", "as:"]);
  expect(role(source, "str")).toEqual(['"green"']);
});

it("preserves exact source including partial input, annotation strings, Unicode and whitespace", () => {
  const samples = ["", "\t\n ", '@env{"dev space"} :list types', '@trace(http) :type check $a as:Text',
    '@env{nested{value:"x}y"}} service call', '@env{unfinished', 'echo value:"unfinished\\',
    "echo value:'it\\'s fine'", ":calc {\n return {şehir: \"İstanbul 😀\"};\n}",
    'catalog echo value:"<script>alert(1)</script>"'];
  for (const source of samples) expect(commandTokens(source).map(token => token.text).join("")).toBe(source);
  expect(role(samples[2]!, "str")).toEqual(['"dev space"']);
  // Display must always advance, even for malformed punctuation sequences.
  for (const character of ['@', ':', '$', '>', '*', '\\', '{', '}', '"', "'", '\n', '😀']) {
    const source = character.repeat(257);
    expect(commandTokens(source).map(token => token.text).join("")).toBe(source);
  }
});

it("restarts provider/meta highlighting at pipes without splitting shell strings or calc boolean operators", () => {
  const source = 'sh run cmd:"ls | grep name"|http request url:input.url|:calc { return true || false; }';
  expect(commandTokens(source).map(token => token.text).join("")).toBe(source);
  expect(role(source, "verb")).toEqual(["sh", "run", "http", "request"]);
  expect(role(source, "meta")).toEqual([":calc"]);
  expect(role(source, "str")).toEqual(['"ls | grep name"']);
});
