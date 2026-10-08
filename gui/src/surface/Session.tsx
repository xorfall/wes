/**
 * The session: a top bar, a scrollback of cells, and the prompt.
 *
 * It reads top to bottom in the order the work happened, which is the order a person did it in.
 * The chrome the cells wear is one setting for all of them, so changing it re-dresses the whole
 * scrollback in one state change and moves nothing that should not move.
 *
 * The two lines around the prompt describe the current session context: what a command typed now
 * would run against, and what the session has accumulated. A cell carries a fact only when that
 * fact could have differed for it; these say what is true right now.
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { useClipboard } from "./useClipboard";
import { shouldFollow } from "../following";
import { Inspector, type InspectorSelection } from "./Inspector";
import { useColumns, monoAdvance } from "./render/measure";
import { Cell, type CellActions, type CellBlock, type Theme } from "./Cell";
import { MonoLine } from "./MonoLine";
import type { SessionCell, SessionModel } from "./session-model";
import type { Workspace } from "../workspace";
import "./surface.css";
import "./session.css";
import type { DefinitionJump, RegisterDefinition } from "./definition-target";

export interface SessionProps {
  readonly definition?: { readonly pane: string; readonly workspace?: string; readonly register: RegisterDefinition };
  readonly jump?: DefinitionJump;
  readonly history?: import("./RunHistory").RunHistoryContext;
  readonly model: SessionModel;
  /** The cell theme, one setting for every cell: `controls` or `keys`. */
  readonly chrome: Theme;
  /** Whether the focused action footer includes key hints. */
  readonly tailKeys?: boolean;
  /** The prompt line. A node rather than runs, because the real caret lives in it. */
  readonly prompt: ReactNode;
  /** What each cell can be asked to do. The session owns the doing; the cell only offers it. */
  readonly actions?: (cell: SessionCell) => CellActions;
  /** The output blocks of one cell. */
  readonly output?: (cell: SessionCell) => readonly CellBlock[];
  readonly onFocus?: (id: string | undefined) => void;
  /**
   * Whether the scrollback stays with the newest line as it arrives.
   *
   * A terminal follows its output and stops the moment somebody scrolls up to read something. The
   * question is asked of the height before the growth — `following.ts` says why — so a stream that
   * grows while it is being read never yanks the page away.
   *
   * Off, the session stops both halves: no jump when a command is sent, and no staying with the
   * output. That is what somebody who turned it off asked for — to read an old cell while new work
   * runs underneath.
   */
  readonly following?: boolean;
  /**
   * A count of the commands sent, so the scrollback knows when one just was.
   *
   * Following is not only "stay at the bottom while it grows". Pressing ⏎ is itself a request to
   * see the answer: somebody who scrolled up to read an old cell and typed there meant to watch
   * what they just ran. `following.ts` calls that `pinned`; a changed count is what raises it, and
   * a count rather than a flag so that two commands in a row are two separate asks.
   */
  readonly pinned?: number;
  /**
   * `pane` when the session is one surface among several rather than the whole workspace.
   *
   * It drops the top bar and the footer and keeps the scrollback, the prompt and the context line —
   * the same bargain the screens strike in a pane. The split already says which workspace this is
   * and which keys move between panes, and saying either twice costs the pane the room it was
   * opened for.
   */
  readonly chromeMode?: "full" | "pane";
  /**
   * A `/clear` just asked the viewport to start after the unpinned cell `after` — or, when `after`
   * is absent, after nothing, because the unpinned scrollback was empty the moment it was asked.
   * Nothing is unmounted: every earlier cell stays reachable by scrolling up, and the pinned strip
   * is untouched. `revision` changes on every `/clear`, including a repeat that names the same
   * `after`, so the scrollback knows to move again rather than sitting still. The parent sets this
   * back to `undefined` on a generation reset.
   */
  readonly clearRequest?: { readonly after?: string; readonly revision: number };
}

/**
 * Whether a cell has work or attempt evidence that its run history could show.
 *
 * Read only from what the workspace already holds — nothing is fetched to decide. Admitted nodes
 * count whatever their state, including failed and pending ones; so does a rejected attempt, whose
 * admission may still be recorded, and any recorded diagnostic of the cell. An empty restored or
 * unrun cell has none, whatever history context the workspace has.
 */
export function hasRunEvidence(cell: SessionCell, workspace: Workspace): boolean {
  if (cell.nodes.length > 0) return true;
  const attempts = new Set([cell.id, ...(cell.attempt === undefined ? [] : [cell.attempt])]);
  if ([...attempts].some(id => workspace.attemptFailures?.[id] !== undefined || (workspace.cells[id]?.length ?? 0) > 0)) return true;
  return workspace.history.some(entry => entry.event === "log-diagnostic" && attempts.has(entry.record.cell));
}

export function Session({
  model, chrome, tailKeys = true, prompt, actions, output, onFocus, following = true, pinned = 0, chromeMode = "full",
  clearRequest, history, definition, jump,
}: SessionProps) {
  const clipboard=useClipboard();
  useEffect(() => definition?.register({ pane: definition.pane, workspace: definition.workspace, cells: model.cells.map(cell => cell.id) }),
    [definition?.register, definition?.pane, definition?.workspace, model.cells]);
  const layout = useRef<HTMLDivElement>(null);
  const columns = useColumns(layout);
  const [selection,setSelection] = useState<InspectorSelection>();
  const [hosts,setHosts] = useState<ReadonlyMap<string,InspectorSelection>>(new Map());
  const [windowed,setWindowed] = useState(false);
  const [paneWidth,setPaneWidth] = useState(64*monoAdvance());
  const returnTo = useRef<HTMLElement|null>(null);
  const overlay = windowed || columns < 72;
  const hostKey = (chosen:InspectorSelection) => `${chosen.cell}:${chosen.node ?? "submission"}`;
  const openInspector = (cell:SessionCell,node:string|undefined,tab:InspectorSelection["tab"]) => {
    returnTo.current = typeof document !== "undefined" ? document.activeElement as HTMLElement : null;
    const chosen={cell:cell.id,node:node ?? cell.nodes.at(-1)?.id,tab};
    setSelection(chosen);
    setHosts(was=>{const next=new Map(was);next.delete(hostKey(chosen));next.set(hostKey(chosen),chosen);while(next.size>8){const first=next.keys().next().value!;if(first===hostKey(chosen))break;next.delete(first);}return next;});
  };
  const closeInspector = () => { setSelection(undefined);returnTo.current?.focus?.({preventScroll:true}); };
  useLayoutEffect(()=>{if(surface.current){if(selection && overlay)surface.current.setAttribute?.("inert","");else surface.current.removeAttribute?.("inert");}},[selection,overlay]);
  useEffect(()=>{setSelection(undefined);setHosts(new Map());},[history?.generation]);
  const scrollback = useRef<HTMLDivElement>(null);
  const grown = useRef(0);
  const sent = useRef(pinned);
  const followingNow = useRef(following);
  followingNow.current = following;
  const watchCells = useRef<() => void>();

  const clearMark = useRef<HTMLDivElement | null>(null);
  const clearTail = useRef<HTMLDivElement | null>(null);
  const clearedRevision = useRef<number | undefined>(undefined);

  /*
   * The blank room under the fresh screen, sized so the cleared cells can be scrolled all the way
   * out of view even when little has run since. Zeroed and re-measured every time, because the tail
   * itself sits inside the scrollback and would otherwise be measuring its own last guess.
   */
  const recomputeClearTail = useCallback(() => {
    const box = scrollback.current;
    const tail = clearTail.current;
    if (!box || !tail) return;
    const mark = clearMark.current;
    if (!mark) {
      tail.style.height = "0px";
      return;
    }
    const was = box.scrollTop;
    tail.style.height = "0px";
    // scrollHeight is at least the viewport height, even when actual content is shorter.
    // Measure real content to avoid leaving short histories partly visible after clearing.
    const afterMark = tail.getBoundingClientRect().top - mark.getBoundingClientRect().top;
    tail.style.height = `${Math.max(0, box.clientHeight - afterMark)}px`;
    box.scrollTop = was;
  }, []);

  useLayoutEffect(() => {
    const box = scrollback.current;
    if (!box) return;
    // Re-measured on every render, so new output shrinks the tail as it fills the room the tail held.
    recomputeClearTail();
    // Taken whether or not the session is following, so turning it back on does not act on an old ⏎.
    const asked = sent.current !== pinned;
    sent.current = pinned;
    const before = grown.current;
    grown.current = box.scrollHeight;
    if (!following) return;
    // Revealing the focused action footer must not move a control between pointer down/up.
    const active = box.ownerDocument?.activeElement;
    if (!asked && active && box.contains(active) && active.closest("[data-cell]")) return;
    if (shouldFollow(asked, box.scrollTop, box.clientHeight, before)) box.scrollTop = box.scrollHeight;
  });

  /*
   * Cells also grow without this component rendering: a View frame reports its height after it
   * draws, a live block fills in. The height the next render compares against must be the height the
   * reader actually saw, or a render caused by nothing but focus reads a stale, shorter scrollback as
   * "the reader was at the bottom" and jumps there. The same question, asked of the height before the
   * growth, also lets such growth be followed while the reader is at the bottom.
   */
  useEffect(() => {
    const box = scrollback.current;
    if (!box || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      const before = grown.current, now = box.scrollHeight;
      if (now === before) return;
      grown.current = now;
      if (!followingNow.current) return;
      const active = box.ownerDocument?.activeElement;
      if (active && box.contains(active) && active.closest("[data-cell]")) return;
      if (shouldFollow(false, box.scrollTop, box.clientHeight, before)) box.scrollTop = now;
    });
    watchCells.current = () => { for (const child of Array.from(box.children ?? [])) observer.observe(child); };
    watchCells.current();
    return () => { watchCells.current = undefined; observer.disconnect(); };
  }, []);
  useLayoutEffect(() => watchCells.current?.());

  /*
   * A `/clear` moves the viewport on its own — asked explicitly, the same way pressing ⏎ is — so it
   * acts whether or not the session is following, and normal following resumes right after.
   * `clearedRevision` is what keeps this from repeating on every render a fresh cell causes once the
   * clear has already been acted on.
   */
  useLayoutEffect(() => {
    const box = scrollback.current;
    if (!box) return;
    if (clearRequest === undefined) {
      if (clearedRevision.current !== undefined) {
        clearedRevision.current = undefined;
        recomputeClearTail();
      }
      return;
    }
    if (clearedRevision.current === clearRequest.revision) return;
    clearedRevision.current = clearRequest.revision;
    recomputeClearTail();
    const mark = clearMark.current;
    if (mark) box.scrollTop += mark.getBoundingClientRect().top - box.getBoundingClientRect().top;
  }, [clearRequest, recomputeClearTail]);

  const clearing = clearRequest !== undefined;
  /*
   * One observer per session, watching only this session's own scrollback, so a split with more
   * than one session never reaches for a document-wide id to learn which one moved. It catches a
   * pane or window being resized after a clear; new output shrinking the tail is already caught by
   * the render above, since a cell only grows here in answer to a prop this component was passed.
   */
  useEffect(() => {
    if (!clearing || typeof ResizeObserver === "undefined") return;
    const box = scrollback.current;
    if (!box) return;
    const observer = new ResizeObserver(() => recomputeClearTail());
    observer.observe(box);
    return () => observer.disconnect();
  }, [clearing, recomputeClearTail]);

  const surface = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const root = surface.current;
    if (!root || !onFocus) return;
    // Opaque View frames can notify focus without a matching host blur event.
    const entered = (event: FocusEvent) => {
      if (!root.contains(event.target as Node | null)) onFocus(undefined);
    };
    root.ownerDocument.addEventListener("focusin", entered);
    return () => root.ownerDocument.removeEventListener("focusin", entered);
  }, [onFocus]);
  const focusAfterPin = useRef<string>();
  const jumped = useRef<number>();
  useLayoutEffect(() => {
    if (!jump || jump.pane !== definition?.pane || jumped.current === jump.revision) return;
    if (selection) { setSelection(undefined); return; }
    const cell = Array.from(surface.current?.querySelectorAll<HTMLElement>("[data-cell]") ?? []).find(cell => cell.dataset.cell === jump.cell);
    if (!cell) return;
    jumped.current = jump.revision;
    cell.scrollIntoView?.({ block: "center" });
    cell.focus({ preventScroll: true });
  }, [jump, definition?.pane, model.cells, selection]);
  useLayoutEffect(() => {
    if (!focusAfterPin.current) return;
    const id = focusAfterPin.current;
    focusAfterPin.current = undefined;
    const cells = surface.current?.querySelectorAll<HTMLElement>("[data-cell]");
    Array.from(cells ?? []).find(cell => cell.dataset.cell === id)?.focus({ preventScroll: true });
  });
  const cellActions = (cell: SessionCell): CellActions | undefined => {
    const offered = actions?.(cell);
    const { history: offeredHistory, ...rest }: CellActions = offered ?? {};
    const evidence = history === undefined || hasRunEvidence(cell, history.workspace);
    return { ...rest,
      copyPath:(node?:string)=>{const id=node ?? cell.nodes.at(-1)?.id;const found=history?.workspace.nodes.find(it=>it.id===id);if(id)void clipboard.copy(`$${found?.name || id}`);},
      ...(offered?.pin ? {pin:()=>{focusAfterPin.current=cell.id;offered.pin!();}} : {}),
      ...(history ? {
        open:(node?:string)=>openInspector(cell,node,"inspect"),
        json:(node?:string)=>openInspector(cell,node,"json"),
      } : {}),
      ...(evidence && history ? { history:(node?:string)=>openInspector(cell,node,"history") }
        : evidence && offeredHistory ? { history: offeredHistory } : {}),
    };
  };
  const drawCell = (cell: SessionCell) => (
    <Cell
      key={cell.id}
      theme={chrome}
      tailKeys={tailKeys}
      state={cell.state}
      streamOutput={cell.streamOutput}
      streamSource={cell.streamSource}
      outputIdentity={`${history?.generation ?? ""}:${cell.source ?? ""}`}
      pinned={cell.pinned}
      rows={cell.rows}
      {...(cell.time ? { time: cell.time, timestamp: cell.timestamp } : {})}
      chars={cell.chars}
      verdict={cell.verdict}
      marks={cell.marks}
      actions={cellActions(cell)}
      confirmRepeat={cell.guard}
      pipeline={cell.pipeline}
      runActive={cell.runActive}
      view={cell.view}
      blocks={output?.(cell) ?? []}
      onFocus={() => onFocus?.(cell.id)}
      label={cell.id}
      attempt={cell.attempt}
    />
  );

  /*
   * The "cleared" marker sits between two cells in the same flow they already draw in — never a
   * wrapper around a cell — so a cell's own key and place in the list never change because a clear
   * did or did not land next to it, and none of them remount for it.
   */
  const clearMarker = (revision: number) => (
    <div key={`session-clear-${revision}`} ref={clearMark} className="session-clear-mark">
      <span>cleared</span>
    </div>
  );
  const unpinnedCells = model.cells.filter(cell => !cell.pinned);
  // The boundary belongs to chronology, not to pin placement. If its cell moves to the
  // pinned strip, place the marker after the last remaining unpinned predecessor.
  const cutoff = clearRequest?.after === undefined ? -1 : model.cells.findIndex(cell => cell.id === clearRequest.after);
  const boundary = model.cells.slice(0, cutoff + 1).filter(cell => !cell.pinned).at(-1)?.id;
  const scrollbackChildren: ReactNode[] = [];
  if (clearRequest !== undefined && boundary === undefined) {
    scrollbackChildren.push(clearMarker(clearRequest.revision));
  }
  for (const cell of unpinnedCells) {
    scrollbackChildren.push(drawCell(cell));
    if (clearRequest !== undefined && boundary === cell.id) {
      scrollbackChildren.push(clearMarker(clearRequest.revision));
    }
  }

  return (
    <div className="session-layout" ref={layout}>
    <div ref={surface} aria-hidden={selection && overlay ? true : undefined} className={chromeMode === "pane" ? "session session-in-pane" : "session"}
      onFocus={event=>{if(!(event.target as HTMLElement).closest("[data-cell]"))onFocus?.(undefined);}}
      onBlur={event=>{if(!event.currentTarget.contains(event.relatedTarget as Node|null))onFocus?.(undefined);}}
>
      {chromeMode === "full" && (
        <div className="session-top surface-sunk">
          <MonoLine segments={model.top} className="session-top-line" />
        </div>
      )}

      {model.cells.some(cell => cell.pinned) && (
        <div className="session-pinned" role="region" aria-label="Pinned cells">
          {model.cells.filter(cell => cell.pinned).map(drawCell)}
        </div>
      )}

      <div className="session-scrollback" role="log" aria-label="Scrollback" ref={scrollback}>
        {scrollbackChildren}
        {/* Present only while a clear is live: nothing to measure, and nothing to keep blank, otherwise. */}
        {clearing && <div ref={clearTail} className="session-clear-tail" aria-hidden="true" />}
      </div>

      <div className="session-prompt surface-sunk">
        {prompt}
        {clipboard.notice && <p className="mono-dim" role="status">{clipboard.notice}</p>}
        <MonoLine segments={model.context} className="session-context" />
      </div>

      {chromeMode === "full" && (
        <div className="session-footer">
          {model.note && <MonoLine segments={model.note} className="session-note" />}
          <MonoLine segments={model.counts} className="session-counts" />
          <MonoLine segments={model.keys} className="session-keys" />
        </div>
      )}
    </div>
    {history && <div className={`inspector-stack${overlay ? " inspector-overlay" : ""}`} hidden={!selection}
      style={{["--inspector-width" as string]:`${Math.min(paneWidth,Math.max(32*monoAdvance(),columns*monoAdvance()-40*monoAdvance()))}px`}}>
      {!overlay && <div className="inspector-resize" role="separator" aria-label="Resize inspector" tabIndex={0} onKeyDown={event=>{if(event.key==="ArrowLeft"||event.key==="ArrowRight"){event.preventDefault();setPaneWidth(was=>Math.max(32*monoAdvance(),was+(event.key==="ArrowLeft" ? 24 : -24)));}}} onPointerDown={event=>{
        event.preventDefault();const start=event.clientX, initial=paneWidth,target=event.currentTarget;
        target.setPointerCapture(event.pointerId);
        const move=(e:PointerEvent)=>setPaneWidth(Math.max(32*monoAdvance(),initial+start-e.clientX));
        const end=()=>{target.removeEventListener("pointermove",move);target.removeEventListener("pointerup",end);target.removeEventListener("pointercancel",end);};
        target.addEventListener("pointermove",move);target.addEventListener("pointerup",end);target.addEventListener("pointercancel",end);
      }}/>}
      {[...hosts].map(([key,chosen])=>{
        const cell=model.cells.find(it=>it.id===chosen.cell);if(!cell)return null;
        return <Inspector key={`${history.generation}:${key}`} {...history} cell={cell} selection={chosen} active={selection!==undefined && hostKey(selection)===key} overlay={overlay}
          onClose={closeInspector} onWindow={()=>setWindowed(was=>!was)} onTab={tab=>{const next={...chosen,tab};setSelection(next);setHosts(was=>new Map(was).set(key,next));}}/>;
      })}
    </div>}
    </div>
  );
}
