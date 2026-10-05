/** Client-only session commands. Each session owns its viewport and diagnostic cells. */
import { useCallback, useEffect, useRef, useState } from "react";
import { newCell, type Cell } from "../cells";
import { report } from "../debug";
import type { Engine } from "../engine";
import type { Workspace } from "../workspace";
import type { Settings } from "../settings";
import { persistenceState } from "../desktop-preferences";
import { read } from "./commands";

type ClearRequest = { readonly after?: string; readonly revision: number };
type Storage = Workspace["storage"];
type Report = { storage?: Storage; state: string };
export function useSessionCommands({ engine, workspace, settings, generation, cells, append, onTrouble }: {
  engine: Engine; workspace: Workspace; settings: Settings; generation?: string;
  cells: readonly Cell[]; append: (cell: Cell) => void; onTrouble: (text: string | undefined) => void;
}) {
  const [clearRequest, setClearRequest] = useState<ClearRequest>();
  const [reports, setReports] = useState<ReadonlyMap<string, Report>>(new Map());
  const [home, setHome] = useState<string>();
  const pending = useRef(new Set<string>());
  const epoch = useRef(0);
  useEffect(() => {
    epoch.current++;
    pending.current.clear(); setReports(new Map()); setClearRequest(undefined); setHome(undefined);
    return () => { epoch.current++; };
  }, [generation]);
  useEffect(() => {
    if (!workspace.storage || !pending.current.size) return;
    const answered = new Set(pending.current); pending.current.clear();
    setReports(was => new Map([...was].map(([id, entry]) => [id, answered.has(id)
      ? { storage: workspace.storage, state: "storage received" } : entry])));
  }, [workspace.storage]);
  const answer = useCallback((text: string): boolean => {
    const typed = read(text);
    if (typed.kind !== "clear" && typed.kind !== "debug") return false;
    onTrouble(undefined);
    if (typed.kind === "clear") {
      const after = cells.filter(cell => !cell.pinned).at(-1)?.id;
      setClearRequest(was => ({ after, revision: (was?.revision ?? 0) + 1 }));
      return true;
    }
    const cell = { ...newCell("/debug"), state: "answered" as const };
    pending.current.add(cell.id);
    setReports(was => new Map(was).set(cell.id, { state: "waiting for current engine storage" }));
    append(cell);
    const askedEpoch = epoch.current;
    engine.storage().catch((error: Error) => {
      if (epoch.current !== askedEpoch) return;
      pending.current.delete(cell.id);
      setReports(was => new Map(was).set(cell.id, { state: `storage unavailable: ${error.message}` }));
    });
    if (typeof window !== "undefined" && window.__WES_DESKTOP__) {
      void fetch("/data-home", { cache: "no-store" }).then(async response => {
        if (!response.ok) throw new Error("data folder unavailable");
        const data: unknown = await response.json();
        if (epoch.current === askedEpoch && data && typeof data === "object" && "path" in data && typeof data.path === "string") setHome(data.path);
      }).catch(() => { if (epoch.current === askedEpoch) setHome(undefined); });
    }
    return true;
  }, [engine, cells, append, onTrouble]);
  const reportFor = (id: string): string | undefined => {
    const entry = reports.get(id);
    if (!entry) return undefined;
    const desktop = typeof window !== "undefined" && window.__WES_DESKTOP__ === true;
    const persistence = persistenceState();
    const facts: [string, string][] = [
      ["status", entry.state],
      ["cells", `${cells.length} in this session pane`],
      ["nodes", String(workspace.nodes.length)],
      ["providers", workspace.catalogue.providers.map(provider => provider.name).join(", ") || "none"],
      ["palette", settings.surfacePalette],
      ["settings", desktop ? (home ? `${home.replace(/\/$/, "")}/desktop-ui.json` : "desktop-ui.json in the selected data folder (path unavailable)") : "localStorage wes.settings"],
      ["holds", "palette, font, layout, personal aliases and client preferences"],
      ["arrangement", "pin/collapse state and clear position: memory in this window; not saved"],
    ];
    if (desktop) facts.push(["preferences", persistence.error ?? (persistence.saving ? "saving" : "no pending write")]);
    return report(entry.storage, facts, { name: desktop ? "desktop app" : "this browser", places: [], note: "settings location shown above; client storage sizes are not measured" });
  };
  return { answer, clearRequest, reportFor };
}
