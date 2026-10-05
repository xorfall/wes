import { afterEach, expect, it, vi } from "vitest";
import { useState } from "react";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { SpecPackage } from "../api-library";
import type { DraftSummary } from "../draft-api";
import { SpecLibrary } from "./SpecLibrary";
import { READ, READING, type HomeRead } from "./spec-home";

let tree: ReactTestRenderer;
afterEach(() => { if (tree) act(() => tree.unmount()); });
const textOf = (node: ReactTestInstance | string): string => typeof node === "string" ? node : node.children.map(textOf).join("");
const key = { service: "inventory", apiVersion: "v1", scope: "public" };
const draft = (revision: string, valid = false, descriptorRevision: string | null = null): DraftSummary => ({
  key, revision, valid, accepted: false, origin: `https://docs.synthetic.invalid/${revision}`, sourceDigest: null, descriptorRevision,
});
const saved = (revision: string, accepted = false): SpecPackage => ({ key, revision, accepted, origin: "/synthetic/saved.json" });

function Library({ drafts, packages, read = READ, busy = false, onDraft = vi.fn(), onPackage = vi.fn(), onRefresh = vi.fn() }: {
  drafts: DraftSummary[]; packages: SpecPackage[]; read?: HomeRead; busy?: boolean;
  onDraft?: (d: DraftSummary) => void; onPackage?: (p: SpecPackage) => void; onRefresh?: () => void;
}) {
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const onToggle = (id: string) => setOpen(old => { const next = new Set(old); if (!next.delete(id)) next.add(id); return next; });
  return <SpecLibrary drafts={drafts} packages={packages} read={read} busy={busy} expansion={{ open, onToggle }} onDraft={onDraft} onPackage={onPackage} onRefresh={onRefresh} />;
}
const render = (props: Parameters<typeof Library>[0]) => act(() => { tree = create(<Library {...props} />); });
const byLabel = (label: string) => tree.root.findByProps({ "aria-label": label });
const click = (label: string) => act(() => byLabel(label).props.onClick());
const rows = (table: ReactTestInstance) => table.findAll(n => n.props.role === "row" && n.findAll(c => c.props.role === "cell").length > 1, { deep: true });
const cells = (row: ReactTestInstance) => row.findAll(n => n.props.role === "cell").map(textOf);

it("should_GroupByFullKeyInATable_When_VersionsAndScopesDiffer", () => {
  const otherVersion = { ...draft("v2"), key: { ...key, apiVersion: "v2" } };
  const otherScope = { ...draft("private"), key: { ...key, scope: "private" } };
  render({ drafts: [draft("a"), otherVersion, otherScope], packages: [] });
  const table = byLabel("Library APIs");
  expect(table.props.role).toBe("table");
  expect(table.findAll(n => n.props.role === "columnheader").map(textOf)).toEqual(["", "api", "kind", "rev", "status", "source", ""]);
  // Real versions stay visible; the scope joins the chip only where service and version alone are ambiguous.
  expect(rows(table).map(r => cells(r)[1])).toEqual([
    "inventoryversion v1 · public" + "draft r1", "inventoryversion v2" + "draft r1", "inventoryversion v1 · private" + "draft r1",
  ]);
  expect(textOf(tree.root.findByType("header"))).toContain("Library·3");
});

it("should_OpenTheLatestAndKeepEarlierRevisions_When_AGroupIsExpanded", () => {
  const old = draft("old", true), latest = draft("latest"), api = saved("saved");
  const onDraft = vi.fn(), onPackage = vi.fn();
  render({ drafts: [old, latest], packages: [api], onDraft, onPackage });
  expect(cells(rows(byLabel("Library APIs"))[0]!).slice(2, 5)).toEqual(["draft", "r2", "needs attention"]);
  click("Open inventory draft r2");
  expect(onDraft).toHaveBeenLastCalledWith(latest);
  click("Expand inventory version v1");
  expect(byLabel("Collapse inventory version v1").props["aria-expanded"]).toBe(true);
  expect(onDraft).toHaveBeenCalledOnce(); // expanding never opens an editor
  const detail = tree.root.findByProps({ className: "spec-home-detail" });
  expect(detail.props).toMatchObject({ role: "cell", "aria-colspan": 7 }); // the detail spans the whole table row
  expect(textOf(detail)).toContain(`source${latest.origin}`);
  expect(textOf(detail)).toContain("identityversion v1 · scope public");
  expect(textOf(detail)).toContain("Open the draft to see what needs fixing.");
  const earlier = detail.findByProps({ className: "spec-home-link" });
  expect(textOf(earlier)).toBe("▸ 2 earlier revisions");
  act(() => earlier.props.onClick());
  const history = byLabel("Earlier revisions of inventory version v1");
  expect(rows(history).map(r => cells(r).slice(0, 4))).toEqual([["r1", "draft", "valid · needs preparation", "—"],["r1", "saved API", "ready to import", "—"]]);
  expect(textOf(history)).toContain(`from ${old.origin}`); // a differing earlier source stays visible
  click("Open inventory draft r1");
  expect(onDraft).toHaveBeenLastCalledWith(old);
  click("Open inventory API r1");
  expect(onPackage).toHaveBeenCalledExactlyOnceWith(api);
  click("Collapse inventory version v1");
  expect(tree.root.findAllByProps({ className: "spec-home-detail" })).toHaveLength(0);
});

it("should_DeriveReadinessFromValidityAndPreparationOnly_When_ReviewIsRecorded", () => {
  const ready = { ...draft("ready", true, "prepared"), accepted: true };
  const prepare = { ...draft("prepare", true), key: { ...key, service: "billing" } };
  const broken = { ...draft("broken"), key: { ...key, service: "orders" }, accepted: true };
  const api = { ...saved("api"), key: { ...key, service: "catalog" } };
  render({ drafts: [ready, prepare, broken], packages: [api] });
  const status = rows(byLabel("Library APIs")).map(r => r.findAll(n => n.props.className === "spec-home-td spec-home-c-status")[0]!);
  expect(status.map(textOf)).toEqual(["ready to import · reviewed", "valid · needs preparation", "needs attention · reviewed", "ready to import"]);
  expect(status.map(s => s.findAllByType("span")[0]!.props.className)).toEqual(["mono-ok", "mono-warn", "mono-bad", "mono-ok"]);
  expect(textOf(tree.root.findByType("header"))).toContain("1 needs attention");
  expect(textOf(tree.root.findByProps({ className: "spec-home-key spec-home-wide" }))).toContain("review is optional and never blocks");
  // No operation or problem counts are invented for a closed row.
  expect(JSON.stringify(tree.toJSON())).not.toMatch(/operations?|problems?/);
});

it("should_HideAMaterializedDuplicate_When_ADraftOwnsTheDescriptor", () => {
  const materialized = saved("materialized"), later = saved("later");
  const onPackage = vi.fn();
  render({ drafts: [draft("draft", true, materialized.revision)], packages: [materialized, later], onPackage });
  expect(tree.root.findAllByProps({ "aria-label": "Open inventory API r1" })).toHaveLength(0);
  click("Expand inventory version v1");
  act(() => tree.root.findByProps({ className: "spec-home-link" }).props.onClick());
  click("Open inventory API r2");
  expect(onPackage).toHaveBeenCalledExactlyOnceWith(later);
});

it("should_ShowTheReadableSourceAndNoStorageIdentity_When_TheApiWasDescribed", () => {
  const url = "https://docs.synthetic.invalid/guide/";
  const described = (revision: string, origin: string): DraftSummary => ({
    ...draft(revision, true), key: { service: "placeholder", apiVersion: "describe", scope: "7".repeat(64) }, origin,
  });
  const oldUrl = "https://docs.synthetic.invalid/v0/";
  render({ drafts: [described("old", `described:${oldUrl}; source:${oldUrl}openapi.yaml`), described("a", `described:${url}; source:${url}openapi.json`)], packages: [] });
  const row = rows(byLabel("Library APIs"))[0]!;
  expect(cells(row)[1]).toBe("placeholderdraft r2");
  expect(cells(row)[5]).toBe("…/guide/");
  expect(row.findAll(n => n.props["aria-description"] === url)).toHaveLength(1); // the whole source is the hint
  click("Expand placeholder");
  const text = textOf(tree.root);
  expect(text).not.toContain("7".repeat(64));
  expect(text).not.toContain("described:");
  expect(text).not.toContain("identity");
  expect(textOf(tree.root.findByProps({ className: "spec-home-detail" }))).toContain(`${url} · via ${url}openapi.json`);
  // A differing earlier source keeps its discovery page too, not only the described location.
  act(() => tree.root.findByProps({ className: "spec-home-link" }).props.onClick());
  const earlier = rows(byLabel("Earlier revisions of placeholder"))[0]!;
  expect(cells(earlier)[4]).toBe(`openfrom ${oldUrl} · via ${oldUrl}openapi.yaml`);
  expect(textOf(tree.root)).not.toContain("described:");
});

it("should_KeepLoadingFailureAndEmptyDistinct_When_TheLibraryIsRead", () => {
  const onRefresh = vi.fn();
  render({ drafts: [], packages: [], read: READING, onRefresh });
  expect(textOf(tree.root)).toContain("Reading the library…");
  expect(textOf(tree.root)).toContain("Library·—");
  act(() => tree.update(<Library drafts={[]} packages={[]} read={{ state: "failed", message: "Synthetic read failure" }} onRefresh={onRefresh} />));
  expect(textOf(tree.root.findByProps({ role: "alert" }))).toContain("Couldn’t read the library.");
  expect(textOf(tree.root)).toContain("Synthetic read failure");
  expect(textOf(tree.root)).not.toContain("No APIs yet");
  act(() => tree.root.findAllByType("button").find(b => textOf(b) === "try again")!.props.onClick());
  expect(onRefresh).toHaveBeenCalledOnce();
  act(() => tree.update(<Library drafts={[]} packages={[]} onRefresh={onRefresh} />));
  expect(textOf(tree.root)).toContain("No APIs yet. Import OpenAPI below to create an editable draft.");
  expect(textOf(tree.root)).toContain("Library·0");
  click("Refresh library");
  expect(onRefresh).toHaveBeenCalledTimes(2);
});

it("should_DisableEveryOpenAndRefresh_When_Busy", () => {
  render({ drafts: [draft("a", true, "p")], packages: [], busy: true });
  expect(byLabel("Open inventory draft r1").props.disabled).toBe(true);
  expect(byLabel("Refresh library").props.disabled).toBe(true);
  expect(byLabel("Expand inventory version v1").props.disabled).toBeUndefined(); // reading is never blocked
});
