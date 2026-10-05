/**
 * One editable API draft on the /spec screen: outline, source, Problems and evidence.
 *
 * The backend decides everything that matters — syntax, semantics, readiness, evidence status. The
 * screen's own job is to never let an answer speak for text it was not computed on:
 *
 * - every validation answer is checked against the SHA-256 of the exact text that was sent, and is
 *   dropped when a newer request exists or the text has moved on since;
 * - an edit immediately turns the last answer stale: its marks go hollow, its counts say "stale", and
 *   nothing stale can approve review or import;
 * - review and import speak only for the saved revision, and only when the saved text itself was
 *   judged valid and a descriptor was materialized for it. Review is optional for import;
 * - a save whose answer arrives after further typing moves the saved baseline but never the text.
 */
import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type MutableRefObject } from "react";
import { provenanceEntriesFor, type ProvenanceEntry, type SchemaProvenance } from "../api-library";
import {
  draftApi, draftProvenance, isManual, manualTargetsOf, offsetOfPointer, evidencePointerAt, orderProblems, readPreview, validationFor,
  type DraftPreview, type DraftProblem, type DraftResult, type DraftSummary, type DraftValidation,
} from "../draft-api";
import type { Segment } from "./MonoLine";
import { SpecOperations } from "./SpecOperations";
import { SchemaFieldsTable } from "./SchemaProvenance";
import { describeLineRanges } from "./DescribeFailureDetails";
import { DraftSourceEditor, type DraftEditorHandle } from "./DraftSourceEditor";
import { SpecImportForm, type ImportHold } from "./SpecImportForm";
import "./draft.css";
import { DraftAgentAccess } from "./DraftAgentAccess";

export const DRAFT_TABS = ["operations", "schema", "source", "problems", "import"] as const;
export type DraftTab = typeof DRAFT_TABS[number];
/** Quiet time after the last keystroke before the edited text is checked. */
export const CHECK_DELAY_MS = 600;
const REVISION_PREVIEW = 12;

export interface DraftControls { dirty: boolean; save: () => Promise<boolean> }

interface Check { text: string; validation: DraftValidation }
interface Parsed { text: string; preview: DraftPreview }

export interface DraftEditorProps {
  opened: DraftSummary;
  /** Every listed revision of this draft's key, oldest first; `rN` counts through them. */
  revisions: readonly DraftSummary[];
  onSubmit: (source: string) => string | undefined | Promise<string | undefined>;
  onClose: () => void;
  onSaved: (draft: DraftSummary) => void;
  onSubject: (segments: Segment[]) => void;
  controls: MutableRefObject<DraftControls | undefined>;
  /** The workspace the screen is bound to, shown where an import would land; the environment is the session's. */
  workspace?: string | undefined;
}

export function revisionLabel(revisions: readonly DraftSummary[], revision: string): string {
  const index = revisions.findIndex(r => r.revision === revision);
  return index >= 0 ? `r${index + 1}` : revision.slice(0, REVISION_PREVIEW);
}

/** The one state a draft's status line names, most pressing first. */
export function draftState({ checking, current, valid, errors, importable }: { checking: boolean; current: boolean; valid: boolean; errors: number; importable: boolean }): Segment {
  if (checking) return { text: "checking…", role: "mono-dim" };
  if (current && errors) return { text: `${errors} ${errors === 1 ? "error" : "errors"}`, role: "mono-bad" };
  if (importable) return { text: "ready to import", role: "mono-ok" };
  if (current && valid) return { text: "valid", role: "mono-ok" };
  if (!current) return { text: "not checked", role: "mono-faint" };
  return { text: "not importable", role: "mono-dim" };
}

/** Problems of one code and severity, in their order; a message or fix they all share is said once. */
export function groupProblems(problems: readonly DraftProblem[]) {
  const groups = new Map<string, { key: string; code: string; severity: string; problems: DraftProblem[] }>();
  for (const p of problems) {
    const key = `${p.severity}:${p.code}`;
    if (!groups.has(key)) groups.set(key, { key, code: p.code, severity: p.severity, problems: [] });
    groups.get(key)!.problems.push(p);
  }
  const shared = (values: readonly string[]) => values.every(v => v === values[0]) ? values[0] || undefined : undefined;
  return [...groups.values()].map(g => ({ ...g, message: shared(g.problems.map(p => p.message)), fix: shared(g.problems.map(p => p.fix)) }));
}

type Tone = "ok" | "bad" | "warn" | "dim" | "faint";
const plural = (count: number, one: string, many: string) => `${count} ${count === 1 ? one : many}`;

export function DraftEditor({ opened, revisions, onSubmit, onClose, onSaved, onSubject, controls, workspace }: DraftEditorProps) {
  const [result, setResult] = useState<DraftResult>();
  const resultRef = useRef<DraftResult>();
  resultRef.current = result;
  const [text, setText] = useState("");
  const textRef = useRef("");
  const [check, setCheck] = useState<Check>();
  const [parsed, setParsed] = useState<Parsed>();
  const [checking, setChecking] = useState(false);
  const sequence = useRef(0);
  const timer = useRef<ReturnType<typeof setTimeout>>();
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [failed, setFailed] = useState(false);
  const [tab, setTab] = useState<DraftTab>("source");
  const [selected, setSelected] = useState<string>();
  const [evidenceTarget, setEvidenceTarget] = useState<string>();
  const [schemaType, setSchemaType] = useState<{ name: string }>();
  const [alias, setAlias] = useState("");
  const [endpoint, setEndpoint] = useState("");
  const [replace, setReplace] = useState(false);
  const [exportPath, setExportPath] = useState("");
  const editor = useRef<DraftEditorHandle>(null);
  const pendingReveal = useRef<{ target: { index?: number; from: number; to: number }; focus: boolean }>();

  const remember = (forText: string, validation: DraftValidation) => {
    const preview = readPreview(validation.preview);
    if (preview) setParsed({ text: forText, preview });
  };
  /** Takes a backend answer as the saved baseline; the text only follows when nothing newer was typed. */
  const adopt = (next: DraftResult, replaceText: boolean) => {
    setResult(next);
    resultRef.current = next;
    if (!replaceText && textRef.current !== next.text) return;
    if (replaceText) { sequence.current++; setChecking(false); clearTimeout(timer.current); }
    textRef.current = next.text;
    setText(next.text);
    if (validationFor(next.text, next.validation)) {
      // The backend just judged exactly this text; a queued check for it has nothing left to say.
      sequence.current++; clearTimeout(timer.current); setChecking(false);
      setCheck({ text: next.text, validation: next.validation }); remember(next.text, next.validation);
    }
  };

  useEffect(() => {
    let active = true;
    setBusy(true);
    void draftApi.inspect(opened.key, opened.revision).then(next => {
      if (!active) return;
      adopt(next, true);
      setAlias(readPreview(next.validation.preview)?.provider || next.draft.key.service);
      setTab(next.validation.valid ? "operations" : "source");
    }).catch((e: Error) => { if (active) { setFailed(true); setMessage(e.message); } })
      .finally(() => { if (active) setBusy(false); });
    return () => { active = false; clearTimeout(timer.current); sequence.current++; };
  }, [opened.key.service, opened.key.apiVersion, opened.key.scope, opened.revision]);

  const validate = async (forText: string) => {
    const id = ++sequence.current;
    clearTimeout(timer.current);
    setChecking(true);
    try {
      const answer = await draftApi.validate(forText);
      if (id !== sequence.current || forText !== textRef.current) return; // a newer edit or request owns the answer now
      if (!validationFor(forText, answer)) { setFailed(true); setMessage("Check ignored: it answered for other text."); return; }
      setCheck({ text: forText, validation: answer });
      remember(forText, answer);
    } catch (e) {
      if (id === sequence.current) { setFailed(true); setMessage((e as Error).message); }
    } finally {
      if (id === sequence.current) setChecking(false);
    }
  };
  const change = (next: string) => {
    if (next === textRef.current) return;
    textRef.current = next;
    setText(next);
    sequence.current++; // any answer still in flight is for older text
    setChecking(true);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => void validate(textRef.current), CHECK_DELAY_MS);
  };

  const run = async (work: () => Promise<void>) => {
    if (busy) return;
    setBusy(true); setFailed(false); setMessage("");
    try { await work(); } catch (e) { setFailed(true); setMessage((e as Error).message); } finally { setBusy(false); }
  };

  const save = async (): Promise<boolean> => {
    const base = resultRef.current;
    if (!base) return false;
    const captured = textRef.current;
    try {
      const next = await draftApi.save(base.draft.key, base.draft.revision, captured);
      adopt(next, false);
      onSaved(next.draft);
      const kept = textRef.current !== captured;
      setFailed(false);
      setMessage(kept ? "Saved the earlier text · newer edits unsaved." : next.validation.valid ? "Saved." : "Saved with problems.");
      return !kept;
    } catch (e) {
      setFailed(true);
      setMessage(`Save rejected: ${(e as Error).message} Your text is kept.`);
      return false;
    }
  };
  controls.current = { dirty: !!result && text !== result.text, save };

  /* ---------- what the screen knows ---------- */

  const dirty = !!result && text !== result.text;
  const current = check && check.text === text ? check.validation : undefined;
  const stale = !current;
  const shown = check?.validation.diagnostics ?? [];
  const currentPreview = current ? readPreview(current.preview) : undefined;
  const preview = currentPreview ?? parsed?.preview;
  const previewStale = !currentPreview && !!parsed;
  const problems = orderProblems(shown, preview);
  const errors = problems.filter(p => p.severity === "error").length;
  const warnings = problems.length - errors;
  // Unsaved text is not what describe recorded evidence about: until a save, every record is history.
  const provenance = draftProvenance(result?.evidence, dirty);
  const manualTargets = manualTargetsOf(result?.evidence);

  // Hashing the saved text once per saved result, not on every caret move.
  const savedChecked = useMemo(() => !!result && validationFor(result.text, result.validation), [result?.text, result?.validation]);
  const savedValid = !!result && savedChecked && result.validation.valid && result.draft.valid && !!result.descriptorPath;
  const needsMaterialization = !!result && savedChecked && result.validation.valid && !result.descriptorPath;
  const savedErrors = result ? result.validation.diagnostics.filter(d => d.severity === "error") : [];
  const revision = result ? revisionLabel(revisions, result.draft.revision) : "";
  // Each hold keeps its readiness wording (`reason`) and says the same thing in the import form.
  const holds: (Omit<ImportHold, "action"> & { reason: string; save?: true })[] = [];
  if (dirty) holds.push({ reason: "unsaved edit", tone: "mono-warn", title: "Import held.", detail: `Import uses saved text only; your edits after ${revision} aren’t saved.`, save: true });
  if (checking) holds.push({ reason: "checking…", tone: "mono-dim", title: "◌ Checking…", detail: "import waits for the check to finish" });
  if (result && !dirty && !savedValid) {
    if (!savedChecked) holds.push({ reason: "saved result does not match the saved text", tone: "mono-bad", title: "Import held.", detail: "The saved result does not match the saved text." });
    else if (result.validation.preview === null && !result.validation.valid) holds.push({ reason: "does not parse", tone: "mono-bad", title: `Saved ${revision} does not parse`, detail: "Fix the source and save it again." });
    else if (savedErrors.length) {
      const reason = `${savedErrors.length} ${savedErrors.length === 1 ? "error blocks" : "errors block"} import: ${orderProblems(savedErrors, readPreview(result.validation.preview)).map(p => [p.operation, p.field].filter(Boolean).join(" ")).join(", ")}`;
      holds.push({ reason, tone: "mono-bad", title: `Saved ${revision} isn’t valid · ${savedErrors.length} blocking`, detail: reason });
    } else if (needsMaterialization) holds.push({ reason: "save a revision to prepare this newly valid draft", tone: "mono-warn", title: "Valid · not prepared for import", detail: "save a revision to prepare this newly valid draft", save: true });
    else holds.push({ reason: "no descriptor for this revision", tone: "mono-warn", title: "Import held.", detail: "No descriptor for this revision." });
  }
  const importable = !!result && holds.length === 0;
  const reviewable = importable && !result!.draft.accepted;
  const advisories = warnings ? ` ${plural(warnings, "advisory is a note", "advisories are notes")} you can resolve later.` : "";
  const readiness: { tone: Tone; title: string; detail: string; syntax: boolean } =
    checking ? { tone: "dim", title: "Checking…", detail: "Import waits for this check.", syntax: false }
    : !current ? { tone: "faint", title: "Not checked", detail: "The edited text is checked shortly after typing stops.", syntax: false }
    : importable ? { tone: "ok", title: "✓ Ready to import", detail: `Nothing blocks the import.${advisories} Review is optional.`, syntax: false }
    : errors ? { tone: "bad", title: "● Import held", detail: `${plural(errors, "blocking problem", "blocking problems")} to fix.${advisories}`, syntax: current.preview === null }
    : { tone: "warn", title: dirty && current.valid ? "Valid · not saved" : "Import held", detail: holds.map(h => h.reason).join(" · "), syntax: false };

  // The subject is one clipped status line: name, revision and a single state. Counts, review and the
  // source live in the body, where they wrap.
  const subject = useMemo<Segment[]>(() => {
    if (!result) return [{ text: "draft", role: "mono-dim" }];
    const dot: Segment = { text: " · ", role: "mono-faint" };
    const parts: Segment[] = [{ text: preview?.provider || result.draft.key.service, role: "mono-ref" }, dot, { text: revision, role: "mono-dim" }];
    if (dirty) parts.push(dot, { text: "unsaved", role: "mono-warn" });
    parts.push(dot, draftState({ checking, current: !!current, valid: !!current?.valid, errors, importable }));
    return parts;
  }, [result, preview?.provider, revision, dirty, checking, current, errors, importable]);
  const subjectKey = JSON.stringify(subject);
  useEffect(() => onSubject(subject), [subjectKey]);

  /* ---------- navigation ---------- */

  const reveal = (target: { index?: number; from: number; to: number }, focus: boolean) => {
    if (tab !== "source") { pendingReveal.current = { target, focus }; setTab("source"); return; }
    editor.current?.reveal(target, focus);
  };
  useEffect(() => {
    if (tab !== "source" || !pendingReveal.current) return;
    const { target, focus } = pendingReveal.current;
    pendingReveal.current = undefined;
    editor.current?.reveal(target, focus);
  }, [tab]);
  /** A diagnostic's range: its pushed mark when those marks are the ones in the editor, its own offsets otherwise. */
  const goToProblem = (problem: DraftProblem, focus: boolean) => {
    setSelected(problem.label);
    setEvidenceTarget(problem.target);
    reveal({ index: problem.index, from: problem.from, to: problem.to }, focus);
  };
  const goToPointer = (pointer: string) => {
    setEvidenceTarget(pointer);
    const at = offsetOfPointer(textRef.current, pointer);
    if (at !== undefined) reveal({ from: at, to: at }, true);
  };
  const step = (by: 1 | -1) => {
    if (!problems.length) return;
    const index = problems.findIndex(p => p.label === selected);
    const next = problems[(index + by + problems.length) % problems.length]!;
    goToProblem(next, false);
  };
  const keys = (event: KeyboardEvent) => {
    const target = event.target as HTMLElement;
    if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey || target.closest?.(".cm-editor, input, textarea, select")) return;
    const act = ({ "]": () => step(1), "[": () => step(-1), r: () => { if (reviewable) void run(review); }, e: () => { setTab("source"); editor.current?.focus(); }, i: () => setTab("import") } as Record<string, () => void>)[event.key];
    if (!act) return;
    event.preventDefault();
    act();
  };

  const review = async () => {
    const base = resultRef.current;
    if (!base || !reviewable) return;
    const next = await draftApi.review(base.draft.key, base.draft.revision);
    // A review answers for its own revision; one that lands after another save changes nothing.
    if (resultRef.current?.draft.revision !== base.draft.revision || next.draft.revision !== base.draft.revision) return;
    adopt(next, false);
    onSaved(next.draft);
    setMessage(`Reviewed ${revisionLabel(revisions, next.draft.revision)}.`);
  };

  if (!result) return <div className="draft-workbench-loading">{message ? <p role="status" className="mono-bad">{message}</p> : <p className="mono-dim">Opening draft…</p>}</div>;

  const counts = (prefix: string) => {
    const matching = problems.filter(p => p.target === prefix || p.target.startsWith(`${prefix}/`));
    const e = matching.filter(p => p.severity === "error").length;
    return { e, w: matching.length - e };
  };
  const countMarks = (prefix: string) => {
    const { e, w } = counts(prefix);
    return <span className={stale ? "mono-faint" : ""}>{e > 0 && <span className={stale ? "" : "mono-bad"}>{` ${e}${stale ? "○" : "●"}`}</span>}{w > 0 && <span className={stale ? "" : "mono-warn"}>{` ${w}${stale ? "△" : "▲"}`}</span>}</span>;
  };
  const checkedLine = stale ? (checking ? "checking… · earlier result stale" : check ? "stale" : "not checked") : `checked ${dirty ? "unsaved text" : revision}`;

  const outline = <nav className="draft-outline" aria-label="Draft outline">
    <p><span className="mono-ink draft-heading">outline</span><span className="mono-faint">{preview ? ` ${preview.operations.length} operations · ${Object.keys(preview.types).length} types` : " nothing parsed yet"}{previewStale ? " · last parsed text" : ""}</span></p>
    {preview?.operations.map(op => <button type="button" key={op.index} className={`draft-outline-row ${evidenceTarget === `#/operations/${op.index}` ? "draft-chosen" : ""}`} onClick={() => goToPointer(`#/operations/${op.index}`)}>
      <span className="mono-ink">{op.name}</span><span className="mono-dim">{` ${op.method} ${op.route}`}</span>{countMarks(`#/operations/${op.index}`)}
    </button>)}
    <p><span className="mono-ink draft-heading">types</span></p>
    {preview && Object.entries(preview.types).map(([name, def]) => {
      const fields = def && typeof def === "object" && !Array.isArray(def) && (def as Record<string, unknown>).fields;
      const pointer = `#/types/${name.replace(/~/g, "~0").replace(/\//g, "~1")}`;
      return <button type="button" key={name} className={`draft-outline-row ${evidenceTarget === pointer ? "draft-chosen" : ""}`} onClick={() => goToPointer(pointer)}>
        <span className="mono-ink">{name}</span><span className="mono-dim">{fields && typeof fields === "object" ? ` ${Object.keys(fields).length} fields` : ""}</span>{countMarks(pointer)}
      </button>;
    })}
    <p className="mono-faint draft-legend">● blocks import · ▲ advisory · ⏎ jumps to the source{stale ? " · ○ △ stale" : ""}</p>
  </nav>;

  const chosen = problems.find(p => p.label === selected);
  const strip = chosen && <section className="draft-strip" aria-label="Selected problem">
    <p><span className={chosen.severity === "error" ? "mono-bad" : "mono-warn"}>{`${chosen.severity === "error" ? "●" : "▲"} ${chosen.label}`}</span><span className="mono-ink">{`  ${chosen.operation}${chosen.field ? ` · ${chosen.field}` : ""}  `}</span><span className={chosen.severity === "error" ? "mono-bad" : "mono-warn"}>{chosen.message}</span><span className="mono-faint">{`  line ${chosen.line}${stale ? " · stale" : ""}`}</span></p>
    {chosen.fix && <p><span className="mono-ref">fix </span><span className="mono-ink">{chosen.fix}</span></p>}
    {chosen.code === "DRAFT_UNRESOLVED" && <div>
      <p className="settings-note">Use only for behavior notes such as simulated writes. Required HTTP or schema rules must stay blocking.</p>
      <button type="button" className="screen-chip cell-action" disabled={busy || stale || checking} onClick={() => run(async () => {
        if (!current || checking) return;
        const { keepAsAdvisory } = await import("../draft-advisory");
        const next = keepAsAdvisory(textRef.current, current, chosen);
        if (next === undefined) { setFailed(true); setMessage("Check the current source before moving this problem."); return; }
        change(next); setSelected(undefined); setFailed(false);
        setMessage("Kept as an advisory in the edited source. Check and save; other requirements still apply.");
      })}>Keep as advisory</button>
    </div>}
  </section>;

  const problemList = <section className="draft-problems" aria-label="Problems">
    <p><span className="mono-ink draft-heading">problems</span><span className="mono-dim">{` ${problems.length}`}</span>{errors > 0 && <span className={stale ? "mono-faint" : "mono-bad"}>{`  errors ${errors}`}</span>}{warnings > 0 && <span className={stale ? "mono-faint" : "mono-warn"}>{`  warnings ${warnings}`}</span>}<span className="mono-faint">{`  ${checkedLine}`}</span></p>
    {problems.length === 0 ? <p className="mono-dim">{current ? "No problems." : checking ? "Checking…" : "Not checked yet."}</p>
      : groupProblems(problems).map(group => <details key={group.key} className="draft-problem-group" open={group.severity === "error" ? true : undefined} aria-label={`${group.code} ${group.problems.length}`}>
        <summary><span className={stale ? "mono-faint" : group.severity === "error" ? "mono-bad" : "mono-warn"}>{`${group.severity === "error" ? (stale ? "○" : "●") : (stale ? "△" : "▲")} ${group.message ?? group.code}`}</span><span className="mono-faint">{`  ${group.code} · ${group.problems.length}`}</span></summary>
        {group.fix && <p className="mono-dim draft-problem-fix">{group.fix}</p>}
        <ol className="draft-problem-list">{group.problems.map(p => <li key={p.label}>
        <button type="button" className={`draft-problem ${stale ? "draft-stale" : ""} ${p.label === selected ? "draft-chosen" : ""}`} aria-label={`${p.label} ${p.operation} ${p.field} ${p.message}`} onClick={() => goToProblem(p, true)}>
          <span className={stale ? "mono-faint" : p.severity === "error" ? "mono-bad" : "mono-warn"}>{`${p.severity === "error" ? (stale ? "○" : "●") : (stale ? "△" : "▲")} ${p.label}`}</span>
          <span className="mono-ink">{p.operation}</span>{p.field && <span className="mono-ref">{p.field}</span>}{group.message === undefined && <span className="mono-ink">{p.message}</span>}<span className="mono-faint">{`line ${p.line}`}</span>{!group.fix && p.fix && <span className="mono-dim">{p.fix}</span>}
        </button></li>)}</ol>
      </details>)}
  </section>;

  const evidence = evidenceTarget && <DraftEvidence target={evidenceTarget} provenance={provenance} manual={isManual(manualTargets, evidenceTarget)} dirty={dirty} />;

  const actions = <>
    <div className="spec-tools draft-actions">
      <button type="button" className="screen-chip chip-chosen" disabled={busy || (!dirty && !needsMaterialization)} onClick={() => void run(async () => { await save(); })}>{`save${dirty || needsMaterialization ? ` as r${revisions.length + 1}` : ""}`}</button>
      <button type="button" className="screen-chip cell-action" disabled={busy} onClick={() => void validate(textRef.current)}>check</button>
      <button type="button" className="screen-chip cell-action" disabled={!importable} onClick={() => setTab("import")}>{`import…${importable ? "" : " · held"}`}</button>
    </div>

  </>;

  const operationsTable = preview ? <SpecOperations types={preview.types} operations={preview.operations} provenance={provenance} stale={previewStale || stale} problems={problems} onSource={goToPointer} onEvidence={setEvidenceTarget} onType={name => { setSchemaType({ name }); setTab("schema"); }} evidence={target => <DraftEvidence target={target} provenance={provenance} manual={isManual(manualTargets, target)} dirty={dirty}/>} /> : <p className="mono-dim">Nothing has parsed yet; the source tab has the text.</p>;
  const previewNotice = previewStale && <p role="status" className="mono-warn">last parsed text · stale</p>;

  return <div className="draft-workbench" onKeyDown={keys}>
    {actions}
    <section className={`draft-readiness draft-readiness-${readiness.tone}`} aria-label="Readiness" role="status">
      <p><span className={`mono-${readiness.tone}`}>{dirty ? "● unsaved changes · " : `${revision} saved · `}{readiness.title}</span><span className="mono-dim">{`  ${readiness.detail}`}</span></p>
      {previewStale && <p className="mono-warn">Last parsed text · stale. Earlier problem counts do not describe your edits.</p>}
      {readiness.syntax && <p className="mono-bad">The source does not parse as JSON; save still keeps it as a draft revision.</p>}
      {errors > 0 && !stale && <button type="button" className="screen-chip cell-action" onClick={() => goToProblem(problems.find(p => p.severity === "error")!, true)}>go to first problem</button>}
    </section>
    <details className="spec-secondary"><summary>Details · {stale ? "earlier checks stale" : `${errors} blocking · ${warnings} advisory`} · review {result.draft.accepted ? "recorded" : "optional"}</summary>
      <DraftAgentAccess key={JSON.stringify(result.draft.key)} draftKey={result.draft.key}/>
      <section aria-label="Review"><p className="settings-note">{result.draft.accepted ? `reviewed ${revision}` : "not reviewed · optional"} · Review acknowledges this saved revision; it never changes validation or import readiness.</p>
      <button type="button" className="screen-chip cell-action" disabled={busy || !reviewable} onClick={() => void run(review)}>{result.draft.accepted ? `reviewed ${dirty ? "saved " : ""}${revision} ✓` : "mark reviewed"}</button></section>
      <p className="settings-note">{revisions.length} saved draft revisions · return to the library to open earlier revisions.</p>
    </details>
    <div className="spec-tabs" role="tablist" aria-label="Draft views">
      {DRAFT_TABS.map(t => <button type="button" role="tab" aria-selected={tab === t} key={t} className="spec-tab" data-count={preview ? t === "operations" ? preview.operations.length : t === "schema" ? `${Object.keys(preview.types).length} types` : undefined : undefined} onClick={() => setTab(t)}>
        {t === "problems" ? `problems ${problems.length}${stale && check ? " · stale" : ""}` : t === "import" ? `import${importable ? "" : " · held"}` : t}
      </button>)}
    </div>

    {tab === "source" && <div className="draft-layout">
      {outline}
      <div className="draft-main">
        <p><span className="mono-ink draft-heading">source</span><span className="mono-faint">{` ${revision} · ${dirty ? "unsaved" : "saved"} · ${checkedLine}`}</span></p>
        <DraftSourceEditor handle={editor} text={text} onChange={change} onSave={() => void run(async () => { await save(); })} onCheck={() => void validate(textRef.current)}
          onCaret={at => { const pointer = evidencePointerAt(textRef.current, at); if (pointer) setEvidenceTarget(pointer); }}
          typeNames={() => Object.keys(preview?.types ?? {})}
          marks={check ? { forText: check.text, marks: check.validation.diagnostics.map(d => ({ from: d.from, to: d.to, severity: d.severity, message: d.message })) } : undefined} />
        {strip}
        {problemList}
        {evidence}
      </div>
    </div>}

    {tab === "problems" && <div className="draft-main">
      {problemList}
      {strip}
      {evidence}
    </div>}

    <div className="draft-main" hidden={tab !== "operations"}>{previewNotice}{operationsTable}{tab === "operations" && evidence}</div>
    <div className="draft-main" hidden={tab !== "schema"}>{previewNotice}{preview ? <SchemaFieldsTable problems={problems} stale={stale} types={preview.types} provenance={provenance} requestedType={schemaType} {...(evidenceTarget ? { selected: evidenceTarget } : {})} onSelect={setEvidenceTarget} /> : <p className="mono-dim">Nothing has parsed yet.</p>}{tab === "schema" && evidence}</div>

    {tab === "import" && <div className="draft-main">
      <SpecImportForm name={result.draft.key.service} kind="draft" revision={revision} workspace={workspace} descriptorPath={result.descriptorPath} busy={busy}
        state={dirty ? { text: "● unsaved changes", tone: "mono-warn" } : importable ? { text: "ready to import", tone: "mono-ok" } : { text: "import held", tone: "mono-warn" }}
        holds={holds.map(h => ({ title: h.title, detail: h.detail, tone: h.tone, ...(h.save ? { action: <button type="button" className="spec-t-action" disabled={busy} onClick={() => void run(async () => { await save(); })}>save</button> } : {}) }))}
        alias={alias} onAlias={setAlias} endpoint={endpoint} onEndpoint={setEndpoint} replace={replace} onReplace={setReplace}
        notes={<p className="spec-t-label">Review is optional.</p>}
        onSubmit={command => { if (!importable) return; void run(async () => { const cell = await onSubmit(command); if (cell) onClose(); }); }} />
      <div className="settings-field-row">
        <input className="settings-field" aria-label="Draft export path" value={exportPath} onChange={e => setExportPath(e.target.value)} placeholder="Absolute path for a new spec file" />
        <button className="screen-chip cell-action" type="button" disabled={busy || !importable || !exportPath} onClick={() => void run(async () => {
          const r = await draftApi.export(result.draft.key, result.draft.revision, exportPath);
          setMessage(`Exported ${r.exportedPath}`);
        })}>export</button>
      </div>
    </div>}

    {message && <p role="status" className={`spec-message ${failed ? "mono-bad" : "mono-dim"}`}>{message}</p>}
  </div>;
}

/**
 * Evidence for one target: the describe step's original records, then — separately — whether you
 * supplied it. A manual fact is never shown as documented. Original records are history once the
 * saved text changed, and while unsaved edits exist; unsaved edits are recorded only when saved.
 */
export function DraftEvidence({ target, provenance, manual, dirty }: { target: string; provenance: SchemaProvenance | undefined; manual: boolean; dirty: boolean }) {
  const own = provenanceEntriesFor(provenance, target);
  const above = provenance?.entries.filter(e => target.startsWith(`${e.target}/`)) ?? [];
  const entries: ProvenanceEntry[] = [...above, ...own.filter(e => !above.includes(e))];
  const stale = provenance?.status === "stale";
  return <section className="draft-evidence spec-provenance" aria-label="Evidence">
    <p><span className="mono-ink draft-heading">evidence</span><span className="mono-ref">{` ${target}`}</span><span className="mono-faint">{stale ? "  describe · history" : "  describe"}</span></p>
    {entries.length === 0 ? <p className="mono-dim">no original evidence recorded</p>
      : <ol className="spec-provenance-entries">{entries.map((entry, i) => <li key={`${entry.target}:${i}`} className="spec-provenance-entry draft-evidence-original">
        <p><span className={entry.basis === "documented" && !stale ? "mono-ok" : "mono-warn"}>{stale ? `${entry.basis} · stale` : entry.basis}</span><span className="mono-faint">{` · source ${entry.source}`}</span>
          {provenance?.location && <span className="mono-faint">{` · ${provenance.location}`}</span>}{entry.lines.length > 0 && <span className="mono-faint">{` · lines ${describeLineRanges(entry.lines)}`}</span>}</p>
        <p className="mono-ink spec-provenance-reason">{entry.reason}</p>
      </li>)}</ol>}
    {manual && <p className="draft-evidence-manual"><span className="mono-ref">manual</span><span className="mono-dim"> · you</span><span className="mono-faint"> · never documented</span></p>}
    {dirty && <p className="draft-evidence-manual"><span className="mono-warn">unsaved edits</span><span className="mono-faint"> · recorded when saved</span></p>}
  </section>;
}
