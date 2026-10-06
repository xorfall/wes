vi.mock("./workspace-events", () => ({ WorkspaceEvents: function(path: string) { return new EventSource(path); } }));
import { afterEach, expect, it, vi } from "vitest";
import { Engine } from "./engine";
import type { Event } from "./protocol";
import { documentCommand } from "./surface/document-command";
import { planned, repeating } from "./cells";
import { readSession } from "./surface/session-model";
import { emptyWorkspace } from "./workspace";
import { lineText } from "./surface/MonoLine";

class Events {
  static current: Events;
  onmessage?: (event: { data: string }) => void;
  onerror?: () => void;
  constructor() { Events.current = this; }
  close() {}
  emit(event: Event) { this.onmessage?.({ data: JSON.stringify(event) }); }
}
afterEach(() => vi.unstubAllGlobals());
function fixture() {
  vi.stubGlobal("EventSource", Events);
  const fetch = vi.fn().mockResolvedValue({ ok: true });
  vi.stubGlobal("window", { fetch });
  const engine = new Engine(); engine.listen(() => {}, () => {});
  Events.current.emit({ event: "session", workspace: null, generation: "g" });
  return { engine, fetch };
}

it("sends one compact operation with exact raw multiline document, then waits for the authoritative outcome", async () => {
  const { engine, fetch } = fixture();
  const source = 'package: "Türkçe"\r\ntypes:\n  TextPath: {base: Text}\n# C:\\folder\\file';
  const text = documentCommand("types", "editor:synthetic", "", "plan1");
  const pending = engine.submitDocument("cell", text, source, "g", { selected: "DEV", revisions: { DEV: "rev" } });
  expect(fetch).toHaveBeenCalledOnce();
  expect(JSON.parse(fetch.mock.calls[0]![1].body)).toMatchObject({ request: "submit", client: engine.client, cell: "cell", text, document: { source }, environments: { selected: "DEV" } });
  Events.current.emit({ event: "planned", cell: "cell", text, nodes: [], diagnostics: [] });
  await expect(pending).resolves.toMatchObject({ cell: "cell" });
  await engine.retrySubmission("cell");
  expect(fetch.mock.calls[1]![1].body).toBe(fetch.mock.calls[0]![1].body);
  await expect(engine.submitDocument("cell", text, "different", "g")).rejects.toThrow("different document");
});

it("reports semantic diagnostics even when transport and preparation return successfully", async () => {
  const { engine } = fixture();
  const pending = engine.submitDocument("cell", ':package load source:""', "types: {}", "g");
  Events.current.emit({ event: "planned", cell: "cell", text: "", nodes: [], diagnostics: [
    { code: "TYPE", severity: "error", message: "duplicate type", start: 0, end: 0, hints: [] },
  ] });
  await expect(pending).rejects.toThrow("TYPE: duplicate type");
});

it.each(["workspace", "disconnect"])("refuses stale outcomes on %s without automatically retrying", async kind => {
  const { engine, fetch } = fixture();
  const pending = engine.submitDocument("cell", ':package load source:""', "types: {}", "g");
  if (kind === "workspace") Events.current.emit({ event: "session", workspace: null, generation: "other" });
  else Events.current.onerror?.();
  await expect(pending).rejects.toThrow(/changed|lost/);
  expect(fetch).toHaveBeenCalledOnce();
});

it("never sends a document to a replacement workspace", async () => {
  const { engine, fetch } = fixture();
  await expect(engine.submitDocument("cell", ':package load source:""', "types: {}", "old")).rejects.toThrow("Workspace changed");
  expect(fetch).not.toHaveBeenCalled();
});

it("uses shared type loading for types and explicit base plus separate apply for environment plans", () => {
  expect(documentCommand("types", "editor:x", "", "plan1")).toBe(':package load source:"" origin:"editor:x"');
  expect(documentCommand("env", "editor:x", '/synthetic/"quoted"', "plan1"))
    .toBe(':env plan source:"" origin:"editor:x" base:"/synthetic/\\"quoted\\"" > plan1');
});

it("repeats the captured raw document and rejects changes under the same attempt", async () => {
  const { engine, fetch } = fixture();
  const text = documentCommand("types", "editor:x", "", "plan1");
  const document = { source: "types:\n  Local: {base: Text}" };
  await engine.rerun("repeat", text, "original", false, undefined, document);
  expect(JSON.parse(fetch.mock.calls[0]![1].body)).toMatchObject({ repeat: "original", document });
  await expect(engine.rerun("repeat", text, "original", false, undefined, { source: "changed" })).rejects.toThrow("different repeat intent");
});

it("restores exact document inspection and repeat data without showing empty transport placeholders", () => {
  const document = { source: "types:\n  Şehir: {base: Text}" };
  const cells = planned([], { event: "planned", cell: "original", text: ':package load source:"" origin:"editor:x"', nodes: [], document });
  expect(cells[0]!.document).toEqual(document);
  expect(repeating(cells[0]!, false).document).toEqual(document);
  const model = readSession({ workspace: emptyWorkspace, cells, context: { workspace: "synthetic", connection: "connected" } }, new Date());
  const shown = model.cells[0]!.rows.map((row) => lineText(row.segments)).join("\n");
  expect(shown).toContain(":package load");
  expect(shown).not.toContain("origin:");
  expect(model.cells[0]!.source).toBe(document.source);
});

it("decodes captured editor context safely and keeps environment plan bindings visible", async () => {
  const { documentOperation, documentLabel } = await import("./surface/document-command");
  const origin = 'editor:Türkçe"\\\n\r\t';
  const base = '/synthetic/with "quote"/C:\\folder';
  const text = documentCommand("env", origin, base, "editorPlan42");
  expect(documentOperation(text)).toEqual({ context: "env", origin, base, planName: "editorPlan42" });
  expect(documentLabel(text)).toBe(":env plan > editorPlan42");
  expect(documentOperation(documentCommand("types", "editor:types", "", "unused"))).toEqual({ context: "types", origin: "editor:types", base: "" });
  expect(documentOperation(':env plan source:"" origin:"bad\\q" > plan')).toBeUndefined();
  expect(documentOperation(text + '\n:env apply $editorPlan42')).toBeUndefined();
  expect(documentLabel(':env plan source:"" > externalPlan')).toBe(':env plan source:"" > externalPlan');
});

it("retains backend plan diagnostics in cell projection and marks restored previews as historical", () => {
  const text = documentCommand("env", "editor:x", "/synthetic", "editorPlan42");
  const event = { event: "planned" as const, cell: "env-plan", text, nodes: [], document: { source: "version: 1" },
    diagnostics: [{ code: "ENV000", severity: "info" as const, message: "DEV: added synthetic", start: 0, end: 0, hints: [] }] };
  const project = (restored: boolean) => readSession({ workspace: emptyWorkspace, cells: planned([], { ...event, restored }),
    context: { workspace: "synthetic", connection: "connected" } }, new Date()).cells[0]!;
  expect(project(false).rows.map((row) => lineText(row.segments)).join("\n")).toContain("editorPlan42");
  expect(project(false).documentNotice).toContain("DEV: added synthetic");
  expect(project(false).documentNotice).toContain(":env apply $editorPlan42");
  expect(project(false).documentNotice).toContain(":env discard $editorPlan42");
  expect(project(true).documentNotice).toContain("DEV: added synthetic");
  expect(project(true).documentNotice).toContain("plan again before applying");
  const failed = readSession({ workspace: emptyWorkspace, cells: planned([], { ...event, diagnostics: [{ code: "ENV009", severity: "error", message: "invalid recipe", start: 0, end: 0, hints: [] }] }),
    context: { workspace: "synthetic", connection: "connected" } }, new Date()).cells[0]!;
  expect(failed.documentNotice).not.toContain(":env apply");
});
