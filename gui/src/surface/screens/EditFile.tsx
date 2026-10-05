/**
 * Named YAML buffer. Saving and submitting its current text are independent actions.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { leaving, Screen } from "./../Screen";
import { MonoLine, type Segment } from "./../MonoLine";
import type { TypeVocabulary } from "./../yaml-highlight";
import { loadYamlSchema } from "./../yaml-schema-loader";
import type { YamlSchema } from "./../yaml-schema";
import { yamlSyntaxProblem } from "./../yaml-syntax";
import type { EnvironmentContext } from "../../protocol";
import type { EnvironmentDocument } from "../../engine";
import { primaryGlyph } from "../../platform-keys";
import "./../surface.css";
import "./../editor.css";

export type EditFileContext = "env" | "types";

export interface EditFileProps {
  readonly top: readonly Segment[];
  readonly context: EditFileContext;
  readonly vocabulary: TypeVocabulary;
  readonly initialName?: string;
  readonly initialSource?: string;
  readonly initialBase?: string;
  readonly initialOrigin?: string;
  readonly onClose: () => void;
  readonly generation?: string;
  readonly environmentContext?: EnvironmentContext;
  readonly onRun?: (source: string, options: { name: string; base: string; origin: string; generation: string; environments?: EnvironmentContext }) => Promise<string>;
  readonly chrome?: "full" | "pane";
  readonly loadEnvironments?: (signal: AbortSignal) => Promise<readonly EnvironmentDocument[]>;
}

type SaveStatus = "idle" | "saving" | "saved" | "error";

function statusLine(status: SaveStatus, path: string): Segment[] {
  if (status === "saving") return [{ text: "saving…", role: "mono-dim" }];
  if (status === "saved") return [{ text: "saved to ", role: "mono-dim" }, { text: path, role: "mono-literal" }];
  if (status === "error") return [{ text: path || "could not save", role: "mono-bad" }];
  return [{ text: "not saved yet", role: "mono-faint" }];
}

export function EditFileScreen(props: EditFileProps) {
  if (props.context === "env" && props.loadEnvironments && props.initialSource === undefined && props.initialName === undefined) {
    return <EnvironmentDocuments key={props.generation} {...props} />;
  }
  return <EditFileBuffer key={JSON.stringify([props.context, props.initialName ?? ""])} {...props} />;
}

function EnvironmentDocuments(props: EditFileProps) {
  const [documents, setDocuments] = useState<readonly EnvironmentDocument[]>();
  const [problem, setProblem] = useState<string>();
  const [selected, setSelected] = useState<EnvironmentDocument | "new">();
  const [refresh, setRefresh] = useState(0);
  const loader = useRef(props.loadEnvironments);
  loader.current = props.loadEnvironments;
  useEffect(() => {
    const controller = new AbortController();
    setDocuments(undefined); setProblem(undefined);
    void loader.current!(controller.signal).then(documents => {
      if (!controller.signal.aborted) setDocuments(documents);
    }).catch(error => {
      if (!controller.signal.aborted) setProblem(error instanceof Error ? error.message : "Could not load environment documents.");
    });
    return () => controller.abort();
  }, [refresh]);
  if (selected !== undefined) {
    const initial = selected === "new" ? {} : { initialSource: selected.source, initialOrigin: selected.origin, initialName: selected.name };
    return <EditFileBuffer key={selected === "new" ? "new" : selected.origin} {...props} {...initial}
      onClose={() => { setSelected(undefined); setRefresh(value => value + 1); }} />;
  }
  return <Screen name="/edit env" top={props.top} chrome={props.chrome ?? "full"} onClose={props.onClose}
    subject={[{ text: "Environment documents", role: "mono-ink" }]}
    tools={<button type="button" className="cell-action" onClick={() => setSelected("new")}>New</button>}
    footer={leaving()}>
    <div className="environment-documents">
      {!documents && !problem && <p className="mono-dim">Loading environment documents…</p>}
      {problem && <><p className="mono-bad" role="alert">{problem}</p><button type="button" className="cell-action" onClick={() => setRefresh(value => value + 1)}>Retry</button></>}
      {documents?.length === 0 && <p className="mono-dim">No environment documents yet. Choose New to create one.</p>}
      {documents?.map(document => <button type="button" className="environment-document" key={document.name} onClick={() => setSelected(document)}>
        <span className="mono-ink">{document.name}</span>
        <span className="mono-dim">{document.environments.join(", ")}</span>
      </button>)}
    </div>
  </Screen>;
}

function EditFileBuffer({
  top, context, vocabulary, initialName = "", initialSource = "", initialBase = "", initialOrigin, onClose, chrome = "full", generation, environmentContext, onRun,
}: EditFileProps) {
  const card = useRef<HTMLDivElement>(null);
  const [name, setName] = useState(initialName);
  const [written, setWritten] = useState(initialSource);
  const [status, setStatus] = useState<SaveStatus>("idle");
  const [message, setMessage] = useState("");
  const [saving, setSaving] = useState(false);
  const [base, setBase] = useState(initialBase);
  const [runState, setRunState] = useState<{ busy: boolean; error?: boolean; message: string }>({ busy: false, message: "" });
  const running = useRef(false);
  const mounted = useRef(true);
  const openedGeneration = useRef(generation);
  const openedEnvironments = useRef(environmentContext);
  const [origin] = useState(() => initialOrigin ?? `editor:${crypto.randomUUID()}`);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const [schemaStatus, setSchemaStatus] = useState<"loading" | "ready" | "unavailable">("loading");
  const pending = useRef(false);
  const revision = useRef(0);
  const latest = useRef({ name, source: written });
  latest.current = { name, source: written };

  const changed = (next: Partial<{ name: string; source: string }>) => {
    revision.current += 1;
    latest.current = { ...latest.current, ...next };
    if (next.name !== undefined) setName(next.name);
    if (next.source !== undefined) setWritten(next.source);
    setStatus("idle"); setMessage("");
    setRunState(was => ({ busy: was.busy, message: "" }));
  };

  const save = useCallback(async () => {
    if (pending.current) return;
    const current = latest.current;
    const savedRevision = revision.current;
    if (current.name.trim() === "") {
      setStatus("error");
      setMessage("name it first");
      return;
    }
    pending.current = true; setSaving(true);
    setStatus("saving");
    try {
      const response = await fetch("/edit-files", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ context, name: current.name.trim(), content: current.source }),
      });
      if (!response.ok) {
        const error = (await response.text()) || `save failed (${response.status})`;
        if (savedRevision !== revision.current) return;
        setStatus("error");
        setMessage(error);
        return;
      }
      const body = (await response.json()) as { path?: string };
      if (savedRevision !== revision.current) return;
      setStatus("saved");
      setMessage(body.path ?? "");
    } catch {
      if (savedRevision !== revision.current) return;
      setStatus("error");
      setMessage("could not reach the engine");
    } finally {
      pending.current = false; setSaving(false);
    }
  }, [context]);

  const run = useCallback(async () => {
    if (running.current || !onRun) return;
    if (!generation || openedGeneration.current !== generation) {
      setRunState({ busy: false, error: true, message: "Workspace changed or disconnected. Keep your text and reopen the editor in the intended workspace before submitting." });
      return;
    }
    const current = latest.current;
    const problem = yamlSyntaxProblem(current.source);
    if (problem) { setRunState({ busy: false, error: true, message: problem }); return; }
    const submittedRevision = revision.current;
    running.current = true;
    setRunState({ busy: true, message: context === "env" ? "planning…" : "loading…" });
    try {
      const message = await onRun(current.source, { name: current.name.trim(), base: base.trim(), origin, generation, environments: openedEnvironments.current });
      if (mounted.current && submittedRevision === revision.current) setRunState({ busy: false, message });
    } catch (error) {
      if (mounted.current && submittedRevision === revision.current) {
        setRunState({ busy: false, error: true, message: error instanceof Error ? error.message : "Submission failed." });
      }
    } finally {
      running.current = false;
      if (mounted.current) setRunState(was => ({ ...was, busy: false }));
    }
  }, [base, context, generation, onRun, origin]);

  const callbacks = useRef({ save, run, onClose, vocabulary });
  callbacks.current = { save, run, onClose, vocabulary };

  useEffect(() => {
    const parent = card.current;
    if (!parent) return;
    let view: { destroy: () => void } | undefined;
    let applySchema: ((schema: YamlSchema) => void) | undefined;
    let schema: YamlSchema | undefined;
    let dead = false;
    const controller = new AbortController();
    void loadYamlSchema(controller.signal).then(loaded => {
      if (dead) return;
      schema = loaded;
      applySchema?.(loaded);
      setSchemaStatus("ready");
    }).catch(() => {
      if (!dead) setSchemaStatus("unavailable");
    });
    void import("./../yaml-editor").then(({ makeYamlEditor, updateYamlSchema }) => {
      if (dead || !card.current) return;
      view = makeYamlEditor(
        card.current,
        latest.current.source,
        {
          context,
          schema,
          vocabulary: { get names() { return callbacks.current.vocabulary.names; } },
          onSave: () => void callbacks.current.save(), onRun: () => void callbacks.current.run(), onLeave: () => callbacks.current.onClose(),
        },
        source => changed({ source }),
        chrome === "full",
      );
      const mounted = view as ReturnType<typeof makeYamlEditor>;
      applySchema = loaded => updateYamlSchema(mounted, loaded);
    });
    return () => { dead = true; controller.abort(); view?.destroy(); };
    // Keep editor/history for this buffer; callbacks and vocabulary read current props.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <Screen
      name="/edit"
      top={top}
      chrome={chrome}
      subject={[{ text: context, role: "mono-meta" }]}
      onClose={onClose}
      tools={
        <div className="edit-file-tools">
          <input
            className="edit-file-name"
            value={name}
            onChange={(event) => changed({ name: event.target.value })}
            placeholder="name"
            aria-label="File name"
            spellCheck={false}
          />
          <button type="button" className="cell-action" disabled={saving} onClick={() => void save()}>
            save
          </button>
          {onRun && <button type="button" className="cell-action" disabled={runState.busy} onClick={() => void run()}>
            {context === "env" ? "plan" : "load"}
          </button>}
        </div>
      }
      footer={leaving({ text: `${primaryGlyph()}S`, role: "mono-ref" }, { text: " save", role: "mono-dim" },
        ...(onRun ? [{ text: ` · ${primaryGlyph()}R`, role: "mono-ref" as const }, { text: context === "env" ? " plan" : " load", role: "mono-dim" as const }] : []))}
    >
      <div className="edit-body">
        {context === "env" && onRun && <label className="edit-file-status mono-dim">
          Base directory for local inputs
          <input className="edit-file-name" aria-label="Base directory for local inputs" value={base}
            placeholder="absolute path (if needed)" spellCheck={false}
            onChange={event => { revision.current += 1; setBase(event.target.value); setRunState(was => ({ busy: was.busy, message: "" })); }} />
        </label>}
        <div className="edit-card surface-sunk">
          <div className="edit-code" ref={card} />
        </div>
        <MonoLine segments={statusLine(status, message)} className="edit-file-status" />
        {runState.message && <MonoLine segments={[{ text: runState.message, role: runState.error ? "mono-bad" : "mono-meta" }]} className="edit-run-status" />}
        {schemaStatus !== "ready" && <MonoLine
          segments={[{ text: schemaStatus === "loading" ? "Loading completion schema…" : "Completion schema unavailable", role: "mono-faint" }]}
          className="edit-schema-status"
        />}
      </div>
    </Screen>
  );
}
