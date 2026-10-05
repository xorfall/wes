/**
 * Plain multiline input over the surface's highlighted source. Both layers share
 * metrics and scroll positions; the textarea owns native editing and its caret.
 * Shift+Enter inserts a newline; Mod+Enter runs; Mod+Shift+Enter moves the draft to the editor.
 * Mod is the platform's primary modifier (`platform-keys.ts`): Cmd on macOS, Ctrl elsewhere.
 */
import { useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { acceptInto, UNFOCUSED } from "../focus";
import { composing, primaryHeld } from "../platform-keys";
import { atHistoryBoundary, moveRecall, type Recall } from "../prompt";
import { expandAlias, type Aliases } from "../aliases";
import type { Catalogue } from "../vocabulary";
import { commandSegments } from "./command-line";
import { MonoLine, type Segment } from "./MonoLine";
import { VISIBLE_ROWS, asks, promptCompletion, suggestionLine, suggestionSource, type Suggestion } from "./prompt-complete";
import type { Language } from "./language";
import type { Theme } from "./Cell";
import "./editor.css";

/** The prompt line's runs. The caret is the field's own, so no block is drawn for it. */
export function promptLine(text: string, language?: Language): Segment[] {
  return commandSegments(text, language);
}

/** One row of the open list, whichever list it is. The two sources agree on this much. */
interface Offer {
  readonly key: string;
  readonly text: string;
  readonly line: (chosen: boolean) => Segment[];
  readonly suggestion: Suggestion;
}

export interface PromptProps {
  readonly language?: Language;
  readonly autoFocus?: boolean;
  readonly draft: string;
  readonly onDraft: (text: string) => void;
  readonly onSubmit: (text: string) => void;
  readonly chromeName: Theme;
  readonly onChrome: (chrome: Theme) => void;
  /** What the engine said this workspace can do. The one-line suggestions are read from it. */
  readonly catalogue: Catalogue;
  /** Every way a result can be referred to, so `$` offers what there is. */
  readonly names: readonly string[];
  readonly variables?: readonly string[];
  readonly dashboards?:readonly string[];
  readonly aliases: Aliases;
  readonly workspaces?: readonly string[];
  /** This session's submitted/restored commands, oldest first. Recall never submits them. */
  readonly history?: readonly string[];
  /** A workspace replacement must not carry an old history walk into the new session. */
  readonly historyScope?: string;
  /** Mod+Shift+Enter hands the exact draft to the workspace editor. */
  readonly onGrow: (text: string) => void;
}

export function Prompt({ language, draft, onDraft, onSubmit, chromeName, onChrome, catalogue, names, variables, dashboards, aliases, workspaces, onGrow, history = [], historyScope = "", autoFocus = true }: PromptProps) {
  const field = useRef<HTMLTextAreaElement>(null);
  const drawn = useRef<HTMLPreElement>(null);
  const [caret, setCaret] = useState(draft.length);
  /** Somebody pressed ⌃space: answer even where the rule would not have offered. */
  const [asked, setAsked] = useState(false);
  /** Somebody pressed esc or took a suggestion: stay shut until the next keystroke. */
  const [shut, setShut] = useState(false);
  const [chosen, setChosen] = useState(0);
  const recalled = useRef<{ readonly text: string; readonly recall: Recall }>();

  const at = Math.min(caret, draft.length);
  const wanted = asked || (!shut && asks(draft, at));
  const offers = useMemo<{ readonly hint?: string; readonly from: number; readonly items: readonly Offer[] }>(() => {
    if (!wanted) return { from: 0, items: [] };
    const completion = promptCompletion({ line: draft, caret: at, catalogue, names, variables,dashboards, aliases, workspaces });
    return {
      from: completion.from, hint: completion.hint,
      items: completion.items.map((suggestion) => ({
        key: `${suggestion.kind}:${suggestion.text}`,
        text: suggestion.text,
        line: (isChosen: boolean) => suggestionLine(suggestion, isChosen),
        separate: suggestion.separate,
        suggestion,
      })),
    };
  }, [wanted, draft, at, catalogue, names, variables,dashboards, aliases, workspaces]);

  const open = offers.items.length > 0;
  const here = Math.min(chosen, Math.max(0, offers.items.length - 1));
  const firstVisible = Math.max(0, Math.min(here - VISIBLE_ROWS + 1, offers.items.length - VISIBLE_ROWS));
  const visibleOffers = offers.items.slice(firstVisible, firstVisible + VISIBLE_ROWS);
  const source = suggestionSource(offers.items.map(offer => offer.suggestion.kind));
  const expansion = useMemo(() => {
    const written = draft;
    try {
      const expanded = expandAlias(written, aliases, catalogue);
      return expanded === written ? undefined : { text: `alias → ${expanded}`, role: "mono-dim" as const };
    } catch (error) {
      return { text: (error as Error).message, role: "mono-bad" as const };
    }
  }, [draft, aliases, catalogue]);
  /*
   * Whether taking the chosen suggestion would change anything.
   *
   * Somebody who typed `/settings` in full and pressed ⏎ meant to run it. The list is still open —
   * `/settings` is a perfectly good suggestion for `/settings` — and accepting it would leave the
   * line exactly as it is and swallow the keystroke.
   */
  const already = offers.items[here]?.text === draft.slice(offers.from, at);

  /**
   * Puts the text back with the caret where it now belongs.
   *
   * The field's value arrives with the next render, so the caret has to be placed after it — but
   * not a frame after it. A `requestAnimationFrame` here is a race somebody typing quickly wins: a
   * newline inserted at the caret, then four characters typed before the frame, and the frame then
   * drags the caret back to where the newline was. So the wanted position is left here and applied
   * in the layout effect below, in the same commit that writes the value.
   */
  const caretWanted = useRef<{ text: string; start: number; end: number; direction?: "forward" | "backward" | "none" }>();
  const committedDraft = useRef(draft);
  const put = (text: string, where: number) => {
    caretWanted.current = { text, start: where, end: where };
    onDraft(text);
    setCaret(where);
  };

  const accept = (offer: Offer | undefined) => {
    recalled.current = undefined;
    setAsked(false);
    setShut(true);
    if (!offer) return;
    const taken = acceptInto(UNFOCUSED, draft, at, offers.from, offer.text, (offer as { separate?: boolean }).separate);
    put(taken.text, taken.caret);
  };

  const follow = (element: HTMLTextAreaElement) => setCaret(element.selectionStart ?? element.value.length);

  /* The drawn line scrolls with the field, so a long command stays under its own caret. */
  const mirror = () => {
    const box = field.current;
    const ink = drawn.current;
    if (!box || !ink || (box.clientWidth === 0 && box.clientHeight === 0)) return;
    // Scroll tracks belong only to the native field. Match its content viewport,
    // otherwise the overlay clamps earlier at the bottom/right with visible bars.
    if (box.clientWidth > 0 && box.clientHeight > 0) {
      ink.style.width = `${box.clientWidth}px`;
      ink.style.height = `${box.clientHeight}px`;
    }
    ink.scrollLeft = box.scrollLeft;
    ink.scrollTop = box.scrollTop;
  };

  useLayoutEffect(() => {
    const box = field.current;
    if (!box || typeof ResizeObserver === "undefined") return;
    let active = true;
    const observer = new ResizeObserver(() => { if (active) mirror(); });
    observer.observe(box);
    return () => { active = false; observer.disconnect(); };
  }, []);

  /* Every render, because both jobs are about the field as it now is on screen. */
  useLayoutEffect(() => {
    const box = field.current;
    if (!box) return;
    const wanted = caretWanted.current;
    if (wanted && wanted.text === draft && box.value === draft) {
      caretWanted.current = undefined;
      if (box.selectionStart !== wanted.start || box.selectionEnd !== wanted.end || (wanted.direction !== undefined && box.selectionDirection !== wanted.direction)) {
        if (wanted.direction === undefined) box.setSelectionRange(wanted.start, wanted.end);
        else box.setSelectionRange(wanted.start, wanted.end, wanted.direction);
      }
    } else if (committedDraft.current !== draft) {
      // Another owner replaced the draft. Never restore an old selection if that text returns.
      caretWanted.current = undefined;
    }
    committedDraft.current = draft;
    mirror();
  });

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (composing(event)) return;
    const key = event.key;
    // Exact chords only: anything else held belongs to the field, the pane or the platform.
    const bare = !event.ctrlKey && !event.metaKey && !event.altKey;
    const primary = primaryHeld(event) && !event.altKey;
    if (key === " " && event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey) {
      return stop(event, () => { setChosen(0); setShut(false); setAsked(true); });
    }
    if (key === "Escape" && open) return stop(event, () => { setAsked(false); setShut(true); });
    // Shift+Tab moves focus back and Ctrl+Tab cycles panes; only a bare Tab completes.
    if (key === "Tab" && bare && !event.shiftKey) {
      return stop(event, () => (open ? accept(offers.items[here]) : (setChosen(0), setShut(false), setAsked(true))));
    }
    const submit = () => { recalled.current = undefined; setAsked(false); onSubmit(draft); };
    if (key === "Enter" && event.shiftKey && primary) {
      return stop(event, () => { recalled.current = undefined; onGrow(draft); });
    }
    if (key === "Enter" && event.shiftKey && bare) return stop(event, () => {
      const { selectionStart, selectionEnd } = event.currentTarget;
      recalled.current = undefined;
      setAsked(false); setShut(true); setChosen(0);
      put(draft.slice(0, selectionStart) + "\n" + draft.slice(selectionEnd), selectionStart + 1);
    });
    if (key === "Enter" && !event.shiftKey && primary) return stop(event, submit);
    if (key === "Enter" && !event.shiftKey && bare) {
      return stop(event, () => (open && !already ? accept(offers.items[here]) : submit()));
    }
    if (key === "ArrowDown" || key === "ArrowUp") {
      if (event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
      const step = key === "ArrowDown" ? 1 : -1;
      if (open) return stop(event, () => setChosen((was) => (was + step + offers.items.length) % offers.items.length));
      const start = event.currentTarget.selectionStart;
      const end = event.currentTarget.selectionEnd;
      if (!atHistoryBoundary(draft, start, end, step)) return;
      return stop(event, () => {
        // The parent may replace the text without an input event (screen commands, for example).
        const previous = recalled.current?.text === draft ? recalled.current.recall : undefined;
        const next = moveRecall(history, UNFOCUSED, historyScope, draft, previous, step);
        recalled.current = next.recall ? { text: next.text, recall: next.recall } : undefined;
        setAsked(false); setShut(true); setChosen(0);
        put(next.text, next.text.length);
      });
    }
    // `/theme cell …` is the command; this is the same setting without leaving the line. Shift keeps
    // plain Ctrl+C for copying the selection where that is the platform's copy.
    if (key.toLowerCase() === "c" && event.ctrlKey && event.shiftKey && !event.metaKey && !event.altKey) {
      return stop(event, () => {
        const order: Theme[] = ["keys", "controls"];
        onChrome(order[(order.indexOf(chromeName) + 1) % order.length]!);
      });
    }
  };

  return (
    <div className="prompt-line">
      {offers.hint && <MonoLine segments={[{ text: offers.hint, role: "mono-dim" }]} className="editor-candidate-source" />}
      {open && (
        <div className="prompt-completion surface-sunk" role="listbox" aria-label="Suggestions">
          {visibleOffers.map((offer, index) => (
            <MonoLine key={offer.key} segments={offer.line(firstVisible + index === here)} className="editor-candidate" />
          ))}
          {offers.items.length > VISIBLE_ROWS && <MonoLine segments={[{ text: `${here + 1} / ${offers.items.length} suggestions · ↑ ↓ to browse · Tab to accept`, role: "mono-dim" }]} className="editor-candidate-source" />}
          <MonoLine segments={source} className="editor-candidate-source" />
        </div>
      )}
      <div className="prompt-stack">
        <pre className="mono-line prompt-prefix" aria-hidden="true"><span className="mono-ref-strong">❯</span></pre>
        <div className="prompt-editor">
          <pre className="mono-line prompt-drawn" ref={drawn} aria-hidden="true">
            {promptLine(draft, language).map((segment, index) => <span key={index} className={segment.role ?? "mono-ink"}>{segment.text}</span>)}
            <span>{"\u200b"}</span>
          </pre>
          <textarea
            ref={field}
            className="prompt-field"
            aria-label="Command"
            rows={Math.min(10, draft.split("\n").length)}
            wrap="off"
            spellCheck={false}
            value={draft}
            autoFocus={autoFocus}
            onChange={(event) => {
              recalled.current = undefined;
              setAsked(false);
              setShut(false);
              setChosen(0);
              const box = event.target;
              const start = box.selectionStart ?? box.value.length;
              // A scoped workspace publishes its updated controlled value through another
              // render. That write can reset the browser's selection, even for native input.
              caretWanted.current = (event.nativeEvent as InputEvent | undefined)?.isComposing ? undefined
                : { text: box.value, start, end: box.selectionEnd ?? start, direction: box.selectionDirection };
              setCaret(start);
              onDraft(box.value);
            }}
            onSelect={(event) => follow(event.currentTarget)}
            onScroll={mirror}
            onKeyDown={onKeyDown}
          />
        </div>
      </div>
      {expansion && <div role="status" aria-label="Alias expansion">
        <MonoLine segments={[expansion]} className="editor-candidate-source" />
      </div>}
    </div>
  );
}

function stop(event: KeyboardEvent<HTMLElement>, what: () => void) {
  event.preventDefault();
  event.stopPropagation();
  what();
}
