import { useId, useLayoutEffect, useRef, useState } from "react";
import { acceptInto, UNFOCUSED } from "../focus";
import { composing } from "../platform-keys";
import { MonoLine } from "./MonoLine";
import { asks, promptCompletion, suggestionLine, VISIBLE_ROWS } from "./prompt-complete";
import "./editor.css";

/** Screens accept UI commands with the same workspace completion as their session. */
export function PaneCommand({ onCommand, variables = [], workspaces = [], dashboards=[] }: {
  onCommand: (text: string) => void; variables?: readonly string[]; workspaces?: readonly string[];dashboards?:readonly string[];
}) {
  const [draft, setDraft] = useState(""), [caret, setCaret] = useState(0), [chosen, setChosen] = useState(0), [shut, setShut] = useState(false);
  const field = useRef<HTMLInputElement>(null), wanted = useRef<number>(), id = useId();
  const completion = asks(draft, caret) && !shut ? promptCompletion({ line: draft, caret,
    catalogue: { commands: [], annotations: [], providers: [] }, names: variables, variables, aliases: {}, workspaces,dashboards }) : { from: 0, items: [] };
  const at = Math.min(chosen, Math.max(0, completion.items.length - 1));
  const first = Math.max(0, at - VISIBLE_ROWS + 1);
  useLayoutEffect(() => { if (wanted.current !== undefined) { field.current?.setSelectionRange(wanted.current, wanted.current); wanted.current = undefined; } }, [draft]);
  const accept = (index: number) => {
    const suggestion = completion.items[index]; if (!suggestion) return;
    const next = acceptInto(UNFOCUSED, draft, caret, completion.from, suggestion.text, suggestion.separate);
    wanted.current = next.caret; setDraft(next.text); setCaret(next.caret); setShut(true); field.current?.focus();
  };
  return <form className="pane-command" onSubmit={event => {
    event.preventDefault(); if (!draft.trim()) return; onCommand(draft); setDraft(""); setCaret(0); setShut(false);
  }}>
    {!!completion.items.length && <div id={id} className="prompt-completion surface-sunk" role="listbox" aria-label="Pane suggestions">
      {completion.items.slice(first, first + VISIBLE_ROWS).map((suggestion, index) => <div
        key={`${suggestion.kind}:${suggestion.text}`} id={`${id}-${first + index}`} role="option" aria-selected={first + index === at}
        onMouseDown={event => { event.preventDefault(); accept(first + index); }}>
        <MonoLine className="editor-candidate" segments={suggestionLine(suggestion, first + index === at)} />
      </div>)}
    </div>}
    <input ref={field} aria-label="Pane command" placeholder="/goto · /tab $value · /split related $value · /close"
      role="combobox" aria-autocomplete="list" aria-expanded={!!completion.items.length} aria-controls={id}
      aria-activedescendant={completion.items.length ? `${id}-${at}` : undefined}
      value={draft} onChange={event => { setDraft(event.target.value); setCaret(event.target.selectionStart ?? event.target.value.length); setChosen(0); setShut(false); }}
      onSelect={event => setCaret(event.currentTarget.selectionStart ?? draft.length)}
      onKeyDown={event => {
        if (composing(event)) return;
        if (event.key === "Escape" && completion.items.length) { event.preventDefault(); event.stopPropagation(); setShut(true); }
        if (!completion.items.length || event.metaKey || event.ctrlKey || event.altKey) return;
        if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); setChosen((at + (event.key === "ArrowDown" ? 1 : -1) + completion.items.length) % completion.items.length); }
        if (event.key === "Tab" && !event.shiftKey) { event.preventDefault(); accept(at); }
      }} />
  </form>;
}
