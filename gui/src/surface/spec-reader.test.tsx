import { readFileSync } from "node:fs";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { descriptorPreview, readPreview } from "../draft-api";
import { SpecOperations } from "./SpecOperations";
import { SchemaFieldsTable } from "./SchemaProvenance";
import { SpecDocumentation } from "./SpecDocumentation";

const types = {
  Item: { base: "Record", description: "An inventory item.", fields: {
    owner: { type: "Option<Owner>", optional: false, description: "The owning person." },
    tags: { type: "List<Text>", optional: true },
  } },
  Owner: { base: "Record", description: "Owner details.", fields: { id: { type: "Int", optional: false, description: "Stable id." } } },
};
const document = { provider: "synthetic", types, operations: [
  { path: ["other"], method: "GET", route: "/other", auth: [], parameters: [], responses: { "204": null } },
  { path: ["list"], method: "GET", route: "/items", description: "Read **inventory**.\n\nDo not mutate items.", auth: [],
    parameters: [{ name: "limit", type: "Int", location: "query", required: false, description: "Maximum items." }],
    responses: { "200": "List<Item>" }, responseDescriptions: { "200": "Matching items.", "400": "Invalid input." } },
] };
const json = (tree: ReactTestRenderer) => JSON.stringify(tree.toJSON());
const text = (node: ReactTestInstance | string): string => typeof node === "string" ? node : node.children.map(text).join("");
/** One operation or type: its row and, when open, the detail under it. */
const groups = (tree: ReactTestRenderer) => tree.root.findAll(n => n.type === "div" && n.props.role === "rowgroup");
const cell = (group: ReactTestInstance, column: string) => text(group.find(n => n.type === "div" && n.props.role === "cell" && String(n.props.className).split(" ").includes(column)));
const fieldRow = (tree: ReactTestRenderer, field: string) => tree.root.findByProps({ "aria-label": `Evidence for ${field}` }).parent!.parent!.parent!;

describe("the shared spec reader", () => {
  it("retains descriptions in saved and draft previews, including absent requiredness", () => {
    const saved = descriptorPreview(document);
    expect(saved.operations[1]).toMatchObject({ description: "Read **inventory**.\n\nDo not mutate items.", responseDescriptions: { "400": "Invalid input." }, parameters: [{ required: false, description: "Maximum items." }] });
    const draft = readPreview({ ...document, operations: [{ ...document.operations[1], responses: [{ status: 200, type: "List<Item>" }], parameters: [{ name: "limit", type: "Int" }] }] })!;
    expect(draft.operations[0]!.parameters[0]!.required).toBeUndefined();
    expect(draft.operations[0]!.description).toBe(saved.operations[1]!.description);
  });

  it("filters without changing source targets and opens documentation only on disclosure", () => {
    const source = vi.fn(); const evidence = vi.fn(); let tree!: ReactTestRenderer;
    act(() => { tree = create(<SpecOperations operations={descriptorPreview(document).operations} onSource={source} onEvidence={evidence} problems={[{ target: "#/operations/1/parameters/0", severity: "error" }]}/>); });
    expect(json(tree)).toContain("Read **inventory**.");
    expect(json(tree)).not.toContain("Maximum items.");
    act(() => tree.root.findByProps({ "aria-label": "Filter spec operations" }).props.onChange({ target: { value: "items" } }));
    act(() => tree.root.findByProps({ "aria-label": "Expand operation list" }).props.onClick());
    expect(json(tree)).toContain("Maximum items."); expect(json(tree)).toContain("Matching items.");
    const count = () => tree.root.findByProps({ className: "spec-reader-count" }).children.join("");
    expect(count()).toBe("1 of 2 · 1 open");
    act(() => tree.root.findByProps({ "aria-label": "Filter spec operations" }).props.onChange({ target: { value: "other" } }));
    expect(count()).toBe("1 of 2"); // an opened operation hidden by the filter is not counted as open
    act(() => tree.root.findByProps({ "aria-label": "Filter spec operations" }).props.onChange({ target: { value: "items" } }));
    expect(count()).toBe("1 of 2 · 1 open");
    expect(json(tree)).toContain("documented · no result contract"); expect(json(tree)).toContain("1 blocking");
    act(() => tree.root.findByProps({ "aria-label": "Provenance of parameter limit of list" }).props.onClick());
    expect(evidence).toHaveBeenCalledWith("#/operations/1/parameters/0");
    act(() => tree.root.findAllByType("button").find(b => b.children.join("") === "go to source")!.props.onClick());
    expect(source).toHaveBeenCalledWith("#/operations/1");
    act(() => tree.root.findByProps({ "aria-label": "Collapse operation list" }).props.onClick());
    expect(json(tree)).not.toContain("Maximum items.");
    act(() => tree.unmount());
  });

  it("shows a summary-only operation's whole summary when opened without inventing a description", () => {
    const summary = "Latest trade for one symbol, including exchange, size, conditions and the tape it was reported on, as returned by the feed.";
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<SpecOperations operations={descriptorPreview({ provider: "synthetic", types: {}, operations: [{ path: ["latest"], method: "GET", route: "/latest", summary, auth: [], parameters: [], responses: { "200": null } }] }).operations}/>); });
    act(() => tree.root.findByProps({ "aria-label": "Expand operation latest" }).props.onClick());
    const row = groups(tree)[0]!;
    expect(row.props.className).toContain("spec-t-open");
    expect(row.findByProps({ className: "spec-doc-summary" }).children).toEqual([summary]);
    expect(row.findAllByProps({ className: "spec-documentation" })).toHaveLength(0);
    // The detail spans all seven columns; narrow panes, which hide the summary column, read it there.
    const detail = row.find(n => n.props.role === "cell" && n.props["aria-colspan"] !== undefined);
    expect(detail.props["aria-colspan"]).toBe(7);
    expect(detail.findByProps({ className: "spec-t-prose spec-t-narrow" }).children).toEqual([summary]);
    act(() => tree.unmount());
  });

  it("keeps a closed row's problem status in the operation cell, which narrow panes still show", () => {
    const operations = descriptorPreview(document).operations;
    const statusIn = (group: ReactTestInstance, column: string) => group.find(n => n.type === "div" && String(n.props.className).split(" ").includes(column)).findAll(n => String(n.props.className).includes("spec-row-status")).map(text);
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<SpecOperations operations={operations} problems={[{ target: "#/operations/1/parameters/0", severity: "error" }]}/>); });
    const list = groups(tree)[1]!;
    expect(statusIn(list, "spec-c-sum")).toEqual(["1 blocking"]);
    expect(statusIn(list, "spec-c-op")).toEqual(["1 blocking"]);
    expect(list.find(n => n.type === "span" && String(n.props.className).includes("spec-row-narrow")).props.className).toContain("spec-t-narrow");
    expect(statusIn(groups(tree)[0]!, "spec-c-op")).toEqual([]);
    act(() => tree.update(<SpecOperations operations={operations} stale />));
    expect(statusIn(groups(tree)[0]!, "spec-c-op")).toEqual(["stale"]);
    act(() => tree.unmount());
  });

  it("keeps method, route and operation in their own columns and generated response types out of the closed row", () => {
    const generated = "Api_catalog_latest_events_response_single_200f1dc5";
    const onType = vi.fn(); let tree!: ReactTestRenderer;
    const operations = descriptorPreview({ provider: "synthetic", types: { [generated]: { base: "Record", fields: { price: { type: "Decimal", optional: false } } } }, operations: [
      { path: ["CatalogLatestEventSingle"], method: "GET", route: "/v2/catalog/{symbol}/events/latest", summary: "Latest trade", auth: [],
        parameters: [{ name: "symbol", type: "Text", location: "path", required: true }, { name: "feed", type: "Text", location: "query", required: false }],
        responses: { "200": generated } },
      { path: ["ping"], method: "GET", route: "/ping", auth: [], parameters: [{ name: "echo", type: "Text", location: "query", required: false }], responses: {} },
    ] }).operations;
    act(() => { tree = create(<SpecOperations operations={operations} types={{ [generated]: { base: "Record", fields: {} } }} onType={onType}/>); });
    const header = tree.root.findAll(n => n.props.role === "columnheader").map(text);
    expect(header).toEqual(["", "method", "route · operation", "operation", "summary", "inputs", "returns"]);
    const [row, ping] = groups(tree);
    expect(cell(row!, "spec-c-method")).toBe("GET");
    expect(cell(row!, "spec-c-route")).toBe("/v2/catalog/{symbol}/events/latest");
    expect(cell(row!, "spec-c-op")).toBe("CatalogLatestEventSingle");
    expect(cell(row!, "spec-c-sum")).toBe("Latest trade");
    expect(cell(row!, "spec-c-in")).toBe("symbol, feed?");
    expect(cell(row!, "spec-c-ret")).toBe("200 · Record");
    expect(cell(ping!, "spec-c-in")).toBe("echo?");
    expect(cell(ping!, "spec-c-ret")).toBe("no responses");
    expect(json(tree)).not.toContain(generated);
    act(() => tree.root.findByProps({ "aria-label": "Filter spec operations" }).props.onChange({ target: { value: "LatestEventSingle" } }));
    expect(groups(tree)).toHaveLength(1);
    act(() => tree.root.findByProps({ "aria-label": "Expand operation CatalogLatestEventSingle" }).props.onClick());
    const link = tree.root.findByProps({ "aria-label": "Responses of CatalogLatestEventSingle" }).findByProps({ className: "spec-type-link" });
    expect(link.children.join("")).toBe(`${generated} ›`);
    act(() => link.props.onClick());
    expect(onType).toHaveBeenCalledWith(generated);
    act(() => tree.unmount());
  });

  it("opens an operation in place with inputs, body and every response, pointing at original indices", () => {
    const evidence = vi.fn(); const source = vi.fn(); let tree!: ReactTestRenderer;
    const operations = descriptorPreview({ provider: "synthetic", types: { Order: { base: "Record", fields: {} } }, operations: [
      { path: ["first"], method: "GET", route: "/first", auth: [], parameters: [], responses: {} },
      { path: ["createOrder"], method: "POST", route: "/orders", summary: "Place an order.", auth: [{ scheme: "bearer", secret: "shop_token" }],
        parameters: [{ name: "Idempotency-Key", type: "Text", location: "header" }, { name: "body", type: "Order", location: "body", required: true, description: "The lines to order." }],
        responses: { "201": "Order", "409": "Order", "422": null }, responseDescriptions: { "201": "Stored.", "404": "Not here." } },
    ] }).operations;
    act(() => { tree = create(<SpecOperations operations={operations} types={{ Order: { base: "Record", fields: {} } }} onEvidence={evidence} onSource={source} onType={() => {}} evidence={at => <p>{`recorded for ${at}`}</p>}/>); });
    act(() => tree.root.findByProps({ "aria-label": "Filter spec operations" }).props.onChange({ target: { value: "orders" } }));
    act(() => tree.root.findByProps({ "aria-label": "Expand operation createOrder" }).props.onClick());
    // Unknown requiredness stays unknown; the body is its own table with a link to its type.
    const inputs = tree.root.findByProps({ "aria-label": "Inputs of createOrder" });
    expect(inputs.findAll(n => n.props.role === "row").slice(1).map(r => r.findAll(n => n.props.role === "cell").map(text))).toEqual([["Idempotency-Key header", "header", "Text", "unknown", "evidence"]]);
    const body = tree.root.findByProps({ "aria-label": "Request body of createOrder" });
    expect(body.findByProps({ className: "spec-type-link" }).children.join("")).toBe("Order ›");
    expect(text(body)).toContain("The lines to order.");
    // Every response and documented-only status remains available, in order.
    const responses = tree.root.findByProps({ "aria-label": "Responses of createOrder" });
    expect(responses.findAll(n => n.props.role === "row").slice(1).map(r => text(r.findAll(n => n.props.role === "cell")[0]!))).toEqual(["201", "409", "422", "404"]);
    act(() => tree.root.findByProps({ "aria-label": "Provenance of parameter body of createOrder" }).props.onClick());
    expect(evidence).toHaveBeenLastCalledWith("#/operations/1/parameters/1");
    act(() => tree.root.findByProps({ "aria-label": "Provenance of response 409 of createOrder" }).props.onClick());
    expect(evidence).toHaveBeenLastCalledWith("#/operations/1/responses/1");
    act(() => tree.root.findByProps({ "aria-label": "Auth provenance of createOrder" }).props.onClick());
    expect(evidence).toHaveBeenLastCalledWith("#/operations/1/auth");
    expect(json(tree)).toContain("shop_token · no provenance");
    // Evidence opens on demand for the same original target.
    expect(json(tree)).not.toContain("recorded for");
    const layer = tree.root.findAll(n => n.type === "button" && n.props.className === "spec-t-layer")[0]!;
    act(() => layer.props.onClick());
    expect(json(tree)).toContain("recorded for #/operations/1");
    act(() => tree.root.findAllByType("button").find(b => text(b) === "go to source")!.props.onClick());
    expect(source).toHaveBeenCalledWith("#/operations/1");
    act(() => tree.unmount());
  });

  it("distinguishes required and nullable, expands nested fields and returns from a reference", () => {
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<SchemaFieldsTable types={types} provenance={undefined} onSelect={() => {}}/>); });
    expect(json(tree)).not.toContain("The owning person.");
    act(() => tree.root.findByProps({ "aria-label": "Expand type Item" }).props.onClick());
    expect(json(tree)).toContain("required"); expect(json(tree)).toContain("nullable");
    // Option<Owner> reads as Owner and nullable; its presence flag stays separate; the exact expression stays readable.
    const owner = fieldRow(tree, "owner");
    // The name cell also holds the disclosure; the name itself is its own button, whole.
    expect(owner.findByProps({ className: "spec-field-name" }).children).toEqual(["owner"]);
    expect(owner.findAll(n => n.props.role === "cell").map(text).slice(0, 4)).toEqual(["▸owner", "Owner › · nullable", "yes", "yes"]);
    expect(text(owner)).toContain("source type Option<Owner>");
    const tags = fieldRow(tree, "tags");
    expect(tags.findByProps({ className: "spec-field-name" }).children).toEqual(["tags"]);
    expect(tags.findAll(n => n.props.role === "cell").map(text).slice(0, 4)).toEqual(["tags", "List<Text>", "no", ""]);
    // Without provenance, fields add no per-row "no provenance" line; the name still opens evidence.
    expect(text(owner)).not.toContain("evidence");
    // The opened type's detail spans the types table's five columns.
    const itemGroup = tree.root.findByProps({ "aria-label": "Collapse type Item" }).parent!.parent!.parent!;
    expect(itemGroup.find(n => n.props.role === "cell" && n.props["aria-colspan"] !== undefined).props["aria-colspan"]).toBe(5);
    act(() => tree.root.findByProps({ "aria-label": "Expand owner" }).props.onClick());
    expect(json(tree)).toContain("Stable id.");
    act(() => tree.root.findAllByProps({ className: "spec-type-link" })[0]!.props.onClick());
    expect(json(tree)).toContain("Opened from Item.owner");
    expect(tree.root.findByProps({ "aria-label": "Collapse type Owner" }).props["aria-expanded"]).toBe(true);
    act(() => tree.root.findAllByType("button").find(b => b.children.join("") === "← back to Item.owner")!.props.onClick());
    expect(json(tree)).not.toContain("Opened from");
    // Following the reference opened Owner; going back closes it and keeps the nested field open.
    expect(tree.root.findByProps({ "aria-label": "Expand type Owner" }).props["aria-expanded"]).toBe(false);
    expect(tree.root.findByProps({ "aria-label": "Collapse owner" }).props["aria-expanded"]).toBe(true);
    act(() => tree.unmount());
  });

  it("keeps a nested field's disclosure and its short name as one group, indented per level", () => {
    const nested = {
      Order: { base: "Record", fields: { shipping: { type: "Address", optional: true, description: "Where to deliver." }, customer: { type: "Text", optional: false } } },
      Address: { base: "Record", fields: { postcode: { type: "Text", optional: false }, geo: { type: "Geo", optional: true } } },
      Geo: { base: "Record", fields: { lat: { type: "Decimal", optional: false } } },
    };
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<SchemaFieldsTable types={nested} provenance={undefined} onSelect={() => {}} requestedType={{ name: "Order" }}/>); });
    act(() => tree.root.findByProps({ "aria-label": "Expand shipping" }).props.onClick());
    const group = (field: string) => fieldRow(tree, field).findByProps({ className: "spec-field-cell" });
    // The chevron and the whole name share one compact cell group; the name is never a full-width button.
    const shipping = group("shipping");
    expect(shipping.children.map(c => typeof c === "string" ? c : c.props.className)).toEqual(["spec-field-chev", "spec-field-name"]);
    expect(shipping.findByProps({ className: "spec-field-name" }).children).toEqual(["shipping"]);
    expect(group("customer").children.map(c => typeof c === "string" ? c : c.props.className)).toEqual(["spec-field-mark", "spec-field-name"]);
    expect(group("postcode").findByProps({ className: "spec-field-name" }).children).toEqual(["postcode"]);
    expect(group("shipping").props.style.paddingLeft).toBe("calc(0 * var(--field-indent))");
    expect(group("postcode").props.style.paddingLeft).toBe("calc(1 * var(--field-indent))");
    expect(text(group("postcode"))).toBe("·postcode");
    act(() => tree.root.findByProps({ "aria-label": "Expand geo" }).props.onClick());
    expect(group("lat").props.style.paddingLeft).toBe("calc(2 * var(--field-indent))");
    act(() => tree.unmount());
  });

  it("styles field names so short words stay whole and the chevron never takes the name's width", () => {
    const css = readFileSync(new URL("./spec-reader.css", import.meta.url), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    const rule = (selector: string) => css.split("}").filter(block => block.split("{").at(-2)?.split(",").map(s => s.trim()).includes(`.wes-terminal ${selector}`)).join(" ");
    const name = rule(".spec-field-name");
    expect(name).toContain("word-break: normal"); expect(name).toContain("overflow-wrap: break-word"); expect(name).toContain("min-width: 0");
    expect(name).not.toMatch(/width:\s*100%|anywhere/);
    expect(rule(".spec-field-chev")).toContain("flex: 0 0 var(--field-chev)");
    expect(rule(".spec-field-cell")).toContain("gap: 6px");
    // An opened row wraps its closed one-line cells, so a long summary or identifier reads whole.
    expect(rule(".spec-t-open > .spec-t-row > .spec-t-td")).toMatch(/white-space: normal;\s*overflow-wrap: anywhere/);
    // Narrow panes show the operation cell's copy of a row's problem status as its own line.
    expect(rule(".spec-reader span.spec-row-narrow")).toContain("display: block");
    // Fonts follow the theme tokens, never a hardcoded family.
    expect(css).not.toMatch(/PT Mono|IBM Plex|font-family:\s*['"]/);
  });

  it("bounds recursive schema exploration and exposes a reference instead", () => {
    let tree!: ReactTestRenderer;
    act(() => { tree = create(<SchemaFieldsTable types={{ Loop: { base: "Record", fields: { next: { type: "Loop", optional: true } } } }} provenance={undefined} onSelect={() => {}} requestedType={{name:"Loop"}}/>); });
    expect(tree.root.findAllByProps({ "aria-label": "Expand next" })).toHaveLength(0);
    expect(tree.root.findAllByProps({ className: "spec-type-link" })).toHaveLength(1);
    act(() => tree.unmount());
  });
});

it("renders Markdown as inert prose and code, without unsafe links or resource fetches", () => {
  let tree!: ReactTestRenderer;
  act(() => { tree = create(<SpecDocumentation text={'# Notes\n\n**Bold** and `code`.\n\n- First\n- Second\n\n```json\n{"x":"<script>"}\n```\n\n<script>alert(1)</script> [bad](javascript:alert) [docs](https://example.invalid/guide)'}/>); });
  expect(tree.root.findAllByType("script")).toHaveLength(0);
  expect(tree.root.findAllByType("a").map(a => a.props.href)).toEqual(["https://example.invalid/guide"]);
  expect(tree.root.findAllByType("pre")).toHaveLength(1);
  expect(tree.root.findAllByType("li")).toHaveLength(2);
  expect(tree.root.findAllByType("strong")).toHaveLength(1);
  act(() => tree.unmount());
});
