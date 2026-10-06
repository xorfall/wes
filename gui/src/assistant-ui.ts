import { documentRenderStatus, parseRenderScope, type RenderScope, type RenderStatusReader } from "./view-render-status";
import { workspaceName } from "./workspace-binding";

/**
 * Typed terminal UI broker operations, scoped to one live UI. Never synthetic key events or a text
 * tunnel: every request is an exact discriminated record, decoded here before any effect.
 */
export type UiOperation =
  | { readonly kind: "draft_read" }
  | { readonly kind: "draft_update"; readonly text: string; readonly revision: string }
  | { readonly kind: "layout_read" }
  | { readonly kind: "tab_open"; readonly workspace: string; readonly pane: string; readonly activate: boolean }
  | { readonly kind: "view_render_status"; readonly scope: RenderScope };
export interface UiRequest { readonly id: string; readonly operation: UiOperation }
type UiKind = UiOperation["kind"];
type Operation<K extends UiKind> = Extract<UiOperation, { kind: K }>;

export interface EditorSnapshot { text: string; revision: string }
export interface EditorTarget { read(): EditorSnapshot; update(text: string, revision: string): EditorSnapshot }
export interface WorkspaceLayoutTarget {
  read(): unknown;
  open(request: { workspace: string; pane: string; activate: boolean }): Promise<unknown>;
}

const MAX_DRAFT_BYTES = 64 * 1024;
const MAX_TOKEN_BYTES = 256;
const MAX_REPLIES = 256;
const INVALID_REQUEST = "Invalid UI request.";
const DRAFT_TOO_LARGE = "Draft exceeds 64 KiB.";

const utf8Length = (text: string) => new TextEncoder().encode(text).length;
const token = (value: unknown): value is string =>
  typeof value === "string" && value.length > 0 && utf8Length(value) <= MAX_TOKEN_BYTES && !/[\u0000-\u001f\u007f-\u009f]/u.test(value);
/** An object with exactly the named fields; extra, missing or inherited fields are refused. */
function exactRecord(value: unknown, keys: readonly string[]): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(INVALID_REQUEST);
  const fields = value as Record<string, unknown>, actual = Object.keys(fields).sort(), expected = [...keys].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) throw new Error(INVALID_REQUEST);
  return fields;
}

/** One decoder per operation kind. Decoders build fields in a fixed order, so decoded requests serialize canonically. */
const OPERATIONS: { readonly [K in UiKind]: { readonly fields: readonly string[]; decode(fields: Record<string, unknown>): Operation<K> } } = {
  draft_read: { fields: [], decode: () => ({ kind: "draft_read" }) },
  draft_update: { fields: ["text", "revision"], decode: ({ text, revision }) => {
    if (typeof text !== "string" || !token(revision)) throw new Error(INVALID_REQUEST);
    if (utf8Length(text) > MAX_DRAFT_BYTES) throw new Error(DRAFT_TOO_LARGE);
    return { kind: "draft_update", text, revision };
  } },
  layout_read: { fields: [], decode: () => ({ kind: "layout_read" }) },
  tab_open: { fields: ["workspace", "pane", "activate"], decode: ({ workspace, pane, activate }) => {
    if (!workspaceName(workspace) || !token(pane) || typeof activate !== "boolean") throw new Error(INVALID_REQUEST);
    return { kind: "tab_open", workspace, pane, activate };
  } },
  view_render_status: { fields: ["scope"], decode: ({ scope }) => ({ kind: "view_render_status", scope: parseRenderScope(scope) }) },
};

/** Decodes the broker wire contract exactly; no legacy action/text/revision packets are accepted. */
export function decodeUiRequest(input: unknown): UiRequest {
  const { id, operation } = exactRecord(input, ["id", "operation"]);
  if (!token(id)) throw new Error(INVALID_REQUEST);
  const kind = operation && typeof operation === "object" ? (operation as { kind?: unknown }).kind : undefined;
  if (typeof kind !== "string" || !Object.hasOwn(OPERATIONS, kind)) throw new Error(INVALID_REQUEST);
  const decoder = OPERATIONS[kind as UiKind];
  return { id, operation: decoder.decode(exactRecord(operation, ["kind", ...decoder.fields])) };
}

const failure = (error: unknown) => ({ ok: false, error: error instanceof Error ? error.message : "UI request failed." });

export class AssistantUi {
  constructor(private readonly renderStatus: RenderStatusReader = documentRenderStatus) {}
  private layout?: WorkspaceLayoutTarget;
  private target?: EditorTarget;
  private replies = new Map<string, { request: string; result: unknown }>();
  attachLayout(target: WorkspaceLayoutTarget): () => void {
    this.layout = target;
    return () => { if (this.layout === target) this.layout = undefined; };
  }
  attach(target: EditorTarget): () => void {
    this.target = target;
    return () => { if (this.target === target) this.target = undefined; };
  }
  reset(): void { this.replies.clear(); }
  /**
   * Applies a broker request at most once per id. A repeated id with identical input returns the
   * recorded reply (including a pending tab open); malformed input fails before any effect.
   */
  handle(input: UiRequest): unknown {
    let request: UiRequest;
    try { request = decodeUiRequest(input); } catch (error) { return failure(error); }
    const serialized = JSON.stringify(request);
    const saved = this.replies.get(request.id);
    if (saved) return saved.request === serialized ? saved.result : { ok: false, error: "UI request ID was reused with different input." };
    if (this.replies.size >= MAX_REPLIES) return { ok: false, error: "UI request limit reached; reopen the workspace connection." };
    let result: unknown;
    try { result = this.perform(request.operation); } catch (error) { result = failure(error); }
    // Stop instead of evicting: old polling replies must never apply a write twice.
    this.replies.set(request.id, { request: serialized, result });
    return result;
  }
  private readonly handlers: { readonly [K in UiKind]: (operation: Operation<K>) => unknown } = {
    draft_read: () => {
      const snapshot = this.editor().read();
      if (utf8Length(snapshot.text) > MAX_DRAFT_BYTES) throw new Error(DRAFT_TOO_LARGE);
      return { ok: true, ...snapshot };
    },
    draft_update: ({ text, revision }) => ({ ok: true, ...this.editor().update(text, revision) }),
    layout_read: () => ({ ok: true, layout: this.workspaceLayout().read() }),
    tab_open: ({ workspace, pane, activate }) => this.workspaceLayout().open({ workspace, pane, activate }).then(layout => ({ ok: true, layout }), failure),
    // Read-only: receipts of canvases already mounted here. Empty hosts means unverified, not drawn.
    view_render_status: ({ scope }) => ({ ok: true, ...scope, hosts: this.renderStatus.receipts(scope) }),
  };
  private perform<K extends UiKind>(operation: Operation<K>): unknown {
    return (this.handlers[operation.kind] as (operation: Operation<K>) => unknown)(operation);
  }
  private editor(): EditorTarget {
    if (!this.target) throw new Error("Open the Workspace command editor before requesting draft access.");
    return this.target;
  }
  private workspaceLayout(): WorkspaceLayoutTarget {
    if (!this.layout) throw new Error("The workspace layout is not connected.");
    return this.layout;
  }
}

/** Revisions change synchronously with keystrokes, before React renders or network replies. */
export class CommandDraft {
  private text = "";
  private revision: string = crypto.randomUUID();
  private scope = "";
  setScope(scope: string): void { if (this.scope !== scope) { this.scope = scope; this.revision = crypto.randomUUID(); } }
  read(): EditorSnapshot { return { text: this.text, revision: this.revision }; }
  write(text: string): void { this.text = text; this.revision = crypto.randomUUID(); }
  replace(text: string, revision: string): EditorSnapshot {
    if (revision !== this.revision) throw new Error("Draft changed; read it again before writing.");
    this.write(text);
    return this.read();
  }
}
