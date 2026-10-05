import { TerminalTargetReview, type TerminalReview } from "./TerminalTargetReview";
import { targetKey, type TerminalTarget } from "./terminal-target";
import { TerminalDiagnostics } from "./terminal-diagnostics";
import type { LogContext } from "./application-log";
import { TerminalUnavailable } from "./terminal-errors";
import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import type { ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import type { Engine } from "./engine";
import { defaults, monoFamily } from "./settings";
import { terminalInput } from "./terminal-input";
import { terminalOutput, type TerminalFrame } from "./terminal-output";
import { normalizeTerminalWheel } from "./terminal-wheel";
import { accelerateTerminal } from "./terminal-renderer";
import { hyperlinkHandler, linkTerminal } from "./terminal-links";
// ANSI palettes matched to the app's light/dark themes so program output stays legible on either ground.
const LIGHT_ANSI = { black: "#24292f", red: "#cf222e", green: "#116329", yellow: "#4d2d00", blue: "#0969da", magenta: "#8250df", cyan: "#1b7c83", white: "#6e7781", brightBlack: "#57606a", brightRed: "#a40e26", brightGreen: "#1a7f37", brightYellow: "#633c01", brightBlue: "#218bff", brightMagenta: "#a475f9", brightCyan: "#3192aa", brightWhite: "#8c959f" };
const DARK_ANSI = { black: "#484f58", red: "#ff7b72", green: "#3fb950", yellow: "#d29922", blue: "#58a6ff", magenta: "#bc8cff", cyan: "#39c5cf", white: "#b1bac4", brightBlack: "#6e7681", brightRed: "#ffa198", brightGreen: "#56d364", brightYellow: "#e3b341", brightBlue: "#79c0ff", brightMagenta: "#d2a8ff", brightCyan: "#56d4dd", brightWhite: "#ffffff" };
/** Derive the emulator theme from the app's own CSS tokens, so the terminal follows light/dark. */
export function xtermTheme(host: HTMLElement): ITheme {
  const surface = host.closest?.(".wes-terminal");
  const style = getComputedStyle(host);
  const dark = surface?.getAttribute("data-palette") === "ink";
  const token = (name: string, fallback: string) => style.getPropertyValue(name).trim() || fallback;
  const accent = token("--mono-ref", dark ? "#AE8FD8" : "#6A4BA8");
  return {
    background: token("--terminal", dark ? "#0D0F12" : "#FAF7F0"),
    foreground: token("--mono-ink", dark ? "#D8DBE0" : "#1B1D1F"),
    cursor: accent,
    cursorAccent: token("--terminal", dark ? "#0D0F12" : "#FAF7F0"),
    selectionBackground: `${accent}40`,
    ...(dark ? DARK_ANSI : LIGHT_ANSI),
  };
}
/** xterm renders its own canvas, so inheriting the host's CSS font is not enough. */
function xtermFont(host: HTMLElement): string {
  return getComputedStyle(host).getPropertyValue("--type-mono-family").trim() || monoFamily(defaults.surfaceFace);
}
/** The emulator remains mounted while hidden. Input never goes through the wes editor. */
export function ShellTerminal({ engine, active, generation, focused = active, autoStart = false, closeOnUnmount = false, cwd, history, target, allowTargetStart = false, beforeStart, onTargetChange, onDirectory, onCommand, logContext }: {
  engine: Engine; active: boolean; generation?: string; focused?: boolean; autoStart?: boolean; closeOnUnmount?: boolean;
  logContext?: LogContext;
  target?: TerminalTarget; allowTargetStart?: boolean;
  cwd?: string; history?: string; beforeStart?: () => Promise<void>;
  onTargetChange?: (previous: TerminalTarget, next: TerminalTarget) => Promise<void>;
  onDirectory?: (cwd: string) => void; onCommand?: (text: string, environment?: string | null) => void | Promise<void>;
}) {
  const container = useRef<HTMLDivElement>(null);
  const emulator = useRef<Terminal>();
  const fit = useRef<(force?: boolean) => void>();
  const identity = useRef<string>();
  const [id, setId] = useState<string>();
  const destination = target;
  const currentTarget = useRef(target); currentTarget.current = target;
  const targetChanged = useRef(onTargetChange); targetChanged.current = onTargetChange;
  const [review, setReview] = useState<{ evidence: TerminalReview; selected: TerminalTarget; generation?: string; owner: object }>();
  const [runningDestination, setRunningDestination] = useState("");
  const [runningLabel, setRunningLabel] = useState("This computer · workspace tools");
  const [runningIdentity, setRunningIdentity] = useState<string>();
  const directoryAllowed = useRef(!target);
  const [busy, setBusy] = useState(false);
  const [ended, setEnded] = useState(false);
  const [restartRequired, setRestartRequired] = useState(false);
  const diagnostics = useRef(new TerminalDiagnostics());
  const logScope = useRef<LogContext>({});
  logScope.current = { workspace: engine.binding, ...logContext, generation };
  const contextFor = (terminal?: string) => ({ ...logScope.current, terminal });
  const fail = (operation: string, error: unknown, terminal?: string, next = "Review the cause in Logs before trying again.") => {
    setRestartRequired(!identity.current); diagnostics.current.error(operation, error, contextFor(terminal), next);
  };
  const input = useRef<ReturnType<typeof terminalInput>>();
  const stopInput = () => { input.current?.dispose(); input.current = undefined; };
  const currentGeneration = useRef(generation); currentGeneration.current = generation;
  const prepareStart = useRef(beforeStart); prepareStart.current = beforeStart;
  const directoryChanged = useRef(onDirectory); directoryChanged.current = onDirectory;
  const commandHandler = useRef(onCommand); commandHandler.current = onCommand;
  const focusWanted = useRef(focused); focusWanted.current = focused;
  const lifetime = useRef<{ disposed: boolean; starting: boolean; autoStarted: boolean; revision: number; id?: string }>({ disposed: false, starting: false, autoStarted: false, revision: 0 });
  // Only the current owner may retire a terminal; old asynchronous replies are inert.
  const unavailable = (current: string, message: string) => {
    if (lifetime.current.disposed || lifetime.current.id !== current) return;
    stopInput();
    identity.current = undefined; lifetime.current.id = undefined;
    setId(undefined); setEnded(true); fail("session access", new TerminalUnavailable(), current, message);
  };
  const closeOwned = async (current: string) => {
    try {
      const result = await engine.terminal<{ problem?: string }>({ action: "close", id: current });
      if (result?.problem) diagnostics.current.error("close", result.problem, contextFor(current), "Inspect remote work before starting another session.");
    }
    catch (error) { if (!(error instanceof TerminalUnavailable)) throw error; }
  };
  const resizeTerminal = async (current: string, cols: number, rows: number) => {
    const owner = lifetime.current;
    try {
      await engine.terminal({ action: "resize", id: current, cols, rows });
      if (!owner.disposed && lifetime.current === owner && owner.id === current) diagnostics.current.connection("resize", undefined, "resize", contextFor(current));
    } catch (error) {
      if (owner.disposed || lifetime.current !== owner || owner.id !== current) return;
      if (error instanceof TerminalUnavailable) unavailable(current, error.message);
      else diagnostics.current.connection("resize", error, "resize", contextFor(current));
    }
  };
  useEffect(() => {
    const owner = { disposed: false, starting: false, autoStarted: false, revision: 0, id: undefined as string | undefined };
    lifetime.current = owner;
    stopInput();
    identity.current = undefined; setId(undefined); setBusy(false); setReview(undefined); setRestartRequired(false); diagnostics.current.reset();
    if (!container.current) return;
    const term = new Terminal({ cursorBlink: true, scrollback: 5000, fontSize: 14, fontFamily: xtermFont(container.current), allowProposedApi: false, theme: xtermTheme(container.current), linkHandler: hyperlinkHandler() });
    const fitting = new FitAddon(); term.loadAddon(fitting); term.open(container.current);
    const releaseRenderer = accelerateTerminal(term);
    const releaseWheel = normalizeTerminalWheel(term);
    const releaseLinks = linkTerminal(term);
    const directory = term.parser.registerOscHandler(7, value => {
      try {
        const url = new URL(value);
        const path = decodeURIComponent(url.pathname);
        if (directoryAllowed.current && url.protocol === "file:" && path.startsWith("/") && path.length <= 512 && !/[\u0000-\u001f]/.test(path)) directoryChanged.current?.(path);
      } catch { /* Non-directory OSC sequences do not change the saved descriptor. */ }
      return true;
    });
    // Terminal content must not drive fitting; only its allocated, visible host size does.
    let lastWidth = 0, lastHeight = 0;
    const fitVisible = (force = false) => {
      if (owner.disposed) return;
      const width = container.current?.clientWidth ?? 0, height = container.current?.clientHeight ?? 0;
      if (!width || !height) { lastWidth = 0; lastHeight = 0; return; }
      if (!force && width === lastWidth && height === lastHeight) return;
      fitting.fit(); lastWidth = width; lastHeight = height;
    };
    emulator.current = term; fit.current = fitVisible;
    const data = term.onData(text => {
      if (!owner.disposed && identity.current) input.current?.push(text);
    });
    const resize = term.onResize(({ cols, rows }) => {
      const current = identity.current;
      if (current) void resizeTerminal(current, cols, rows);
    });
    const observer = new ResizeObserver(() => fitVisible());
    observer.observe(container.current);
    // Include extended Latin when loading: the plain Latin subset alone does not cover Turkish.
    const fonts = document.fonts;
    const loadFont = () => {
      void fonts?.load(`14px ${term.options.fontFamily}`, "ĞğİıŞşÇçÖöÜü").then(() => fitVisible(true)).catch(() => {
        // The family chain still supplies a local fallback if an optional font cannot load.
      });
    };
    const fontLoaded = () => fitVisible(true);
    fonts?.addEventListener("loadingdone", fontLoaded);
    loadFont();
    // Palette and selected font live on the containing Surface. Update the existing emulator,
    // preserving its PTY identity, scrollback and partially typed command.
    const themeWatch = new MutationObserver(() => {
      if (owner.disposed || !container.current) return;
      term.options.theme = xtermTheme(container.current);
      const family = xtermFont(container.current);
      if (family !== term.options.fontFamily) {
        term.options.fontFamily = family;
        fitVisible(true); loadFont();
      }
    });
    const surface = container.current.closest?.(".wes-terminal");
    if (surface) themeWatch.observe(surface, { attributes: true, attributeFilter: ["data-palette", "style"] });
    return () => {
      owner.disposed = true;
      stopInput();
      if (closeOnUnmount && owner.id) void engine.terminal({ action: "close", id: owner.id }).catch(() => {});
      observer.disconnect(); themeWatch.disconnect(); fonts?.removeEventListener("loadingdone", fontLoaded); data.dispose(); resize.dispose(); directory.dispose(); releaseLinks(); releaseWheel(); releaseRenderer(); term.dispose(); emulator.current = undefined; fit.current = undefined; };
  }, [engine, closeOnUnmount, history]);
  // Activation can expose previously hidden geometry. Focus alone never resizes either pane.
  useEffect(() => { if (active) fit.current?.(true); }, [active]);
  useEffect(() => { if (active && focused) emulator.current?.focus(); }, [active, focused]);
  const lastGeneration = useRef(generation);
  useEffect(() => {
    if (!generation) return;
    if (lastGeneration.current && lastGeneration.current !== generation) {
      stopInput();
      identity.current = undefined; lifetime.current.id = undefined; lifetime.current.revision += 1; lifetime.current.starting = false; setId(undefined); setEnded(true); setBusy(false);
      emulator.current?.writeln("\r\n[Workspace changed. Previous terminal access has ended.]");
    }
    lastGeneration.current = generation;
  }, [generation]);
  useEffect(() => {
    if (!id) return;
    const controller = new AbortController();
    const term = emulator.current;
    if (!term) return;
    void terminalOutput({
      poll: (cursor, signal) => engine.terminal<TerminalFrame>({ action: "poll", id, cursor, wait_ms: 1000 }, signal),
      write: bytes => new Promise<void>(resolve => term.write(bytes, resolve)),
      trimmed: () => { term.reset(); term.writeln("[Earlier output was trimmed.]\r\n"); },
      command: async request => {
        // A claim is single-use on the server, even if its response or the later reply is lost.
        const { claimed } = await engine.terminal<{ claimed: boolean }>({ action: "commandclaim", id, request: request.id }, controller.signal);
        if (!claimed || controller.signal.aborted) return;
        let error: string | null = null;
        try {
          if (!commandHandler.current) throw new Error("Pane commands require a workspace terminal pane.");
          await commandHandler.current(request.text, request.environment);
        } catch (failure) { error = failure instanceof Error ? failure.message : "Pane command failed."; }
        await engine.terminal({ action: "commandreply", id, request: request.id, error }, controller.signal);
      },
      editor: async request => {
        const result = await engine.assistantEditor.handle(request);
        await engine.terminal({ action: "editorreply", id, request: request.id, result }, controller.signal);
      },
      problem: message => { if (!controller.signal.aborted) fail("server report", message, id, "Inspect the terminal server diagnostic."); },
      connection: (error, operation = "poll") => { if (!controller.signal.aborted) diagnostics.current.connection("output", error, operation, contextFor(id)); },
      unavailable: message => { if (!controller.signal.aborted) unavailable(id, message); },
      ended: exit => {
        if (controller.signal.aborted || lifetime.current.id !== id) return;
        stopInput();
        setEnded(true); identity.current = undefined;
        term.writeln(`\r\n[Terminal ended${exit === null ? "" : ` · exit ${exit}`}]`);
      },
    }, controller.signal);
    return () => controller.abort();
  }, [engine, id]);
  // A review belongs to one pane lifetime, generation and selection, never to the display alone.
  const visibleReview = review && review.owner === lifetime.current && review.generation === generation && targetKey(review.selected) === targetKey(target) ? review : undefined;
  const start = async (approval?: typeof review) => {
    const owner = lifetime.current;
    if (owner.starting || owner.disposed) return;
    if (approval && (approval !== visibleReview || approval.owner !== owner || approval.generation !== currentGeneration.current || targetKey(approval.selected) !== targetKey(currentTarget.current))) return;
    owner.starting = true;
    const startedGeneration = currentGeneration.current, revision = ++owner.revision;
    const selected = approval?.evidence.target ?? destination;
    const selectedKey = targetKey(selected);
    setBusy(true); setRestartRequired(false); diagnostics.current.reset();
    let operation = "prepare start";
    try {
      if (approval) {
        if (!targetChanged.current) throw new Error("This terminal cannot save a changed destination.");
        operation = "save reviewed target";
        await targetChanged.current(approval.selected, approval.evidence.target);
      }
      if (owner.disposed || lifetime.current !== owner || owner.revision !== revision || currentGeneration.current !== startedGeneration) return;
      await prepareStart.current?.();
      if (owner.disposed || lifetime.current !== owner || owner.revision !== revision || currentGeneration.current !== startedGeneration || targetKey(currentTarget.current) !== selectedKey) return;
      operation = "close";
      if (owner.id) await closeOwned(owner.id);
      if (owner.disposed || lifetime.current !== owner || owner.revision !== revision || currentGeneration.current !== startedGeneration || targetKey(currentTarget.current) !== selectedKey) return;
      // Stop old reads before starting a new process, including a close/expiry race.
      stopInput();
      identity.current = undefined; owner.id = undefined; setId(undefined);
      operation = "start";
      directoryAllowed.current = !selected;
      const result = await engine.terminal<{ review: TerminalReview } | { id: string; cwd?: string; workspace_tools?: boolean; target?: string; destination?: string }>({ action: "start", ...(selected ? { target: selected } : cwd ? { cwd } : {}), ...(history ? { history } : {}) });
      if (owner.disposed || lifetime.current !== owner || owner.revision !== revision || currentGeneration.current !== startedGeneration || targetKey(currentTarget.current) !== selectedKey) {
        if ("id" in result) await engine.terminal({ action: "close", id: result.id });
        return;
      }
      if ("review" in result) {
        if (!selected || result.review.previousRevision !== selected.revision || result.review.target.environment !== selected.environment || result.review.target.target !== selected.target)
          throw new Error("Terminal review does not match the selected destination.");
        setReview({ evidence: result.review, selected, generation: startedGeneration, owner });
        return;
      }
      setReview(undefined);
      owner.id = result.id;
      const current = result.id;
      input.current = terminalInput(
        text => engine.terminal({ action: "write", id: current, text }),
        error => {
          if (owner.disposed || owner.id !== current) return;
          if (error instanceof TerminalUnavailable) { unavailable(current, error.message); return; }
          identity.current = undefined;
          fail("write", error, current, "Input delivery is unconfirmed and was not retried. The command may have run. Check its effects before restarting this terminal.");
        },
      );
      directoryAllowed.current = result.workspace_tools !== false;
      setRunningDestination(selectedKey);
      setRunningIdentity(result.destination);
      setRunningLabel(selected ? `${selected.environment} / ${selected.target}${result.workspace_tools === false ? " · remote shell, no workspace tools" : " · workspace tools"}` : "This computer · workspace tools");
      if (directoryAllowed.current && result.cwd) directoryChanged.current?.(result.cwd);
      emulator.current?.reset(); identity.current = result.id; setId(result.id); setEnded(false);
      fit.current?.(true);
      await resizeTerminal(result.id, emulator.current?.cols ?? 80, emulator.current?.rows ?? 24);
      if (!owner.disposed && focusWanted.current) emulator.current?.focus();
    } catch (e) { if (!owner.disposed && owner.revision === revision) fail(operation, e, owner.id, `Terminal ${operation} was not confirmed. The request was not retried automatically.`); }
    finally { if (owner.revision === revision) { owner.starting = false; if (!owner.disposed) setBusy(false); } }
  };
  useEffect(() => {
    const owner = lifetime.current;
    if (autoStart && (!target || allowTargetStart) && generation && !owner.autoStarted) {
      owner.autoStarted = true;
      void start();
    }
  }, [autoStart, allowTargetStart, generation, history]);
  return <section className="shell-surface" hidden={!active} aria-label="Shell terminal"
    aria-description={id && !ended ? [runningLabel, runningDestination, runningIdentity].filter(Boolean).join(" · ")
      : destination ? `${destination.environment} / ${destination.target} · awaiting start` : undefined}>
    {visibleReview && <TerminalTargetReview review={visibleReview.evidence} busy={busy} onConfirm={() => void start(visibleReview)} onCancel={() => setReview(undefined)} />}
    {!visibleReview && (!id || ended || restartRequired) && <button aria-label="Start terminal" disabled={busy || !generation} onClick={() => void start()}>Start terminal</button>}
    {!visibleReview && !autoStart && !id && <p className="hint">Start a real shell to run programs such as Codex. Workspace tools are available in local shells. Remote shells use their target account; host tools are not forwarded.</p>}
    <div ref={container} className="terminal-emulator" />
  </section>;
}
