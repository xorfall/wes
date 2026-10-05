import { useEffect, useImperativeHandle, useRef, useState, type Ref } from "react";
import type { EditorView } from "@codemirror/view";
import type { DraftMark } from "./draft-editor-wiring";
import "./editor.css";

/** What the draft screen may ask of the editor: put the caret on a range, optionally taking focus. */
export interface DraftEditorHandle {
  /** `index` names a pushed diagnostic, whose range has been carried through every edit since; `fallback` is used otherwise. */
  reveal(target: { index?: number; from: number; to: number }, focus: boolean): void;
  focus(): void;
}

export interface DraftSourceEditorProps {
  text: string;
  /** Marks and the exact text they were computed for; pushed only while the editor holds that text. */
  marks: { forText: string; marks: readonly DraftMark[] } | undefined;
  onChange: (text: string) => void;
  onSave: () => void;
  onCheck: () => void;
  onCaret: (offset: number) => void;
  typeNames: () => readonly string[];
  handle?: Ref<DraftEditorHandle>;
}

/** The draft's JSON text. Validity is the backend's; this only edits, completes, indents and shows marks. */
export function DraftSourceEditor({ text, marks, onChange, onSave, onCheck, onCaret, typeNames, handle }: DraftSourceEditorProps) {
  const parent = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView>();
  const wiring = useRef({ onChange, onSave, onCheck, onCaret, typeNames });
  wiring.current = { onChange, onSave, onCheck, onCaret, typeNames };
  const latest = useRef({ text, marks });
  latest.current = { text, marks };
  const pending = useRef<() => void>();
  const module = useRef<typeof import("./draft-editor-wiring")>();
  const [failed, setFailed] = useState(false);

  const pushMarks = () => {
    const current = view.current;
    const { marks: next } = latest.current;
    if (!current || !module.current || !next || current.state.doc.toString() !== next.forText) return;
    current.dispatch({ effects: module.current.setDraftMarks.of({ marks: next.marks }) });
  };

  useEffect(() => {
    let active = true;
    void import("./draft-editor-wiring").then(wired => {
      if (!active || !parent.current) return;
      module.current = wired;
      view.current = wired.makeDraftEditor(parent.current, latest.current.text, {
        onChange: t => wiring.current.onChange(t), onSave: () => wiring.current.onSave(), onCheck: () => wiring.current.onCheck(),
        onCaret: at => wiring.current.onCaret(at), typeNames: () => wiring.current.typeNames(),
      });
      pushMarks();
      const queued = pending.current;
      pending.current = undefined;
      queued?.();
    }).catch(() => { if (active) setFailed(true); });
    return () => { active = false; view.current?.destroy(); view.current = undefined; };
  }, []);

  useEffect(() => {
    const current = view.current;
    if (current && current.state.doc.toString() !== text) current.dispatch({ changes: { from: 0, to: current.state.doc.length, insert: text } });
  }, [text]);
  useEffect(pushMarks, [marks]);

  useImperativeHandle(handle, () => ({
    reveal(target, focus) {
      const run = () => {
        const current = view.current;
        if (!current || !module.current) return;
        const mapped = target.index === undefined ? undefined : module.current.markRange(current.state, target.index);
        const length = current.state.doc.length;
        const from = Math.max(0, Math.min((mapped ?? target).from, length));
        const to = Math.max(from, Math.min((mapped ?? target).to, length));
        current.dispatch({ selection: { anchor: from, head: to }, scrollIntoView: true });
        if (focus) current.focus();
      };
      if (view.current) run(); else pending.current = run;
    },
    focus() { if (view.current) view.current.focus(); else pending.current = () => view.current?.focus(); },
  }), []);

  return failed
    ? <textarea aria-label="Draft source" className="spec-source-fallback" value={text} onChange={e => onChange(e.target.value)} />
    : <div className="spec-source-editor draft-source-editor surface-sunk" aria-label="Draft source" ref={parent} />;
}
