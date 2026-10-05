/**
 * A second session, in a pane of its own.
 *
 * Any pane can contain a session or a screen, and `/split right` on its own is
 * what somebody means by "a plain terminal on the right". So this is a session: its own scrollback,
 * its own prompt, its own idea of where it has scrolled to — over the *same* workspace, because the
 * workspace is the engine's and there is only one of it.
 *
 * Its own scrollback is the point. Two terminals onto one workspace are two places to work, and a
 * second one that echoed the first's commands would be a mirror rather than a terminal. So a pane
 * claims only the attempts it sent: the session's own pane keeps taking whatever the engine replays
 * — which is how a reload still finds its cells — and a pane opened later starts empty and stays
 * its own.
 *
 * Everything that is the workspace's rather than this pane's goes up: `/graph`, `/settings`,
 * `/split` and the rest are answered by `SurfaceApp`, which is where screens and panes live.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { newCell, NEXT_VIEW, arrangeResult, resultArrangement, planned, retireCells, repeating, running, submissionFailed, refusedBeforeAdmission, type Cell as ClientCell } from "../cells";
import { expandAlias } from "../aliases";
import type { Engine } from "../engine";
import type { Event, StoredValue } from "../protocol";
import { referables, type Workspace } from "../workspace";
import type { Settings } from "../settings";
import { useInteractiveStates } from "./interactive-state";
import { cellBlocks } from "./cell-output";
import { Cell, type CellActions, type Theme } from "./Cell";
import { Prompt } from "./Prompt";
import type { ResultRead } from "./results";
import { Session } from "./Session";
import { useSessionCommands } from "./session-commands";
import { DraftNotice } from "./DraftNotice";
import type { Environments } from "../context";
import { DebugReport } from "./DebugReport";
import type { Language } from "./language";
import { readSession, type SessionCell, type SessionContext } from "./session-model";
import type { DefinitionJump, RegisterDefinition } from "./definition-target";

export interface SessionPaneProps {
  readonly definition?: { readonly pane: string; readonly workspace?: string; readonly register: RegisterDefinition };
  readonly jump?: DefinitionJump;
  readonly language?: Language;
  readonly compositionScope: string;
  readonly environments?: Environments;
  readonly focused?: boolean;
  readonly engine: Engine;
  readonly workspace: Workspace;
  readonly context: SessionContext;
  readonly settings: Settings;
  readonly chrome: Theme;
  readonly held: ReadonlyMap<string, StoredValue>;
  readonly reads: ReadonlyMap<string, ResultRead>;
  readonly retryRead: (handle: string) => void;
  readonly generation: string | undefined;
  /** The engine's events, fanned out from the one subscription the client keeps. */
  readonly subscribe: (listen: (event: Event) => void) => () => void;
  /**
   * A `/` command this pane cannot answer for itself.
   *
   * Screens and panes belong to the workspace, not to one terminal in it, so they are handed up.
   * Answered `true` when the workspace took it, and this pane clears its prompt.
   */
  readonly onCommand: (text: string) => boolean;
  readonly onTrouble: (said: string | undefined) => void;
  readonly onEditDocument?: (cell: ClientCell) => void;
  /**
   * Says that this pane owns an attempt, so the session's own pane leaves it alone.
   *
   * The session's pane takes every attempt nobody has claimed — that is how a reload still finds
   * its cells, since the engine replays what it knows and no pane was there to send it. A pane
   * opened later has to say which are its own, or its commands would appear in both places.
   */
  readonly claim: (attempt: string) => void;
  /** Mod+Shift+Enter returns the editor draft to this session, with its context. */
  readonly onGrow: (text: string, receive: (text: string) => void) => void;
}

export function SessionPane(props: SessionPaneProps) {
  const { engine, workspace, context, settings, chrome, held, reads, retryRead, generation, subscribe } = props;
  const interactiveStates = useInteractiveStates(workspace, generation);
  const [cells, setCells] = useState<readonly ClientCell[]>([]);
  const [draft, setDraft] = useState("");
  const [, contextChanged] = useState(0);
  const scope = props.compositionScope;
  const changeDraft = (text: string) => {
    engine.compose(text.trim().startsWith("/") ? "" : text, scope);
    setDraft(text);
  };
  const grow = (text: string) => { props.onGrow(text, setDraft); setDraft(""); };
  const [focused, setFocused] = useState<string>();
  const [sent, setSent] = useState(0);
  const latest = useRef(cells);
  latest.current = cells;
  /** The attempts sent or explicitly revealed here; these cells belong to this pane. */
  const mine = useRef(new Set<string>());
  useEffect(() => {
    const jump = props.jump;
    if (!jump?.source || jump.pane !== props.definition?.pane) return;
    const cell = jump.source;
    mine.current.add(cell.lastRun); props.claim(cell.lastRun);
    setCells(previous => previous.some(item => item.id === cell.id) ? previous : [...previous, cell]);
    setFocused(cell.id);
  }, [props.jump, props.definition?.pane]);

  useEffect(
    () =>
      subscribe((event) => {
        if (event.event === "work-retired") setCells(previous => retireCells(previous, event.cells));
        if (event.event === "session") {
          mine.current.clear();
          setCells([]);
          setFocused(undefined);
          return;
        }
        if (event.event === "planned" && mine.current.has(event.cell)) {
          setCells((previous) => planned(previous, event));
        }
        if (event.event === "reported" && event.cell !== "") {
          setCells((previous) =>
            previous.map((cell) => (cell.lastRun === event.cell ? { ...cell, diagnostics: event.diagnostics } : cell)),
          );
        }
        if (event.event === "created" && event.command !== "") {
          setCells((previous) =>
            previous.map((cell) =>
              cell.text === "" && cell.nodes.includes(event.node) ? { ...cell, text: event.command } : cell,
            ),
          );
        }
      }),
    [subscribe],
  );

  const change = useCallback((id: string, how: (cell: ClientCell) => ClientCell) => {
    setCells((previous) => previous.map((cell) => (cell.id === id ? how(cell) : cell)));
  }, []);

  const sessionCommands = useSessionCommands({ engine, workspace, settings, generation, cells,
    append: cell => { setCells(was => [...was, cell]); setSent(was => was + 1); setFocused(cell.id); },
    onTrouble: props.onTrouble });

  const submit = useCallback(
    (text: string) => {
      const written = text.trim();
      if (written === "") return;
      if (sessionCommands.answer(written)) { changeDraft(""); return; }
      if (written.startsWith("/")) {
        if (props.onCommand(written)) changeDraft("");
        return;
      }
      let expanded: string;
      try {
        expanded = expandAlias(text, settings.aliases, workspace.catalogue);
      } catch (error) {
        props.onTrouble((error as Error).message);
        return;
      }
      try { engine.checkComposition(scope); }
      catch (error) { props.onTrouble((error as Error).message); return; }
      props.onTrouble(undefined);
      const cell = newCell(expanded);
      mine.current.add(cell.lastRun);
      props.claim(cell.lastRun);
      setCells((previous) => [...previous, cell]);
      setSent((count) => count + 1);
      setFocused(cell.id);
      setDraft("");
      engine.submitComposed(cell.lastRun, cell.text, scope).catch((failure: Error) => {
        props.onTrouble(failure.message);
        change(cell.id, (current) => submissionFailed(current, cell.lastRun, failure));
      });
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [change, engine, settings.aliases, workspace.catalogue, sessionCommands.answer, scope],
  );

  /** A repeat is a new attempt at this pane's own cell, and this pane sent it. */
  const repeat = useCallback(
    (id: string, acknowledgeEffects: boolean) => {
      const was = latest.current.find((it) => it.id === id);
      if (!was || was.document) return;
      if (sessionCommands.answer(was.text)) return;
      // Nothing of a refused first submission was admitted, so its retry is the same source submitted anew.
      const fresh = refusedBeforeAdmission(was);
      const again = fresh ? running(was) : repeating(was, acknowledgeEffects);
      mine.current.add(again.lastRun);
      props.claim(again.lastRun);
      setCells((previous) => previous.map((cell) => (cell.id === id ? again : cell)));
      (fresh ? engine.submit(again.lastRun, again.text)
        : engine.rerun(again.lastRun, again.text, again.originAttempt ?? again.lastRun, acknowledgeEffects, undefined, again.document))
        .catch((failure: Error) => {
          props.onTrouble(failure.message);
          change(id, (current) => submissionFailed(current, again.lastRun, failure));
        });
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [engine, sessionCommands.answer],
  );

  const step = (by: 1 | -1) => {
    const all = latest.current;
    if (all.length === 0) return;
    const at = all.findIndex((cell) => cell.id === focused);
    setFocused(all[at < 0 ? (by === 1 ? 0 : all.length - 1) : (at + by + all.length) % all.length]!.id);
  };


  const actionsFor = (cell: SessionCell): CellActions => ({
    ...(cell.source?.trimStart().startsWith("/") ? {} : { deleteWork: {
      preview: () => engine.previewDeleteWork(latest.current.find(it => it.id === cell.id)?.lastRun ?? cell.id),
      confirm: async (token: string, additionalWork: boolean, protectedContent: boolean) => {
        try { await engine.deleteWork(token, additionalWork, protectedContent); }
        catch (failure) { props.onTrouble((failure as Error).message); throw failure; }
      },
    } }),
    ...(latest.current.find(it => it.id === cell.id)?.document ? {} : { repeat: (acknowledgeEffects: boolean) => repeat(cell.id, acknowledgeEffects) }),
    pin: () => change(cell.id, (it) => ({ ...it, pinned: !it.pinned })),
    cycle: node => change(cell.id, it => arrangeResult(it, node, { view: NEXT_VIEW[resultArrangement(it, node ?? it.nodes.at(-1)).view] })),
      setView: (view, node) => change(cell.id, it => arrangeResult(it, node, { view })),
      setHeight: (rows, node) => change(cell.id, it => arrangeResult(it, node, { rows: rows ?? null })),
    edit: () => {
      const selected = latest.current.find((it) => it.id === cell.id);
      if (selected?.document) { props.onEditDocument?.(selected); return; }
      const text = selected?.text ?? "";
      engine.compose("", scope); engine.compose(text, scope);
      grow(text);
    },
    copy: () => { void navigator.clipboard?.writeText(cell.source ?? "").catch(() => undefined); },
    cancel: () => {
      for (const node of latest.current.find((it) => it.id === cell.id)?.nodes ?? []) {
        engine.cancel(node).catch(() => undefined);
      }
    },
    next: () => step(1),
    previous: () => step(-1),
  });

  const model = useMemo(
    () => readSession({ workspace, cells, context, focused, held, language: props.language, following: settings.stayAtNewest }, new Date()),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [workspace, cells, focused, held, props.language, settings.stayAtNewest],
  );
  const names = useMemo(() => referables(workspace.nodes), [workspace.nodes]);

  return (<>
    <Session
      definition={props.definition} jump={props.jump}
      history={{ engine, workspace, generation }}
      chromeMode="pane"
      model={model}
      chrome={chrome}
      tailKeys={settings.surfaceTailKeys === "shown"}
      prompt={<>
        <Prompt
          dashboards={(settings.dashboards??[]).filter(entry=>entry.workspace===workspace.identity?.name).map(entry=>entry.board.name)}
          language={props.language}
          autoFocus={props.focused ?? true}
          draft={draft}
          history={cells.map(cell => cell.text)}
          historyScope={generation}
          onDraft={changeDraft}
          onSubmit={submit}
          chromeName={chrome}
          onChrome={() => undefined}
          catalogue={workspace.catalogue}
          names={names}
          variables={workspace.nodes.flatMap(node => node.name ? [node.name] : [])}
          aliases={settings.aliases}
          workspaces={workspace.identity?.saved}
          onGrow={grow}
        />
        <DraftNotice engine={engine} scope={scope} environments={props.environments}
          onReview={() => contextChanged(n => n + 1)} onTrouble={props.onTrouble} />
      </>}
      actions={actionsFor}
      output={(cell) => {
        const diagnostic = sessionCommands.reportFor(cell.id);
        return diagnostic === undefined ? cellBlocks({
          interactiveStates, cell, workspace, held, reads, retryRead, engine, generation,
        }) : [{ key: "debug", open: true, content: <DebugReport report={diagnostic} /> }];
      }}
      onFocus={setFocused}
      following={settings.stayAtNewest}
      pinned={sent}
      clearRequest={sessionCommands.clearRequest}
    />
  </>);
}

/** Kept so the unused-import checker sees the cell is the session's, not this file's. */
void Cell;
