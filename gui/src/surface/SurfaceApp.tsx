import { reportApplicationProblem } from "../application-log";
import { roleStyles } from "@wes/view-sdk/theme";
import { SpecScreen } from "./screens/Spec";
import {DashboardHost,DashboardUnavailable} from '../dashboard/DashboardHost';
import {saveDashboard} from '../dashboard/storage';
import type {Dashboard} from '../dashboard/model';
/**
 * The terminal surface, over the engine.
 *
 * This is the surface as the client's default: the session's scrollback of real cells, the prompt
 * that grows into the editor, the four screens summoned by name, and panes. Everything it draws
 * comes from what the engine said — `session-model.ts` does the deciding, this does the wiring.
 *
 * One engine subscription supplies every session and pane.
 */
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, } from "react";
import { flushDesktopPreferences } from "../desktop-preferences";
import { SaveStatus } from "./SaveStatus";
import { newCell, NEXT_VIEW, arrangeResult, resultArrangement, planned, reconcileCells, retireCells, repeating, running, askingAgain, submissionFailed, refusedBeforeAdmission, type Cell as ClientCell } from "../cells";
import { expandAlias } from "../aliases";
import { run as runClientCommand } from "../commands";
import { Engine, type Connection } from "../engine";
import { apply, emptyWorkspace, referables, type Workspace } from "../workspace";
import { readGraph } from "./graph-model";
import { load, resolveSurfacePalette, save, type Settings, surfaceTypeStyle } from "../settings";
import { read, type EditFileContext, type Summoned } from "./commands";
import type { PeekWhat } from "./peek";
import { peekOf, PeekScreen } from "./screens/Peek";
import { Hints } from "./Hints";
import { returnFocusToPane } from "./pane-focus";
import type { TerminalTarget } from "../terminal-target";
import { ShellTerminal } from "../ShellTerminal";
import { PaneEditor } from "./PaneEditor";
import { paneTransitions } from "./pane-transitions";
import { acceptTerminalTarget, allTerminals, canOpenTerminalTab, canSplitTerminal, requireTerminalPlacement, requireTerminalTab, terminalDirectory, terminalPlacement, terminalTabs, type TerminalPlacement } from "./terminal-tabs";
import { applyPaneCommand } from "./pane-command";
import { PaneCommand } from "./PaneCommand";
import { Split } from "./Split";
import { SessionPane } from "./SessionPane";
import { ComponentPane } from "./ComponentPane";
import { sharedCellEvent } from "./component-model";
import {
  clearPane, focus, oneP, SESSION_PANE, sendToPane, max_panes, paneFor, titleOf, type Pane, type Shown, type SplitState,
} from "./split-model";
import { previewOf, readSection, SECTIONS, chose, sectionNamed, type SectionName } from "./settings-model";
import { Cell, RepeatQuestion, type CellActions } from "./Cell";
import { useInteractiveStates } from "./interactive-state";
import { cellBlocks } from "./cell-output";
import type { CellBlock } from "./Cell";
import { language, type Language } from "./language";
import { MonoLine } from "./MonoLine";
import { Prompt } from "./Prompt";
import { Session } from "./Session";
import { useSessionCommands } from "./session-commands";
import { DebugReport } from "./DebugReport";
import { readSession, type SessionCell, type SessionContext } from "./session-model";
import { readOpen, resultNamed } from "./open-model";
import { openRoute, peekRoute, screenRoute, sourceRoute } from "./open-route";
import { followSettings } from "./settings-channel";
import { useTableViewOwner } from "./table-view-owner";
import { EnvScreen } from "./screens/Env";
import { GraphScreen } from "./screens/Graph";
import { GraphCanvas } from "./screens/GraphCanvas";
import { OpenScreen, type OpenTab } from "./screens/Open";
import { LiveView } from "./LiveView";
import { resultAccess } from "./result-access";
import { EditScreen } from "./screens/Edit";
import { EditFileScreen } from "./screens/EditFile";
import { documentCommand, documentOperation } from "./document-command";
import { ranAt, type Bound } from "./edit-model";
import { SettingsScreen } from "./screens/Settings";
import { readEnvironments } from "./environment-model";
import { useResults } from "./results";
import { ObservationStatus } from "./ObservationStatus";
import { ReadStatus } from "./ReadStatus";
import type { ViewSubject } from "./result-views";
import { DraftNotice } from "./DraftNotice";
import { announceWorkspaceOpen, openWorkspace } from "../workspace-binding";
import { WorkspaceSurfaces, type WorkspaceHost, type WorkspaceSurface } from "./workspace-surfaces";
import { workspaceViews, bindWorkspace, openWorkspaceTab, selectWorkspaceTab, openValueTab, activeView, paneTabs, closeViewTab, updateValueTab, viewKey } from "./workspace-tabs";
import { definitionTarget, type DefinitionOwner, type DefinitionJump, type RegisterDefinition } from "./definition-target";
import "./surface.css";
import { WorkspaceDeletion } from "./WorkspaceDeletion";
import { removeWorkspaceViews } from "./workspace-tabs";
import { SandboxPanel } from "./SandboxPanel";
import type { SandboxObservation } from "../engine";

export function SurfaceApp({ binding, host, renderWorkspace }: { readonly binding?: string; readonly host?: WorkspaceHost;
  readonly renderWorkspace?: (surface: WorkspaceSurface) => void } = {}) {
  const engine = useMemo(() => host && binding !== undefined ? host.engine(binding) : new Engine(binding), [binding]);
  const workspaceEngines = useRef(new Map<string, Engine>());
  const workspaceEngine = (name: string) => {
    let scoped = workspaceEngines.current.get(name);
    if (!scoped) { scoped = new Engine(name, engine.client); workspaceEngines.current.set(name, scoped); }
    return scoped;
  };
  const [workspace, setWorkspace] = useState<Workspace>(emptyWorkspace);
  const [cells, setCells] = useState<readonly ClientCell[]>([]);
  const [sandbox, setSandbox] = useState<SandboxObservation>();
  useEffect(() => engine.onSandbox((cell, result) => {
    setCells(previous => previous.filter(item => item.id !== cell));
    if (result.reference) setSandbox(result);
  }), [engine]);
  const [sharedCells, setSharedCells] = useState<readonly ClientCell[]>([]);
  const [connection, setConnection] = useState<Connection>("connecting");
  const [generation, setGeneration] = useState<string>();
  useEffect(() => { setSandbox(undefined); }, [generation]);
  const [environments, setEnvironments] = useState<Extract<import("../protocol").Event, { event: "environments" }>>();
  const [localSettings, setLocalSettings] = useState<Settings>(load);
  const settings = host?.settings ?? localSettings;
  const setSettings = host?.settingsChange ?? setLocalSettings;
  const settingsLatest=useRef(settings);settingsLatest.current=settings;
  const dashboardWorkspace=workspace.identity?.name??binding;
  const dashboards=(settings.dashboards??[]).filter(entry=>entry.workspace===dashboardWorkspace);
  const [dashboardTarget,setDashboardTarget]=useState<string>();
  const saveBoard=(board:Dashboard)=>{
    if(!dashboardWorkspace||!generationRef.current)throw new Error('Wait for the workspace connection before saving a dashboard.');
    const latest=settingsLatest.current;
    const next={...latest,dashboards:saveDashboard(latest.dashboards??[],dashboardWorkspace,board,workspace.nodes.flatMap(node=>node.name?[node.name]:[]))};
    settingsLatest.current=next;setSettings(next);
  };
  const removeBoard=(board:Dashboard)=>{
    const latest=settingsLatest.current,existing=latest.dashboards?.find(entry=>entry.workspace===dashboardWorkspace&&entry.board.id===board.id);
    if(!existing||existing.board.revision!==board.revision)throw new Error('This dashboard changed. Reopen it before removing the layout.');
    const next={...latest,dashboards:latest.dashboards?.filter(entry=>entry!==existing)};settingsLatest.current=next;setSettings(next);
  };
  const [draft, setDraft] = useState("");
  const [focused, setFocused] = useState<string>();
  const definitionOwners = useRef(new Map<string, DefinitionOwner>());
  const registerDefinition = useCallback<RegisterDefinition>(owner => {
    const key = JSON.stringify([owner.pane, owner.workspace]);
    definitionOwners.current.set(key, owner);
    return () => { if (definitionOwners.current.get(key) === owner) definitionOwners.current.delete(key); };
  }, []);
  const [definitionJump, setDefinitionJump] = useState<DefinitionJump>();
  const jumpRevision = useRef(0);
  const [screen, setScreenState] = useState<Summoned>();
  const [screenPane, setScreenPane] = useState<string>();
  const setScreen = (next: Summoned | undefined) => { setScreenPane(splitRef.current.focused); setScreenState(next); };
  /** Which section `/settings` is showing. `/settings connections` opens that one. */
  const [section, setSection] = useState<SectionName>("appearance");
  /** The node whose panel `/graph` is showing. The newest node when nobody has chosen one. */
  const [chosenNode, setChosenNode] = useState<string>();
  const [connectedOnly, setConnectedOnly] = useState(false);
  const [tab, setTab] = useState<OpenTab>("result");
  const [peek, setPeek] = useState<PeekWhat>("value");
  const [sourceCell, setSourceCell] = useState<string>();
  const setTrouble = useCallback((problem?: unknown) => {
    reportApplicationProblem(problem, { workspace: contextRef.current?.workspace ?? binding ?? "Current workspace",
      generation: generationRef.current, pane: splitRef.current.focused });
  }, [binding]);
  const [aliasNotice, setAliasNotice] = useState<string>();
  /*
   * How many commands this session has sent, as the scrollback's cue to jump to the bottom.
   *
   * Pressing ⏎ is itself a request to see the answer: somebody who scrolled up to read an old cell
   * and typed there means to watch what they just ran, not to stay where they were reading. That is
   * `following.ts`'s `pinned`, and this is what says it happened — a count rather than a flag, so
   * two commands in a row are two cues.
   */
  const [sent, setSent] = useState(0);
  /** Which node `/open` is showing, when it is showing one. */
  const [opened, setOpened] = useState<string>();
  /**
   * What `/edit` is holding, and what it is bound to.
   *
   * The draft outlives the screen: `esc` goes back to the session and the editor reopens on what
   * was written, which is what the head promises.
   *
   * The binding is a *cell*, not a node. A run that the engine refused made no node but did make a
   * cell, and that cell is what carries the refusal, what `⌘R` repeats and what the scrollback
   * shows — so binding to the node would leave the pane saying "not run yet" about a command that
   * had very much run. The head names the node when there is one, because that is what a person
   * calls a result.
   */
  const [deletingWorkspace,setDeletingWorkspace]=useState(false);
  const [workspaceClosed,setWorkspaceClosed]=useState(false);
  const [draftProgram, setDraftProgram] = useState("");
  const [bound, setBound] = useState<string>();
  /** `/edit env|types [name]`, over the workspace rather than in a pane. */
  const [fileContext, setFileContext] = useState<EditFileContext>();
  const [fileName, setFileName] = useState<string>();
  const [fileDraft, setFileDraft] = useState<{ id: string; source: string; base: string; origin: string }>();
  const [pack, setPack] = useState<Language>();
  const { held, reads, observations, retry: retryRead } = useResults(engine, generation, workspace.nodes.flatMap(node => (!node.streamOutput || node.evidence) && node.handle ? [node.handle] : []), workspace.nodes.filter(node => !node.streamOutput || node.evidence));
  const [repeatAsked, setRepeatAsked] = useState<{ id: string; attempt: string }>();
  /**
   * How the workspace is divided, and what each pane holds.
   *
   * One pane until somebody says otherwise, and that pane is the session's — `/close` can never
   * take it away, because a workspace with no panes is not a workspace.
   */
  const [localSplit, setSplitState] = useState<SplitState>(() => settings.paneLayout ?? oneP(SESSION_PANE));
  const split = host?.split ?? localSplit;
  const sessionPane = binding === undefined ? SESSION_PANE.id
    : workspaceViews(split).find(p => p.workspace === binding && !p.terminal && !p.value && !p.shows)?.id ?? `workspace:${binding}`;
  const [retainedWorkspaces, setRetainedWorkspaces] = useState<readonly string[]>(() =>
    [...new Set(workspaceViews(split).flatMap(p => p.workspace === undefined ? [] : [p.workspace]))]);
  useEffect(() => {
    if (host) return;
    setRetainedWorkspaces(was => {
      const added = workspaceViews(split).flatMap(p => p.workspace === undefined || was.includes(p.workspace) ? [] : [p.workspace]);
      return added.length ? [...was, ...new Set(added)] : was;
    });
  }, [split, host]);
  const splitRef = useRef(split);
  splitRef.current = split;
  const generationRef = useRef(generation); generationRef.current = generation;
  const settingsRef = useRef(settings); settingsRef.current = settings;
  const transitions = useMemo(() => paneTransitions({
    read: () => splitRef.current,
    commit: async next => {
      splitRef.current = next; setSplitState(next);
      save({ ...settingsRef.current, paneLayout: next });
      await flushDesktopPreferences();
    },
    forget: history => {
      const pane = allTerminals(splitRef.current).find(p => p.history === history);
      return (pane?.workspace === undefined ? engine : workspaceEngine(pane.workspace)).terminal({ action: "forget", history });
    },
    generation: () => generationRef.current,
  }), [engine]);
  const layoutLifetime = useRef(0);
  useEffect(() => {
    transitions.resume();
    return () => { layoutLifetime.current += 1; transitions.dispose(); };
  }, [transitions]);
  const setSplit = (change: SplitState | ((state: SplitState) => SplitState)) => {
    void (host ? host.change(change) : transitions.change(change)).catch(error => setTrouble(error));
  };
  useEffect(()=>{
    if(host)return;
    const remove=(event: globalThis.Event)=>{
      const name=(event as CustomEvent<string>).detail;
      setRetainedWorkspaces(was=>was.filter(n=>n!==name)); workspaceEngines.current.delete(name);
      const next=removeWorkspaceViews(splitRef.current,name);splitRef.current=next;setSplitState(next);
      save({...settingsRef.current,paneLayout:next});void flushDesktopPreferences();
    };
    window.addEventListener?.("wes-workspace-closed",remove);return()=>window.removeEventListener?.("wes-workspace-closed",remove);
  },[host]);
  const prepareTerminal = async (history: string) => {
    const ownsHistory = () => allTerminals(splitRef.current).some(pane => pane.history === history);
    if (!ownsHistory()) throw new Error("The terminal pane has closed.");
    save({ ...settingsRef.current, paneLayout: splitRef.current });
    await flushDesktopPreferences();
    if (!ownsHistory()) throw new Error("The terminal pane has closed.");
  };
  useEffect(() => { if (!host) setSettings(was => was.paneLayout === split ? was : { ...was, paneLayout: split }); }, [split, host]);
  /**
   * The engine's events, fanned out to the panes.
   *
   * One subscription for the client, not one per pane: `engine.listen` opens an event stream, and
   * four panes opening four of them would be four clients where the person is one.
   */
  const paneListeners = useRef(new Set<(event: import("../protocol").Event) => void>());
  /**
   * Attempts another pane owns.
   *
   * The session's own pane takes every attempt nobody has claimed, which is how a reload still
   * finds its cells: the engine replays what it knows and no pane was there to send it. So a pane
   * opened later says which are its own, and the session's pane leaves those where they were run.
   */
  const claimed = useRef(new Set<string>());
  const subscribe = useCallback((listen: (event: import("../protocol").Event) => void) => {
    paneListeners.current.add(listen);
    return () => { paneListeners.current.delete(listen); };
  }, []);
  const [, contextChanged] = useState(0);
  const environmentAttempt = useRef<string>();
  const modelRef = useRef<ReturnType<typeof readSession>>();
  const contextRef = useRef<SessionContext>();
  const focusRef = useRef(focused);
  focusRef.current = focused;
  const changeDraft = (text: string, scope: "prompt" | "editor") => {
    engine.compose(text.trim().startsWith("/") ? "" : text, scope);
    if (scope === "prompt") setDraft(text); else setDraftProgram(text);
  };
  const editorReturn = useRef<{ scope: string; pane: string; receive: (text: string) => void }>();
  const returnEditorDraft = () => {
    const target = editorReturn.current;
    if (target && splitRef.current.panes.some(pane => pane.id === target.pane && !pane.shows && !pane.terminal)) {
      engine.copyComposition("editor", target.scope);
      target.receive(draftProgram);
      setSplit(was => focus(was, target.pane));
    } else {
      engine.copyComposition("editor", "prompt");
      setDraft(draftProgram);
    }
  };
  const openEditor = (text: string, cell?: string) => {
    editorReturn.current = undefined;
    setFileContext(undefined); setFileName(undefined);
    engine.compose("", "editor");
    engine.compose(text, "editor");
    setDraftProgram(text); setBound(cell); setScreen("edit");
  };
  const openDocument = (cell: ClientCell) => {
    const operation = documentOperation(cell.text);
    if (!cell.document || !operation) { setTrouble("This document command cannot be reopened safely."); return; }
    editorReturn.current = undefined;
    setFileContext(operation.context); setFileName(undefined);
    setFileDraft({ id: `${cell.lastRun}:${crypto.randomUUID()}`, source: cell.document.source, base: operation.base, origin: operation.origin });
    setScreen("edit");
  };
  const latest = useRef(cells);
  latest.current = cells;
  /* Read through refs so the cell actions are not rebuilt — and every cell redrawn — on a setting. */
  const openWhere = useRef(settings.openIn);
  openWhere.current = settings.openIn;
  /** The result the caret's cell made, so `/open` with no argument opens what `o` would have. */
  const focusedNode = useRef<string>();
  focusedNode.current = cells.find((it) => it.id === focused)?.nodes.at(-1);

  useEffect(() => { void language().then(setPack); }, []);
  // A settings window announces its choices; the session applies and persists them (it owns the layout).
  useEffect(() => (host ? undefined : followSettings((change) => setLocalSettings((was) => ({ ...was, ...change })))), [host]);
  useEffect(() => { if (!host) save({ ...settings, paneLayout: splitRef.current }); }, [settings, split, host]);
  useTableViewOwner(settings.tables, (tables) => setLocalSettings((was) => ({ ...was, tables })), !host);

  /*
   * `system` is not a third palette: it is paper or ink, whichever the machine is set to, and it
   * changes while the client is open. Resolved in one place so `data-palette` always carries an
   * answer rather than leaving half the tokens to a media query.
   */
  const [systemPalette, setSystemPalette] = useState(() => resolveSurfacePalette("system"));
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const dark = window.matchMedia("(prefers-color-scheme: dark)");
    const follow = () => setSystemPalette(dark.matches ? "ink" : "paper");
    follow();
    dark.addEventListener("change", follow);
    return () => dark.removeEventListener("change", follow);
  }, []);
  const palette = settings.surfacePalette === "system" ? systemPalette : settings.surfacePalette;

  useEffect(() => {
    let seen: string | undefined;
    return engine.listen(
      (event) => {
        setSharedCells(previous => sharedCellEvent(previous, event));
        if (event.event === "work-retired") setCells(previous => retireCells(previous, event.cells));
        if (event.event === "workspace-closed") {setWorkspaceClosed(true);setGeneration(undefined);setDeletingWorkspace(false);setCells([]);}
        if (event.event === "session") {
          setWorkspaceClosed(false);
          setGeneration(event.generation);
          setEnvironments(undefined);
          if (seen !== undefined && seen !== event.generation) {
            setCells([]); setDefinitionJump(undefined); setOpened(undefined); setChosenNode(undefined); setBound(undefined); setFocused(undefined); setRepeatAsked(undefined);
          }
          else setCells((previous) => reconcileCells(previous, event.cells).map((cell) => ({ ...cell, nodes: [], diagnostics: [] })));
          seen = event.generation;
        }
        if (event.event === "environments") setEnvironments(event);
        if (event.event === "planned" && event.cell === environmentAttempt.current && event.failure) setTrouble(event.failure);
        if (event.event === "reported" && event.cell === environmentAttempt.current) {
          const problem = event.diagnostics.find(diagnostic => diagnostic.severity === "error");
          if (problem) setTrouble(`${problem.code}: ${problem.message}`);
        }
        setWorkspace((previous) => apply(previous, event));
        if (event.event === "planned" && !claimed.current.has(event.cell)) {
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
        for (const listen of paneListeners.current) listen(event);
      },
      (reason) => setTrouble(reason),
      setConnection,
    );
  }, [engine]);

  const change = useCallback((id: string, how: (cell: ClientCell) => ClientCell) => {
    setCells((previous) => previous.map((cell) => (cell.id === id ? how(cell) : cell)));
  }, []);

  const sessionCommands = useSessionCommands({ engine, workspace, settings, generation, cells,
    append: cell => { setCells(was => [...was, cell]); setSent(was => was + 1); setFocused(cell.id); },
    onTrouble: setTrouble });

  /**
   * Runs a cell's captured definition again, which is what `r` on it does and what `⌘R` does.
   *
   * One path on purpose: a repeat from the editor must meet the same guard and carry the same
   * effects mark as a repeat from the scrollback, because it is the same act on the same node.
   */
  const repeatCell = useCallback(
    (id: string, acknowledgeEffects: boolean) => {
      const mine = latest.current.find((it) => it.id === id);
      if (!mine || mine.document) return;
      if (sessionCommands.answer(mine.text)) return;
      if (mine.state === "running") { setTrouble("Wait for the submission acknowledgement before running again."); return; }
      const guard = modelRef.current?.cells.find(cell => cell.id === id)?.guard;
      if (mine.state !== "unanswered" && guard && !acknowledgeEffects) {
        setRepeatAsked({ id, attempt: mine.lastRun }); return;
      }
      // Nothing of a refused first submission was admitted, so its retry is the same source submitted anew.
      const fresh = refusedBeforeAdmission(mine);
      const again = mine.state === "unanswered" ? askingAgain(mine) : fresh ? running(mine) : repeating(mine, acknowledgeEffects);
      latest.current = latest.current.map(cell => cell.id === id ? again : cell);
      setCells(previous => previous.map(cell => cell.id === id ? again : cell));
      setTrouble(undefined);
      const request = mine.state === "unanswered"
        ? engine.retrySubmission(mine.lastRun)
        : fresh ? engine.submit(again.lastRun, again.text)
        : engine.rerun(again.lastRun, again.text, again.originAttempt ?? again.lastRun, acknowledgeEffects, undefined, again.document);
      request.catch((failure: Error) => {
        setTrouble(failure);
        change(id, current => submissionFailed(current, again.lastRun, failure));
      });
    },
    [engine, change, sessionCommands.answer],
  );

  /** Which result a cell opened: the last node it made, which is the one its preview showed. */
  const nodeOf = (id: string) => {
    const mine = latest.current.find((it) => it.id === id);
    return mine?.nodes[mine.nodes.length - 1];
  };

  /*
   * Opening a result, the way the setting says.
   *
   * A window is a second document — a real window in the desktop app, a second tab in a browser —
   * so the session is not navigated, not re-rendered and not left behind a screen: it keeps its
   * scroll position and its half-typed line, which is the point of opening a result elsewhere.
   * Without a node there is nothing to open in a window, so it falls back to the screen rather
   * than opening an empty one.
   */
  const openResult = useCallback(
    (node: string | undefined, which: OpenTab) => {
      // A cell that made no result has nothing to open, and says so where the other troubles are.
      if (node === undefined) return setTrouble("that cell made no result to open");
      if (openWhere.current === "window") {
        /*
         * A browser hands back the window it opened and `null` when it refused, so a refusal can
         * fall back to the screen. The desktop shell answers `window.open` by building a real
         * window (`crates/desktop`), and what the webview hands back is not worth trusting — so
         * there, the ask itself is the answer.
         */
        const elsewhere = window.open(openRoute(node, which, engine.binding ?? contextRef.current?.workspace), "_blank");
        if (elsewhere || window.__WES_DESKTOP__ === true) return setOpened(node);
      }
      setOpened(node);
      setTab(which);
      setScreen("open");
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  /* One piece of a result, where `/open` would go: a plain window, or the plain screen. */
  const openPeek = useCallback(
    (node: string | undefined, what: PeekWhat) => {
      if (node === undefined) return setTrouble("that cell made no result to open");
      setSourceCell(undefined);
      if (openWhere.current === "window") {
        const elsewhere = window.open(peekRoute(node, what, engine.binding ?? contextRef.current?.workspace), "_blank");
        if (elsewhere || window.__WES_DESKTOP__ === true) return setOpened(node);
      }
      setOpened(node);
      setPeek(what);
      setScreen("peek");
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  const openCellSource = (cell: SessionCell) => {
    // Local UI commands have no engine ledger entry to restore in another window.
    if (openWhere.current === "window" && !cell.source?.trimStart().startsWith("/")) {
      const elsewhere = window.open(sourceRoute(cell.id, engine.binding ?? contextRef.current?.workspace), "_blank");
      if (elsewhere || window.__WES_DESKTOP__ === true) return;
    }
    setSourceCell(cell.id);
    setPeek("source");
    setScreen("peek");
  };

  const openTab = async (name: string, pane: string, activate = false, create = true) => {
    if (create && workspace.nodes.some(node => node.name === name) && name !== contextRef.current?.workspace &&
      !workspace.identity?.saved.includes(name) && !workspaceViews(splitRef.current).some(view => view.workspace === name))
      throw new Error(`$${name} is a variable in this workspace. Use /tab $${name} for its value; bare names open workspaces.`);
    const original = splitRef.current.panes.find(p => p.id === pane);
    const origin = host?.originWorkspace ?? contextRef.current?.workspace;
    if (name === origin && original && !original.terminal && !original.shows &&
      (original.tabs ?? [{ workspace: original.workspace }]).some(tab => tab.workspace === undefined)) {
      if (activate) await (host ? host.change : transitions.change)(previous => selectWorkspaceTab(previous, pane, undefined));
      return; // The implicit original workspace is already open; do not duplicate its controller.
    }
    openWorkspaceTab(splitRef.current, pane, name, activate); // Validate target/capacity before opening.
    const owner = layoutLifetime.current, session = generationRef.current;
    const identity = await openWorkspace(name, create);
    if (owner !== layoutLifetime.current || (session !== undefined && session !== generationRef.current)) throw new Error("Workspace changed while opening the tab.");
    await (host ? host.change : transitions.change)(previous => {
      const target = previous.panes.find(p => p.id === pane);
      if (!target || target !== original) throw new Error("Target pane changed; inspect layout before opening the tab again.");
      return bindWorkspace(openWorkspaceTab(previous, pane, name, activate), name, identity);
    });
    announceWorkspaceOpen(name);
  };
  const uiLayout = useRef({
    read: () => ({} as unknown),
    open: async (_request: { workspace: string; pane: string; activate: boolean }) => ({} as unknown),
  });
  uiLayout.current = {
    read: () => ({ focused: splitRef.current.focused, panes: splitRef.current.panes.map(p => ({
      id: p.id, kind: p.terminal ? "terminal" : p.value?.related ? "related" : p.value ? "value" : p.shows ? "screen" : "session",
      workspace: p.workspace ?? host?.originWorkspace ?? contextRef.current?.workspace,
      tabs: p.terminal ? terminalTabs(p).map(t => ({ id: t.history, kind: "terminal", active: t.history === p.history })) : paneTabs(p).map(t => ({ workspace: t.workspace ?? host?.originWorkspace ?? contextRef.current?.workspace, ...(t.value ? { node: t.value.node, kind: t.value.related ? "related" : "value" } : { kind: "session" }), active: viewKey(t) === activeView(p) })),
    })) }),
    open: async request => { await openTab(request.workspace, request.pane, request.activate, false); return uiLayout.current.read(); },
  };
  useEffect(() => engine.assistantUi.attachLayout({ read: () => uiLayout.current.read(), open: request => uiLayout.current.open(request) }), [engine]);
  const paneCommand = async (text: string, source: string, terminal = false, commandEnvironment?: string | null, sourceHistory?: string) => {
    const typed = read(text);
    if (terminal) requireTerminalTab(splitRef.current, source, sourceHistory);
    if (typed.kind === "workspace-tab") return openTab(typed.workspace, typed.pane ?? source, typed.activate);
    const valueBinding = (name: string, command: "/tab" | "/split") => {
      const board=dashboards.find(entry=>entry.board.name===name)?.board;
      if(board){
        if(workspace.nodes.some(node=>node.name===name))throw new Error(`$${name} names both a result and a dashboard. Rename the dashboard with /dashboard $${name}.`);
        return {node:board.id,generation:'ui-dashboard',label:board.name,dashboard:true as const};
      }
      const generation = generationRef.current;
      if (!generation) throw new Error("Wait for the workspace connection before opening a value.");
      const named = resultNamed(workspace, name, command);
      if (named.trouble) throw new Error(named.trouble);
      const node = workspace.nodes.find(node => node.id === named.node)!;
      return { node: node.id, generation, label: node.name ?? node.id };
    };
    if (typed.kind === "value-tab") {
      const sourceGeneration=generationRef.current;
      const target = typed.pane ?? source, value = { ...valueBinding(typed.node, "/tab"), ...(typed.related ? { related: true as const } : {}) };
      if(value.dashboard&&typed.related)throw new Error('Dashboards are UI layouts. Use /tab $NAME; related applies to source results.');
      const lifetime = layoutLifetime.current;
      await (host ? host.change : transitions.change)(previous => {
        if (lifetime !== layoutLifetime.current || sourceGeneration!==generationRef.current || (!value.dashboard&&value.generation !== generationRef.current)) throw new Error("Workspace changed while opening the value; try again.");
        return openValueTab(previous, target, binding, value, typed.activate);
      });
      if (lifetime !== layoutLifetime.current || sourceGeneration!==generationRef.current || (!value.dashboard&&value.generation !== generationRef.current)) return;
      setAliasNotice(`${value.related ? "related " : ""}$${value.label} tab is open in ${target}${typed.activate ? "" : ` · /tabx ${typed.related ? "related " : ""}$${value.label} to switch`}`);
      return;
    }
    if (typed.kind === "goto") {
      const lifetime = layoutLifetime.current, session = generationRef.current;
      if (!session) throw new Error("Wait for the workspace connection before finding a definition.");
      const cell = definitionTarget(workspace, [...latest.current, ...sharedCells], typed.node);
      const owner = [...definitionOwners.current.values()].find(owner => owner.workspace === binding && owner.cells.includes(cell.id)
        && splitRef.current.panes.some(pane => pane.id === owner.pane));
      const available = workspaceViews(splitRef.current).filter(pane => pane.workspace === binding && !pane.terminal && !pane.value && !pane.shows);
      const target = owner?.pane ?? available.find(pane => pane.id === sessionPane)?.id ?? available[0]?.id;
      if (!target) throw new Error("Open a session pane in this workspace to see its definition.");
      if (!splitRef.current.panes.some(pane => pane.id === target)) throw new Error("Open a session pane in this workspace to see its definition.");
      await (host ? host.change : transitions.change)(previous => {
        if (lifetime !== layoutLifetime.current || session !== generationRef.current) throw new Error("Workspace changed while finding the definition; try /goto again.");
        return selectWorkspaceTab(previous, target, binding);
      });
      if (lifetime !== layoutLifetime.current || session !== generationRef.current) return;
      if (!owner && target === sessionPane && !latest.current.some(item => item.id === cell.id)) setCells(previous => [...previous, cell]);
      setScreen(undefined);
      setFocused(cell.id);
      setDefinitionJump({ pane: target, cell: cell.id, revision: ++jumpRevision.current, source: cell });
      return;
    }
    let terminalTarget: TerminalTarget | undefined;
    const terminalIntent = typed.kind === "terminal-tab" ? typed
      : typed.kind === "directional-split" && typed.content && "terminal" in typed.content ? typed.content : undefined;
    const terminalOwner = layoutLifetime.current, terminalSession = generationRef.current;
    const terminalSource = splitRef.current.panes.find(p => p.id === source);
    // `/tab xterm` and `/tabx xterm` choose their destination now; it must still hold after resolving.
    const placing = typed.kind === "terminal-tab" && typed.activate !== undefined;
    const placedPane = typed.kind === "terminal-tab" ? typed.pane : undefined;
    let placement: TerminalPlacement | undefined;
    if (terminalIntent) {
      if (placing) placement = terminalPlacement(splitRef.current, source, placedPane);
      else if (typed.kind === "terminal-tab") canOpenTerminalTab(splitRef.current, source, sourceHistory);
      else canSplitTerminal(splitRef.current);
      if (!terminalSource) throw new Error("The source pane has closed.");
      const environment = terminalIntent.environment ?? commandEnvironment ?? engine.environmentContext()?.selected;
      if (terminalIntent.environment === undefined && commandEnvironment === null)
        throw new Error("No environment selected for this command; use xterm env:NAME.");
      const resolved = await engine.terminal<{ target: TerminalTarget | null }>({ action: "resolve",
        ...(environment ? { environment } : {}), ...(terminalIntent.target ? { target: terminalIntent.target } : {}) });
      if (terminalOwner !== layoutLifetime.current || terminalSession !== generationRef.current) throw new Error("Workspace changed while resolving the terminal; retry the terminal command.");
      if (splitRef.current.panes.find(p => p.id === source) !== terminalSource) throw new Error("The source pane changed while resolving the terminal; retry the terminal command.");
      if (placement) requireTerminalPlacement(splitRef.current, placement, placedPane);
      if (!Object.hasOwn(resolved, "target")) throw new Error("Terminal context resolution returned no destination; no terminal was opened.");
      terminalTarget = resolved.target ?? undefined;
    }
    let openedIdentity: string | undefined;
    const requested = typed.kind === "directional-split" && typed.content && "workspace" in typed.content ? typed.content.workspace : undefined;
    if (requested !== undefined) {
      if (workspace.nodes.some(node => node.name === requested) && requested !== contextRef.current?.workspace &&
        !workspace.identity?.saved.includes(requested) && !workspaceViews(splitRef.current).some(view => view.workspace === requested))
        throw new Error(`$${requested} is a variable in this workspace. Use /split $${requested} for its value; bare names open workspaces.`);
      if (splitRef.current.panes.length >= max_panes()) throw new Error("The pane budget is full. Close a pane before splitting.");
      if (!splitRef.current.panes.some(p => p.id === source)) throw new Error("The source pane has closed.");
      const sourcePane = splitRef.current.panes.find(p => p.id === source);
      const owner = layoutLifetime.current, session = generationRef.current;
      openedIdentity = await openWorkspace(requested);
      if (owner !== layoutLifetime.current || session !== generationRef.current) throw new Error("Workspace changed while opening; retry the split.");
      if (splitRef.current.panes.find(p => p.id === source) !== sourcePane) throw new Error("The source pane changed while opening the workspace; retry the split.");
    }
    await (host ? host.change : transitions.change)(previous => {
      if (terminalIntent) {
        // A queued transition may run after a retirement or another layout change: refuse, never reroute.
        if (terminalOwner !== layoutLifetime.current || terminalSession !== generationRef.current)
          throw new Error("Workspace changed while opening the terminal; retry the terminal command.");
        if (previous.panes.find(p => p.id === source) !== terminalSource)
          throw new Error("The source pane changed while opening the terminal; retry the terminal command.");
        if (placement) requireTerminalPlacement(previous, placement, placedPane);
      }
      const next = applyPaneCommand(previous, text, source, shown => {
        if (shown.screen === "open" || (shown.screen === "edit" && shown.node !== undefined)) {
          const named = resultNamed(workspace, shown.node ?? focusedNode.current, shown.screen === "edit" ? "/edit" : "/open");
          if (named.trouble !== undefined) throw new Error(named.trouble);
          return { ...shown, node: named.node };
        }
        if(shown.screen==='dashboard'&&shown.node&&!dashboards.some(entry=>entry.board.name===shown.node))
          throw new Error(`No dashboard named $${shown.node} is saved in this workspace.`);
        return shown;
      }, terminal, name => {
        return valueBinding(name, "/split");
      }, terminalTarget, sourceHistory);
      return requested !== undefined && openedIdentity !== undefined ? bindWorkspace(next, requested, openedIdentity) : next;
    });
    if (requested !== undefined) announceWorkspaceOpen(requested);
  };

  const answerAliases = useCallback((text: string): boolean => {
    setAliasNotice(undefined);
    const result = runClientCommand(text, settings, workspace.catalogue);
    const failure = result.said.find(message => message.severity === "error");
    if (failure) { setTrouble(failure.message); return false; }
    if (result.settings) setSettings(result.settings);
    setTrouble(undefined);
    setAliasNotice(result.said.map(message => message.message).join("\n"));
    return true;
  }, [settings, workspace.catalogue]);

  /*
   * The graph and the settings go where results go: a window of their own when the setting says
   * so, the screen over the session otherwise — or when no window could be opened. Typed as
   * `/settings` or pressed on the top line, the screen is summoned the same way.
   */
  const summonScreen = (which: Summoned, section?: SectionName) => {
    if ((which === "graph" || which === "stale" || which === "settings" || which === "spec") && openWhere.current === "window") {
      const elsewhere = window.open(screenRoute(which, engine.binding ?? contextRef.current?.workspace, section), "_blank");
      if (elsewhere || window.__WES_DESKTOP__ === true) return;
    }
    setScreen(which);
    if (section) setSection(section);
  };
  /** A screen over the session changes what it shows in place: `/stale` from `/graph`, a section from another. */
  const restageShown = (next: Partial<Shown>) => {
    if (next.screen) setScreen(next.screen);
    if (next.section) setSection(next.section);
  };

  const submit = useCallback(
    (text: string, scope: string = "prompt", source = splitRef.current.focused): string | undefined => {
      const written = text.trim();
      if (written === "") return undefined;
      setAliasNotice(undefined);
      const clearSourceDraft = () => { if (source === sessionPane) setDraft(""); };
      const typed = read(written);
      if (typed.kind !== "engine") engine.compose("", scope);
      if (typed.kind === "workspace-delete") {
        if(!generationRef.current || !contextRef.current?.workspace){setTrouble("Open a workspace before requesting its deletion preview.");return undefined;}
        setDeletingWorkspace(true);clearSourceDraft();return undefined;
      }
      if (typed.kind === "clear" || typed.kind === "debug") {
        if (source !== sessionPane || splitRef.current.panes.find(pane => pane.id === source)?.shows) {
          setTrouble(`Use a session prompt for /${typed.kind}.`); return undefined;
        }
        sessionCommands.answer(written); clearSourceDraft(); return undefined;
      }
      if (typed.kind === "aliases") {
        if (answerAliases(typed.command)) clearSourceDraft();
        return undefined;
      }
      if (typed.kind === "goto" || typed.kind === "value-tab" || typed.kind === "directional-split" || typed.kind === "split" || typed.kind === "close" || typed.kind === "workspace-tab" || typed.kind === "terminal-tab") {
        const owner = layoutLifetime.current, session = generationRef.current;
        const current = () => owner === layoutLifetime.current && session === generationRef.current;
        void paneCommand(written, source).then(() => {
          if (!current()) return;
          if (source === sessionPane) setDraft(draft => draft === text ? "" : draft);
          setTrouble(undefined);
        }).catch(error => { if (current()) setTrouble(error); });
        return;
      }
      if (typed.kind === "screen" && typed.inPane) {
        const shown: Shown = {
          screen: typed.screen,
          ...(typed.node === undefined ? {} : { node: typed.node }),
          ...(typed.section === undefined ? {} : { section: typed.section }),
          ...(typed.fileContext === undefined ? {} : { fileContext: typed.fileContext }),
          ...(typed.fileName === undefined ? {} : { fileName: typed.fileName }),
        };
        if (splitRef.current.panes.length >= max_panes() && !paneFor(splitRef.current, shown.screen, splitRef.current.panes.find(p => p.id === source)?.workspace)) {
          setTrouble("The pane budget is full. Close a pane before opening another screen."); return;
        }
        setSplit((was) => sendToPane(focus(was, source), shown));
        clearSourceDraft();
        setTrouble(undefined);
        return undefined;
      }
      if (typed.kind === "screen") {
        if(typed.screen==='dashboard'){
          if(typed.node&&!dashboards.some(entry=>entry.board.name===typed.node)){setTrouble(`No dashboard named $${typed.node} is saved in this workspace.`);return;}
          setDashboardTarget(typed.node);setScreen('dashboard');clearSourceDraft();return;
        }
        if (typed.screen === "edit") {
          editorReturn.current = undefined;
          if (typed.fileContext !== undefined) {
            setFileDraft(undefined);
            setFileContext(typed.fileContext);
            setFileName(typed.fileName);
            setScreen("edit");
            clearSourceDraft();
            return undefined;
          }
          const named = typed.node === undefined ? undefined : resultNamed(workspace, typed.node, "/edit");
          if (named?.trouble !== undefined) { setTrouble(named.trouble); return undefined; }
          if (named?.node !== undefined) {
            const its = latest.current.find((it) => it.nodes.includes(named.node!));
            if (its) openEditor(its.text, its.id);
          }
          setFileContext(undefined);
          setFileName(undefined);
          setScreen("edit");
          clearSourceDraft();
          return undefined;
        }
        if (typed.screen === "open") {
          // `/open` on its own opens whatever the caret is in, which is what `o` would have opened.
          const asked = resultNamed(workspace, typed.node ?? focusedNode.current);
          if (asked.trouble !== undefined) { setTrouble(asked.trouble); return undefined; }
          openResult(asked.node, "result");
          clearSourceDraft();
          return undefined;
        }
        /*
         * The graph and the settings go where results go: a window of their own when the setting
         * says so, the screen over the session otherwise — or when no window could be opened.
         */
        summonScreen(typed.screen, typed.section);
        clearSourceDraft();
        return undefined;
      }
      if (typed.kind === "settings") {
        setSettings(typed.change);
        clearSourceDraft();
        setTrouble(undefined);
        return undefined;
      }
      if (typed.kind === "trouble") {
        setTrouble(typed.said);
        return undefined;
      }
      let expanded: string;
      try {
        expanded = expandAlias(text, settings.aliases, workspace.catalogue);
      } catch (error) {
        setTrouble(error);
        return undefined;
      }
      try { engine.checkComposition(scope); }
      catch (error) { setTrouble(error); return undefined; }
      setTrouble(undefined);
      const cell = newCell(expanded);
      setCells((previous) => [...previous, cell]);
      setSent((count) => count + 1);
      setFocused(cell.id);
      clearSourceDraft();
      engine.submitComposed(cell.lastRun, cell.text, scope, scope === "editor").catch((failure: Error) => {
        setTrouble(failure);
        change(cell.id, (current) => submissionFailed(current, cell.lastRun, failure));
      });
      return cell.id;
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [change, engine, settings.aliases, workspace.catalogue, workspace.nodes, workspace.identity, sharedCells, openResult, answerAliases, sessionCommands.answer, sessionPane],
  );

  /**
   * `⌘⏎` — the editor's text, submitted as any command is, and the editor bound to what it made.
   *
   * The scrollback gets its cell like everything else; the binding is what makes the pane and that
   * cell two views of one node rather than two results that happen to agree.
   */
  const runFromEditor = useCallback(
    (text: string) => {
      const made = submit(text, "editor");
      if (made !== undefined) setBound(made);
    },
    [submit],
  );

  const context: SessionContext = {
    workspace: workspace.identity?.name ?? "workspace",
    environment: engine.environmentContext()?.selected ?? (engine.environmentContext() ? undefined : environments?.default ?? undefined),
    defaultEnvironment: environments?.default ?? undefined,
    connection,
    revision: undefined,
    grantMinutes: undefined,
    target: undefined,
  };

  contextRef.current = context;
  const model = useMemo(
    () => readSession({ workspace, cells, context, focused, held, language: pack, following: settings.stayAtNewest }, new Date()),
    // The verdict's only clock is a running command's, and a running command re-renders on its own.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [workspace, cells, focused, connection, environments, held, pack, settings.stayAtNewest],
  );

  modelRef.current = model;

  const actionsFor = useCallback(
    (cell: SessionCell): CellActions => ({
      ...(cell.source?.trimStart().startsWith("/") ? {} : { deleteWork: {
        preview: () => engine.previewDeleteWork(latest.current.find(it => it.id === cell.id)?.lastRun ?? cell.id),
        confirm: async (token: string, additionalWork: boolean, protectedContent: boolean) => {
          try { await engine.deleteWork(token, additionalWork, protectedContent); }
          catch (failure) { setTrouble(failure); throw failure; }
        },
      } }),
      ...(latest.current.find(it => it.id === cell.id)?.document ? {} : { repeat: (acknowledgeEffects: boolean) => repeatCell(cell.id, acknowledgeEffects) }),
      pin: () => change(cell.id, (it) => ({ ...it, pinned: !it.pinned })),
      cycle: node => change(cell.id, it => arrangeResult(it, node, { view: NEXT_VIEW[resultArrangement(it, node ?? it.nodes.at(-1)).view] })),
      setView: (view, node) => change(cell.id, it => arrangeResult(it, node, { view })),
      setHeight: (rows, node) => change(cell.id, it => arrangeResult(it, node, { rows: rows ?? null })),
      edit: () => {
        const mine = latest.current.find((it) => it.id === cell.id);
        if (!mine) return;
        if (mine.document) openDocument(mine);
        else openEditor(mine.text, mine.id);
      },
      open: (node?: string) => openResult(node ?? nodeOf(cell.id), "result"),
      json: (node?: string) => openResult(node ?? nodeOf(cell.id), "json"),
      details: (node?: string) => openResult(node ?? nodeOf(cell.id), "details"),
      /*
       * The one view chip, and ⌘click on the command line.
       *
       * Both go where `⊙ open` goes, at a tab of their own: a view is another way of reading this
       * result, not another dress for this cell, so the scrollback is left exactly as it was.
       */
      view: (name, node) => openResult(node ?? nodeOf(cell.id), name),
      openSource: () => openCellSource(cell),
      ...(nodeOf(cell.id) ? { peek: (what: PeekWhat, node?: string) => what === "source" ? openCellSource(cell) : openPeek(node ?? nodeOf(cell.id), what) } : {}),
      // The cell shows the request until the engine records each node's state; a refusal is said.
      cancel: () => {
        const mine = latest.current.find((it) => it.id === cell.id);
        const requests = Promise.all((mine?.nodes ?? []).map(node => engine.cancel(node)));
        requests.catch(failure => setTrouble(failure));
        return requests;
      },
      copy: () => { void navigator.clipboard?.writeText(cell.source ?? "").catch(() => undefined); },
      // `⇣ follow` on a live cell is the same setting `/follow` writes: stay with the newest line.
      follow: () => setSettings((was) => ({ ...was, stayAtNewest: !was.stayAtNewest })),
      next: () => step(1),
      previous: () => step(-1),
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [change, engine, openResult, repeatCell],
  );

  const step = (by: 1 | -1) => {
    const all = latest.current;
    if (all.length === 0) return;
    const at = all.findIndex((cell) => cell.id === focusRef.current);
    const next = at < 0 ? (by === 1 ? 0 : all.length - 1) : (at + by + all.length) % all.length;
    const id = all[next]!.id;
    setFocused(id);
    if (typeof document !== "undefined") document.querySelector<HTMLElement>(`[data-cell="${id}"]`)?.focus();
  };

  const chrome = settings.surfaceChrome;
  /** Both a result's given name and the id it has always had answer to `$`. */
  const names = useMemo(() => referables(workspace.nodes), [workspace.nodes]);
  const interactiveStates = useInteractiveStates(workspace, generation);
  const output = (cell: SessionCell): readonly CellBlock[] => {
    const diagnostic = sessionCommands.reportFor(cell.id);
    return diagnostic === undefined
      ? cellBlocks({ interactiveStates, cell, workspace, held, reads, observations, retryRead, engine, generation, onRefresh: !latest.current.find(it => it.id === cell.id)?.document ? () => repeatCell(cell.id, false) : undefined })
      : [{ key: "debug", open: true, content: <DebugReport report={diagnostic} /> }];
  };

  /*
   * `/graph`, over the workspace's own nodes.
   *
   * The selection defaults to the newest node rather than to nothing, because a panel is the point
   * of the screen and the node somebody just made is the one they came to ask about.
   */
  const selectedNode = chosenNode ?? workspace.nodes[workspace.nodes.length - 1]?.id;
  const cellOfNode = (node: string) => latest.current.find((cell) => cell.nodes.includes(node));
  const graph = readGraph(workspace, { selected: selectedNode, staleOnly: screen === "stale", connectedOnly, held });
  /** What `/settings` shows, for whichever section the caller is drawing. */
  const viewOf = (which: SectionName) => readSection(which, { settings, workspace, ...(pack ? { pack } : {}) });

  /*
   * What `/open` shows, when it is showing over the workspace rather than in a window of its own.
   *
   * The same model either way — one node, its facts, its whole result — so the two can only ever
   * agree. The node is the one that was opened; failing that, the focused cell's own.
   */
  /*
   * Which result `/open` is showing.
   *
   * By id only: `openResult` is given a node id, and matching an undefined `wanted` against every
   * node's name would find the first unnamed one — which is how opening a cell that made no result
   * came to show somebody else's.
   */
  const openFor = (which: string | undefined) => {
    const node = which === undefined ? undefined : workspace.nodes.find((it) => it.id === which);
    const observation = node ? observations.get(node.id) : undefined;
    const value = observation?.value ?? (node?.handle ? held.get(node.handle) : undefined);
    // The attempt that made it, for the two facts that belong to the attempt and not to the node.
    const made = node && [...cells, ...sharedCells].find((it) => it.nodes.includes(node.id));
    const handle = node?.handle;
    // An active stream has no stored value here (it is never read through useResults); its pane
    // reads the bounded display window on demand through the same presentation as its cell.
    const live = node && generation && !value && resultAccess(node).live
      ? <LiveView mode="window" engine={engine} generation={generation} node={node} workspace={workspace.identity?.name} cellHold={false}
          key={`${generation}:${node.id}:${node.command}:${JSON.stringify(node.environment)}`}
          sourceLabel={source => { const named = workspace.nodes.find(candidate => candidate.id === source)?.name; return named ? `$${named}` : source; }} />
      : undefined;
    return {
      value,
      live,
      /* One subject, asked of every registered view, here and in the result's own window alike. */
      viewing: {
        ...(value ? { value } : {}),
        ...(node ? { node } : {}),
        engine,
        ...(generation ? { generation } : {}),
      } satisfies ViewSubject,
      ...readOpen({
        ...(node ? { node } : {}),
        ...(made ? { cell: made } : {}),
        workspace,
        context,
        client: engine.client,
        ...(value ? { stored: value } : {}),
      }),
      reading: live ? undefined : observation && observation.state !== "current" ? <div className="result-observation"><ObservationStatus observation={observation} onRetry={handle ? () => retryRead(handle) : undefined} /></div> :
        handle && !value
          ? <ReadStatus problem={reads.get(handle)?.problem} onRetry={() => retryRead(handle)} />
          : undefined,
    };
  };

  /*
   * What `/edit` is bound to, and the one cell that is the other view of it.
   *
   * The pane draws the bound node's own cell, in minimal chrome, from the same model the scrollback
   * draws it from — so a repeat started in either place changes both, because there is only one of
   * them. Before the first run there is no binding and the pane says so.
   */
  const boundCell = bound === undefined ? undefined : model.cells.find((it) => it.id === bound);
  const boundNode = boundCell && (() => {
    const made = cells.find((it) => it.id === boundCell.id)?.nodes.at(-1);
    return made === undefined ? undefined : workspace.nodes.find((it) => it.id === made);
  })();
  const editBound: Bound | undefined = boundCell
    ? {
        ...(boundNode ? { node: boundNode.id } : {}),
        ...(boundNode?.name ? { name: boundNode.name } : {}),
        ...(ranAt(boundNode?.startedAt) ? { ran: ranAt(boundNode?.startedAt)! } : {}),
      }
    : undefined;

  const environmentAction = (action: "use" | "clear" | "enable", name?: string) => {
    const attempt = crypto.randomUUID();
    environmentAttempt.current = attempt;
    setTrouble(undefined);
    void engine.submit(attempt, `:env ${action}${name === undefined ? "" : ` ${JSON.stringify(name)}`}`)
      .catch((error: Error) => setTrouble(error));
  };

  const selectEnvironment = (name?: string) => environmentAction(name === undefined ? "clear" : "use", name);

  /**
   * The surface one pane shows — or the whole workspace, when there is only one.
   *
   * The same screens either way: a `/graph` in a pane and a `/graph` over the workspace are one
   * screen with one set of arguments, drawn in `pane` chrome or `full`. What differs is where `esc`
   * goes, so the caller says: over the workspace it closes the screen, and in a pane it closes what
   * is in the pane and leaves the pane standing.
   */
  const screenSurface = (shown: Shown, chromeMode: "full" | "pane", leave: () => void, restage: (next: Partial<Shown>) => void) => {
    if(shown.screen==='dashboard'){
      const target=shown.node??dashboardTarget,board=dashboards.find(entry=>entry.board.name===target)?.board;
      if(target&&!board)return <DashboardUnavailable message={`No dashboard named $${target} is saved in this workspace.`} onClose={leave}/>;
      return <DashboardHost key={`dashboard-editor:${target??'library'}:${generation}`} workspace={workspace} engine={engine} generation={generation} saved={dashboards} boardId={board?.id} edit={!!target} onSave={saveBoard} onRemove={removeBoard} onClose={leave}/>;
    }
    if (shown.screen === "spec") return <SpecScreen key={`${workspace.identity?.name}:${generation}`} {...(workspace.identity?.name&&generation?{binding:{workspace:workspace.identity.name,generation}}:{})} top={model.top} chrome={chromeMode} onClose={leave} onSubmit={text => submit(text, "prompt", sessionPane)} />;
    const forOpen = openFor(shown.node ?? opened);
    const peekedNode = workspace.nodes.find((it) => it.id === (shown.node ?? opened));
    const peekedCell = sourceCell === undefined ? undefined : model.cells.find(it => it.id === sourceCell);
    return shown.screen === "graph" || shown.screen === "stale" ? (
    <GraphScreen
      chrome={chromeMode}
      top={model.top}
      nodes={graph.nodes}
      edges={graph.edges}
      cycles={graph.cycles}
      {...(graph.selected ? { selected: graph.selected } : {})}
      staleOnly={shown.screen === "stale"}
      connectedOnly={connectedOnly}
      {...(graph.hidden ? { hidden: graph.hidden } : {})}
      direction={settings.direction}
      onStaleOnly={(on) => restage({ screen: on ? "stale" : "graph" })}
      onConnectedOnly={setConnectedOnly}
      onDirection={(direction) => setSettings((was) => ({ ...was, direction }))}
      onJump={() => {
        const mine = selectedNode === undefined ? undefined : cellOfNode(selectedNode);
        if (!mine) return;
        leave();
        setFocused(mine.id);
        // After the session is back on screen; a cell that is not drawn cannot be scrolled to.
        window.requestAnimationFrame(() => {
          document.querySelector(`[data-cell="${mine.id}"]`)?.scrollIntoView({ block: "center" });
        });
      }}
      onRepeat={() => {
        const mine = selectedNode === undefined ? undefined : cellOfNode(selectedNode);
        if (mine) repeatCell(mine.id, false);
      }}
      onOpenResult={() => openResult(selectedNode, "result")}
      onClose={leave}
      canvas={
        <GraphCanvas nodes={graph.nodes} edges={graph.edges} direction={settings.direction} onSelect={setChosenNode} />
      }
    />
  ) : (
    shown.screen === "settings" ? (
      <SettingsScreen
        chrome={chromeMode}
        top={model.top}
        sections={[...SECTIONS]}
        section={shown.section ?? section}
        rows={viewOf(shown.section ?? section).rows}
        facts={viewOf(shown.section ?? section).facts}
        {...(viewOf(shown.section ?? section).empty === undefined ? {} : { empty: viewOf(shown.section ?? section).empty! })}
        {...((shown.section ?? section) === "appearance" ? { preview: previewOf(settings) } : {})}
        onChoose={(row, option) => setSettings((was) => chose(was, row, option))}
        onSection={(name) => restage({ section: sectionNamed(name) })}
        onClose={leave}
      />
    ) : (
      shown.screen === "edit" && shown.fileContext ? (
        <EditFileScreen
          key={chromeMode === "full" ? fileDraft?.id : undefined}
          {...(chromeMode === "full" && fileDraft ? { initialSource: fileDraft.source, initialBase: fileDraft.base, initialOrigin: fileDraft.origin } : {})}
          chrome={chromeMode}
          top={model.top}
          context={shown.fileContext}
          loadEnvironments={signal => engine.environmentDocuments(signal)}
          generation={generation}
          environmentContext={engine.environmentContext()}
          onRun={async (source, options) => {
            const planName = `editorPlan${crypto.randomUUID().replaceAll("-", "")}`;
            const text = documentCommand(shown.fileContext!, options.origin, options.base, planName);
            const cell = { ...newCell(text), document: { source } };
            setCells(previous => [...previous, cell]);
            setSent(count => count + 1);
            try {
              const result = await engine.submitDocument(cell.lastRun, text, source, options.generation, options.environments);
              return shown.fileContext === "env"
                ? `${result.diagnostics?.filter(d => d.severity !== "error").map(d => d.message).join("\n") || "Plan created."}\nApply separately with :env apply $${planName}\nDiscard with :env discard $${planName}\nNothing applied.`
                : "Package loaded.";
            } catch (error) {
              change(cell.id, current => submissionFailed(current, cell.lastRun, error));
              throw error;
            }
          }}
          vocabulary={{ names: workspace.catalogue.types ?? [] }}
          {...(shown.fileName === undefined ? {} : { initialName: shown.fileName })}
          onClose={leave}
        />
      ) : shown.screen === "edit" && pack ? (
        <EditScreen
          chrome={chromeMode}
          autoFocus={chromeMode === "full" || split.focused === screenPane}
          top={model.top}
          source={draftProgram}
          language={pack}
          names={names}
          {...(editBound ? { bound: editBound } : {})}
          context={model.context}
          onChange={text => changeDraft(text, "editor")}
          onRun={(text) => { runFromEditor(text); }}
          onRunAgain={() => { if (boundCell) repeatCell(boundCell.id, false); }}
          onClose={() => { returnEditorDraft(); leave(); }}
          {...(boundCell
            ? {
                output: (
                  <Cell
                    theme={chrome}
                    tailKeys={settings.surfaceTailKeys === "shown"}
                    state={boundCell.state}
                    streamOutput={boundCell.streamOutput}
                    streamSource={boundCell.streamSource}
                    outputIdentity={`${generation ?? ""}:${boundCell.source ?? ""}`}
                    rows={boundCell.rows}
                    {...(boundCell.time ? { time: boundCell.time } : {})}
                    chars={boundCell.chars}
                    verdict={boundCell.verdict}
                    marks={boundCell.marks}
                    actions={actionsFor(boundCell)}
                    {...(boundCell.guard ? { confirmRepeat: boundCell.guard } : {})}
                    pipeline={boundCell.pipeline}
                    view={boundCell.view}
                    blocks={output(boundCell)}
                    label={boundCell.id}
                    attempt={boundCell.attempt}
                  />
                ),
              }
            : {})}
        />
      ) : shown.screen === "peek" ? (
        <PeekScreen engine={engine}
          chrome={chromeMode}
          top={model.top}
          subject={[{ text: sourceCell ?? (peekedNode?.name ? `$${peekedNode.name}` : peekedNode?.id ?? ""), role: "mono-ref" }]}
          what={peek}
          {...peekOf(peekedNode, forOpen.value)}
          {...(sourceCell === undefined ? {} : { source: peekedCell?.source })}
          readStatus={forOpen.reading}
          onClose={leave}
        />
      ) : shown.screen === "open" ? (
        <OpenScreen
          chrome={chromeMode}
          top={model.top}
          subject={forOpen.subject}
          tab={tab}
          onTab={setTab}
          value={forOpen.value}
          live={forOpen.live}
          viewing={forOpen.viewing}
          json={forOpen.json}
          details={forOpen.details}
          readStatus={forOpen.reading}
          onClose={leave}
        />
      ) : (
        shown.screen === "env" ? <EnvScreen {...(workspace.identity?.name&&generation?{authentication:{workspace:workspace.identity.name,generation}}:{})} chrome={chromeMode} top={model.top} environments={readEnvironments(environments)} chosen={engine.environmentContext()?.selected ?? ""}
          onChoose={selectEnvironment}
          onEnable={name => environmentAction("enable", name)}
          onClear={() => selectEnvironment()}
          onClose={leave} /> : null
      )
    )
    );
  };

  const sessionSurface = (inPane: boolean) => (
    <Session
      definition={{ pane: sessionPane, workspace: binding, register: registerDefinition }} jump={definitionJump}
      history={{ engine, workspace, generation }}
      chromeMode={inPane ? "pane" : "full"}
      model={model}
      chrome={chrome}
      tailKeys={settings.surfaceTailKeys === "shown"}
      prompt={
        <Prompt
          dashboards={dashboards.map(entry=>entry.board.name)}
          language={pack}
          autoFocus={split.focused === sessionPane && split.panes.find(p => p.id === sessionPane)?.workspace === binding}
          draft={draft}
          history={cells.map(cell => cell.text)}
          historyScope={generation}
          onDraft={text => changeDraft(text, "prompt")}
          onSubmit={text => submit(text, "prompt", sessionPane)}
          chromeName={chrome}
          onChrome={(next) => setSettings((was) => ({ ...was, surfaceChrome: next }))}
          catalogue={workspace.catalogue}
          variables={workspace.nodes.flatMap(node => node.name ? [node.name] : [])}
          names={names}
          aliases={settings.aliases}
          workspaces={workspace.identity?.saved}
          onGrow={(text) => {
            editorReturn.current = undefined;
            setFileContext(undefined); setFileName(undefined);
            engine.moveComposition("prompt", "editor");
            engine.compose(text, "editor");
            setDraftProgram(text);
            setBound(undefined);
            setScreen("edit");
            setDraft("");
          }}
        />
      }
      actions={actionsFor}
      output={output}
      onFocus={setFocused}
      following={settings.stayAtNewest}
      pinned={sent}
      clearRequest={sessionCommands.clearRequest}
    />
  );

  /*
   * One pane, or several.
   *
   * With one, the workspace is the session or whatever screen was summoned over it — which is what
   * it has always been. With several, the panes are the workspace and each draws its own surface:
   * the session's own pane, a second session, or a screen somebody sent there.
   */
  const newTerminalTab = (pane: Pane) => { void paneCommand("/terminal-tab", pane.id, true, undefined, pane.history).catch((error: Error) => setTrouble(error)); };
  const paneCommandSurface: WorkspaceSurface["command"] = pane => !pane.terminal && (pane.shows || pane.value) ? <PaneCommand dashboards={dashboards.map(entry=>entry.board.name)} variables={workspace.nodes.flatMap(node => node.name ? [node.name] : [])} workspaces={workspace.identity?.saved} onCommand={text => {
        if (read(text).kind === "engine") { setTrouble("Use a session pane for wes commands; this field accepts /commands."); return; }
        submit(text, `pane-command:${pane.id}`, pane.id);
      }} /> : null;
  const paneContent: WorkspaceSurface["content"] = (pane) =>
        pane.value ? (() => {
          const value = pane.value!, key = viewKey(pane), node = workspace.nodes.find(node => node.id === value.node);
          if(value.dashboard)return <DashboardHost key={`board:${value.node}:${generation}`} workspace={workspace} engine={engine} generation={generation} saved={dashboards} boardId={value.node} closeLabel="Close dashboard" onSave={saveBoard} onRemove={removeBoard} onClose={()=>setSplit(previous=>closeViewTab(previous,pane.id,key))}/>;
          if (generation !== value.generation) return <p role="status">This view belongs to a previous workspace session. Open ${value.label} again for the current result.</p>;
          if (!node) return <p role="status">This node no longer exists in this workspace.</p>;
          if (value.related) return <ComponentPane binding={value} generation={generation} cells={sharedCells}
            workspace={workspace} engine={engine} context={context} chrome={chrome} tailKeys={settings.surfaceTailKeys === "shown"} language={pack}
            held={held} reads={reads} retryRead={retryRead} onOpen={openResult} onPeek={openPeek} onTrouble={setTrouble} />;
          const opened = openFor(value.node);
          const rebound = workspace.nodes.some(other => other.id !== value.node && other.name === value.label);
          const status = !opened.value && !opened.reading && !opened.live ? <p role="status">{node.failure ?? node.cancellation?.reason ??
            (node.state === "running" || node.state === "pending" ? "Waiting for this node's output…" : node.staleReason?.message ?? "No value is available for this node. The command was not rerun.")}</p> : opened.reading;
          return <><OpenScreen chrome="pane" top={model.top} subject={opened.subject} value={opened.value} live={opened.live} viewing={opened.viewing}
            tab={value.tab ?? "result"} json={opened.json} details={opened.details} readStatus={<>{rebound && <p role="status">${value.label} now names a newer node; this view stays with the original node.</p>}{status}</>}
            onTab={tab => setSplit(previous => updateValueTab(previous, pane.id, key, tab))}
            onClose={() => setSplit(previous => closeViewTab(previous, pane.id, key))} /></>;
        })() : pane.terminal
          ? <ShellTerminal engine={engine} generation={generation} active focused={split.focused === pane.id && !screen && split.panes.find(p => p.id === pane.id)?.workspace === binding && split.panes.find(p => p.id === pane.id)?.history === pane.history} autoStart allowTargetStart={pane.terminalLaunch} closeOnUnmount cwd={pane.cwd} target={pane.terminalTarget}
              logContext={{ workspace: context.workspace ?? binding, pane: pane.id }} history={pane.history} beforeStart={() => prepareTerminal(pane.history!)}
              onTargetChange={(previous, next) => (host ? host.change : transitions.change)(was => acceptTerminalTarget(was, pane.id, pane.history!, previous, next))}
              onCommand={(text, environment) => paneCommand(text, pane.id, true, environment, pane.history)}
              onDirectory={cwd => setSplit(was => terminalDirectory(was, pane.id, pane.history!, cwd))} />
          : pane.shows?.screen === "edit" && !pane.shows.fileContext && pack
          ? (() => {
              const node = workspace.nodes.find(n => n.id === pane.shows?.node || n.name === pane.shows?.node);
              return pane.shows.node && !node ? <p role="status">Waiting for the saved source; it may no longer exist in this workspace.</p> : <PaneEditor
                key={`${pane.id}:${pane.shows.node ?? ""}`} initial={node?.command ?? ""} engine={engine} scope={`pane-editor:${pane.id}`}
                language={pack} names={names} top={model.top} context={model.context} focused={split.focused === pane.id && !screen && split.panes.find(p => p.id === pane.id)?.workspace === binding}
                onRun={(text, scope) => submit(text, scope, pane.id)} onRepeat={id => repeatCell(id, false)}
                onClose={() => setSplit(was => clearPane(was, pane.id))} />;
            })()
          : pane.shows
          ? screenSurface(
              pane.shows,
              "pane",
              () => setSplit((was) => clearPane(was, pane.id)),
              (next) => setSplit((was) => ({ ...was, panes: was.panes.map(p => p.id === pane.id && p.shows ? { ...p, shows: { ...p.shows, ...next }, title: titleOf({ ...p.shows, ...next }) } : p) })),
            )
          : pane.id === sessionPane
            ? sessionSurface(true)
            : (
                <SessionPane
                  definition={{ pane: pane.id, workspace: binding, register: registerDefinition }} jump={definitionJump}
                  language={pack}
                  focused={split.focused === pane.id && split.panes.find(p => p.id === pane.id)?.workspace === binding}
                  engine={engine}
                  compositionScope={`pane-prompt:${pane.id}`}
                  environments={environments}
                  workspace={workspace}
                  context={context}
                  settings={settings}
                  chrome={chrome}
                  held={held}
                  reads={reads}
                  retryRead={retryRead}
                  generation={generation}
                  subscribe={subscribe}
                  onCommand={(text) => {
                    const typed = read(text);
                    if (typed.kind === "aliases") return answerAliases(typed.command);
                    submit(text, `pane-command:${pane.id}`, pane.id); return true;
                  }}
                  onTrouble={setTrouble}
                  onEditDocument={openDocument}
                  claim={(attempt) => claimed.current.add(attempt)}
                  onGrow={(text, receive) => {
                    const scope = `pane-prompt:${pane.id}`;
                    editorReturn.current = { scope, pane: pane.id, receive };
                    setFileContext(undefined); setFileName(undefined);
                    engine.moveComposition(scope, "editor"); engine.compose(text, "editor");
                    setDraftProgram(text); setBound(undefined); setScreen("edit");
                  }}
                />
              )
      ;
  const body = (views: ReadonlyMap<string, WorkspaceSurface>) => {
    const focusedWorkspace = split.panes.find(pane => pane.id === split.focused)?.workspace;
    return (
    <div className="surface-workspace" hidden={!!screen}>
    <Split
      active={!screen}
      state={split}
      originWorkspace={context.workspace}
      top={views.get(split.panes.find(pane => pane.id === split.focused)?.workspace ?? "")?.top ?? model.top}
      prompt={[]}
      context={model.context}
      status={focusedWorkspace === undefined ? model.note : views.get(focusedWorkspace)?.status}
      capacity={focusedWorkspace === undefined ? (connection === "connected" ? workspace.capacity : undefined) : views.get(focusedWorkspace)?.capacity}
      onChange={setSplit}
      onNewTerminalTab={pane => pane.workspace === undefined ? newTerminalTab(pane) : views.get(pane.workspace)?.newTerminalTab(pane)}
      onSettings={() => summonScreen("settings")}
      onGraph={focusedWorkspace === undefined ? () => summonScreen("graph") : views.get(focusedWorkspace)?.openGraph}
      command={pane => pane.workspace === undefined ? paneCommandSurface(pane) : views.get(pane.workspace)?.command(pane)}
      content={pane => pane.workspace === undefined ? paneContent(pane) : views.get(pane.workspace)?.content(pane)}
    />
    </div>
  );
  };

  const confirmAsked = () => {
    const asking = repeatAsked;
    setRepeatAsked(undefined);
    if (!asking) return;
    if (latest.current.find(cell => cell.id === asking.id)?.lastRun !== asking.attempt) {
      setTrouble("This work changed while confirming; inspect it before repeating."); return;
    }
    repeatCell(asking.id, true);
  };

  const repeatQuestion = repeatAsked && (() => {
        const cell = model.cells.find(cell => cell.id === repeatAsked.id);
        if (!cell?.guard) return null;
        return <div className="surface-repeat-question" tabIndex={-1} ref={element => element?.focus()} onKeyDown={event => {
          if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); setRepeatAsked(undefined); }
          if (event.key === "Enter") { event.preventDefault(); event.stopPropagation(); confirmAsked(); }
        }}>
          <RepeatQuestion guard={cell.guard} />
          <button type="button" className="cell-action" onClick={confirmAsked}>confirm repeat</button>
          <button type="button" className="cell-action" onClick={() => setRepeatAsked(undefined)}>cancel</button>
        </div>;
      })();

  /*
   * Leaving a screen gives the caret back to the pane it covered. The split moves focus only when
   * its own state changes, and a screen inside a pane changes none of it, so without this the
   * caret would fall to the document body, where no pane hears `⌘L` or a typed character.
   */
  const leaveScreen = () => { setScreen(undefined); returnFocusToPane(screenPane ?? sessionPane); };

  const scopedSurface: WorkspaceSurface = {
    openGraph: () => summonScreen("graph"),
    top: model.top,
    status: model.note,
    capacity: connection === "connected" ? workspace.capacity : undefined,
    newTerminalTab,
    command: pane => screen && pane.id === screenPane ? null : paneCommandSurface(pane),
    content: pane => <div className="workspace-pane-content" onKeyDown={event => {
      if (!event.defaultPrevented && event.key === "Escape" && screen && pane.id === screenPane) {
        event.preventDefault(); event.stopPropagation();
        if (screen === "edit" && !fileContext) returnEditorDraft();
        leaveScreen();
      }
    }}>
      {renderWorkspace && deletingWorkspace && generation && contextRef.current?.workspace && pane.id === sessionPane && <WorkspaceDeletion binding={{workspace:contextRef.current.workspace,generation,client:engine.client}} onClose={()=>setDeletingWorkspace(false)} onDeleted={()=>setDeletingWorkspace(false)}/>}
      {renderWorkspace && sandbox && pane.id === sessionPane && <SandboxPanel engine={engine} opened={sandbox} onClose={() => setSandbox(undefined)} />}
      <div className="workspace-pane-view" hidden={!!screen && pane.id === screenPane}>{paneContent(pane)}</div>
      {screen && pane.id === screenPane && screenSurface({ screen, ...(screen==='dashboard'?(dashboardTarget?{node:dashboardTarget}:{}):opened === undefined ? {} : { node: opened }), section,
        ...(fileContext === undefined ? {} : { fileContext }), ...(fileName === undefined ? {} : { fileName }) },
        "pane", leaveScreen, restageShown)}
      {pane.id === sessionPane && repeatQuestion}
      {aliasNotice && <MonoLine segments={[{ text: aliasNotice, role: "mono-dim" }]} />}
      {pane.id === sessionPane && <DraftNotice engine={engine} scope={screen === "edit" && !fileContext ? "editor" : "prompt"}
        environments={environments} onReview={() => contextChanged(n => n + 1)} onTrouble={setTrouble} />}
      {generation === undefined && <MonoLine segments={[{ text: "waiting for this workspace…", role: "mono-faint" }]} />}
    </div>,
  };
  useLayoutEffect(() => { renderWorkspace?.(scopedSurface); });
  if (renderWorkspace) return null;

  return (
    <div
      className="wes-terminal surface-terminal surface-app"
      data-palette={palette}
      data-density={settings.surfaceDensity}
      data-focus={settings.surfaceFocus}
      style={surfaceTypeStyle(settings)}
      onKeyDown={(event) => {
        if (event.defaultPrevented || (event.target as HTMLElement | undefined)?.closest?.("input, textarea, [contenteditable=true]")) return;
        if (!screen) return;
        if (event.key === "Escape") {
          event.preventDefault();
          if (screen === "edit" && !fileContext) returnEditorDraft();
          leaveScreen();
        }
        /*
         * `/ another screen`, which every screen's footer offers. A screen has no prompt of its
         * own, so this is the prompt: the session comes back with the slash already typed and the
         * completion list open on it.
         */
        if (event.key === "/") { event.preventDefault(); leaveScreen(); setDraft("/"); }
      }}
    >
      <style>{roleStyles(".wes-terminal")}</style>
      <Hints />
      {workspaceClosed && <p className="screen-label">No workspace is selected. Use /tabx NAME to open or create one.</p>}
      {deletingWorkspace && generation && contextRef.current?.workspace && <WorkspaceDeletion binding={{workspace:contextRef.current.workspace,generation,client:engine.client}} onClose={()=>setDeletingWorkspace(false)} onDeleted={()=>setDeletingWorkspace(false)}/>}
      {sandbox && <SandboxPanel engine={engine} opened={sandbox} onClose={() => setSandbox(undefined)} />}
      <WorkspaceSurfaces names={retainedWorkspaces} host={{ split, originWorkspace: context.workspace, settings, settingsChange: setSettings,
        change: change => transitions.change(change), engine: workspaceEngine }} children={body} />
      {screen && screenSurface({
        screen, ...(screen==='dashboard'?(dashboardTarget?{node:dashboardTarget}:{}):opened === undefined ? {} : { node: opened }), section,
        ...(fileContext === undefined ? {} : { fileContext }), ...(fileName === undefined ? {} : { fileName }),
      }, "full", leaveScreen, restageShown)}
      {repeatQuestion}
      <DraftNotice engine={engine} scope={screen === "edit" && !fileContext ? "editor" : "prompt"} environments={environments} onReview={() => contextChanged(n => n + 1)} onTrouble={setTrouble} />
      <SaveStatus />
      {aliasNotice && <div role="status" aria-live="polite" aria-label="Personal aliases">
        <MonoLine segments={[{ text: aliasNotice, role: "mono-dim" }]} className="surface-trouble" />
      </div>}
      {generation === undefined && (
        <MonoLine segments={[{ text: "waiting for the engine…", role: "mono-faint" }]} className="surface-trouble" />
      )}
    </div>
  );
}
