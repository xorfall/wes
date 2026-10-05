/** Terminal MCP editor requests are scoped to one live UI, never synthetic key events. */
export interface EditorRequest { id: string; action: string; text: string | null; revision: string | null }
export interface EditorSnapshot { text: string; revision: string }
export interface EditorTarget { read(): EditorSnapshot; update(text: string, revision: string): EditorSnapshot }
export interface WorkspaceLayoutTarget {
  read(): unknown;
  open(request: { workspace: string; pane: string; activate: boolean }): Promise<unknown>;
}
export class AssistantEditor {
  private layout?: WorkspaceLayoutTarget;
  attachLayout(target: WorkspaceLayoutTarget): () => void {
    this.layout = target;
    return () => { if (this.layout === target) this.layout = undefined; };
  }
  private target?: EditorTarget;
  private replies = new Map<string, { request: string; result: unknown }>();
  attach(target: EditorTarget): () => void {
    this.target = target;
    return () => { if (this.target === target) this.target = undefined; };
  }
  reset(): void { this.replies.clear(); }
  handle(request: EditorRequest): unknown {
    const serialized = JSON.stringify(request);
    const saved = this.replies.get(request.id);
    if (saved) return saved.request === serialized ? saved.result : { ok: false, error: "Editor request ID was reused with different input." };
    if (this.replies.size >= 256) return { ok: false, error: "Editor request limit reached; reopen the workspace connection." };
    let result: unknown;
    try {
      if (request.action === "layout") {
        if (!this.layout) throw new Error("The workspace layout is not connected.");
        result = { ok: true, layout: this.layout.read() };
      } else if (request.action === "tab") {
        if (!this.layout) throw new Error("The workspace layout is not connected.");
        const input = JSON.parse(request.text ?? "null");
        if (!input || typeof input.workspace !== "string" || typeof input.pane !== "string" || typeof input.activate !== "boolean") throw new Error("Invalid workspace tab request.");
        result = this.layout.open(input).then(layout => ({ ok: true, layout }), error => ({ ok: false, error: error instanceof Error ? error.message : String(error) }));
      } else {
        if (!this.target) throw new Error("Open the Workspace command editor before requesting draft access.");
        if (request.action === "read") {
          const snapshot = this.target.read();
          if (new TextEncoder().encode(snapshot.text).length > 64 * 1024) throw new Error("Draft exceeds 64 KiB.");
          result = { ok: true, ...snapshot };
        }
        else if (request.action === "update" && typeof request.text === "string" && typeof request.revision === "string") {
          if (new TextEncoder().encode(request.text).length > 64 * 1024) throw new Error("Draft exceeds 64 KiB.");
          result = { ok: true, ...this.target.update(request.text, request.revision) };
        } else throw new Error("Invalid editor request.");
      }
    } catch (error) { result = { ok: false, error: error instanceof Error ? error.message : "Editor request failed." }; }
    // Stop instead of evicting: old polling replies must never apply a write twice.
    this.replies.set(request.id, { request: serialized, result });
    return result;
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
