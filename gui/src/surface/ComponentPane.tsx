import type { PeekWhat } from "./peek";
import { useMemo, useState } from "react";
import { repeating, type Cell as ClientCell } from "../cells";
import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";
import type { Workspace } from "../workspace";
import type { Theme, CellActions } from "./Cell";
import { connectedCells } from "./component-model";
import { cellBlocks } from "./cell-output";
import { NEXT_VIEW, type ResultArrangement } from "../cells";
import { useInteractiveStates } from "./interactive-state";
import type { Language } from "./language";
import type { ResultRead } from "./results";
import { Session } from "./Session";
import { readSession, type SessionCell, type SessionContext } from "./session-model";
import type { ValueBinding } from "./split-model";

export function ComponentPane({ binding, generation, cells, workspace, engine, context, chrome, tailKeys = true, language, held, reads, retryRead, onOpen, onPeek, onTrouble }: {
  readonly binding: Pick<ValueBinding, "node" | "generation">;
  readonly generation?: string;
  readonly cells: readonly ClientCell[];
  readonly workspace: Workspace;
  readonly engine: Engine;
  readonly context: SessionContext;
  readonly chrome: Theme;
  readonly tailKeys?: boolean;
  readonly language?: Language;
  readonly held: ReadonlyMap<string, StoredValue>;
  readonly reads: ReadonlyMap<string, ResultRead>;
  readonly retryRead: (handle: string) => void;
  readonly onOpen: (node: string, tab: string) => void;
  readonly onPeek?: (node: string, what: PeekWhat) => void;
  readonly onTrouble: (problem: unknown) => void;
}) {
  const [focused, setFocused] = useState<string>();
  const [views, setViews] = useState<ReadonlyMap<string, ResultArrangement>>(new Map());
  const [pinned, setPinned] = useState<ReadonlySet<string>>(new Set());
  const interactiveStates = useInteractiveStates(workspace, generation);
  const valid = generation === binding.generation && workspace.nodes.some(node => node.id === binding.node);
  const selected = useMemo(() => valid ? connectedCells(cells, workspace.nodes, binding.node) : [], [valid, cells, workspace.nodes, binding.node]);
  const arranged = selected.map(cell => ({ ...cell, results: Object.fromEntries(cell.nodes.flatMap(id => views.has(id) ? [[id, views.get(id)!]] : [])), pinned: pinned.has(cell.id) }));
  const model = readSession({ workspace, cells: arranged, context, focused, held, language });
  const nodeOf = (id: string) => selected.find(cell => cell.id === id)?.nodes.at(-1);
  const toggle = (set: ReadonlySet<string>, id: string) => { const next = new Set(set); if (!next.delete(id)) next.add(id); return next; };
  const actions = (cell: SessionCell): CellActions => {
    const original = selected.find(candidate => candidate.id === cell.id)!;
    const node = nodeOf(cell.id);
    const open = (tab: string, target = node) => { if (target) onOpen(target, tab); };
    return {
      repeat: acknowledge => {
        const again = repeating(original, acknowledge);
        void engine.rerun(again.lastRun, again.text, again.originAttempt ?? original.lastRun, acknowledge, undefined, original.document).catch(error => onTrouble(error));
      },
      deleteWork: {
        preview: () => engine.previewDeleteWork(original.lastRun),
        confirm: async (token, additional, protectedContent) => {
          try { await engine.deleteWork(token, additional, protectedContent); }
          catch (error) { onTrouble(error); throw error; }
        },
      },
      open: target => open("result", target), json: target => open("json", target), details: target => open("details", target),
      view: open, openSource: () => open("source"),
      ...(onPeek ? { peek: (what: PeekWhat, target = node) => { if (target) onPeek(target, what); } } : {}),
      pin: () => setPinned(previous => toggle(previous, cell.id)),
      cycle: target => { const id = target ?? node; if (id) setViews(previous => new Map(previous).set(id, { ...previous.get(id), view: NEXT_VIEW[previous.get(id)?.view ?? "preview"] })); },
      setView: (view, target) => { const id = target ?? node; if (id) setViews(previous => new Map(previous).set(id, { ...previous.get(id), view })); },
      setHeight: (rows, target) => { const id = target ?? node; if (id) setViews(previous => new Map(previous).set(id, { view: previous.get(id)?.view ?? "preview", rows })); },
      cancel: () => { for (const id of original.nodes) void engine.cancel(id).catch(error => onTrouble(error)); },
    };
  };
  if (!valid) return <p role="status">This related-work pane is no longer bound to the active workspace. Open a new split for the current node.</p>;
  return <Session history={{ engine, workspace, generation }} chromeMode="pane" model={model} chrome={chrome} tailKeys={tailKeys} prompt={null} onFocus={setFocused} actions={actions}
    output={cell => cellBlocks({ cell, workspace, engine, generation, held, reads, retryRead, interactiveStates })}
    following={false} />;
}
